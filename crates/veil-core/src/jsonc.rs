//! 统一的 JSONC 读写：支持注释与尾逗号，更新文本文件时保留未删除字段的注释。

use crate::error::VeilError;
use jsonc_parser::ParseOptions;
use jsonc_parser::cst::{CstContainerNode, CstInputValue, CstLeafNode, CstNode, CstRootNode};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

/// 只在标准 JSON 上开放注释与尾逗号，避免隐式接受 JSON5 或遗漏逗号的输入。
const OPTIONS: ParseOptions = ParseOptions {
    allow_comments: true,
    allow_trailing_commas: true,
    allow_loose_object_property_names: false,
    allow_missing_commas: false,
    allow_single_quoted_strings: false,
    allow_hexadecimal_numbers: false,
    allow_unary_plus_numbers: false,
    allow_bare_decimal_point_numbers: false,
    allow_non_finite_numbers: false,
    allow_extended_string_escapes: false,
};

/// 从 JSONC 文本解析指定模型。
///
/// # 错误
/// 语法无效、必需字段缺失或字段类型不符时返回序列化错误。
pub fn from_str<T: DeserializeOwned>(content: &str) -> Result<T, VeilError> {
    jsonc_parser::parse_to_serde_value(content, &OPTIONS)
        .map_err(|error| VeilError::SerializationError(format!("解析 JSONC 失败: {error}")))
}

/// 生成紧凑的 JSONC 字节，供元数据加密使用；标准 JSON 是 JSONC 的子集。
pub(crate) fn to_vec<T: Serialize>(value: &T) -> Result<Vec<u8>, VeilError> {
    serde_json::to_vec(value)
        .map_err(|error| VeilError::SerializationError(format!("序列化 JSONC 失败: {error}")))
}

/// 更新 JSONC 文件，并通过已有的原子替换流程提交。
///
/// 已有文件按语法树逐字段更新，保留已有字段的注释与排版；无效文件不会被覆盖。
pub fn save<T: Serialize>(path: &Path, value: &T) -> Result<(), VeilError> {
    let content = match fs::read_to_string(path) {
        Ok(previous) => update(&previous, value)?,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let body = serde_json::to_string_pretty(value).map_err(|error| {
                VeilError::SerializationError(format!("序列化 JSONC 失败: {error}"))
            })?;
            format!("// Veil JSONC：支持行注释、块注释和尾逗号。\n{body}\n")
        }
        Err(error) => return Err(error.into()),
    };
    crate::fsutil::atomic_write(path, content.as_bytes())?;
    Ok(())
}

/// 将模型的最终值应用到已有文本，避免整体重新序列化时丢失注释。
fn update<T: Serialize>(previous: &str, value: &T) -> Result<String, VeilError> {
    let root = CstRootNode::parse(previous, &OPTIONS)
        .map_err(|error| VeilError::SerializationError(format!("解析 JSONC 失败: {error}")))?;
    let node = root
        .value()
        .ok_or_else(|| VeilError::SerializationError("JSONC 文档不能为空".to_string()))?;
    let value = serde_json::to_value(value)
        .map_err(|error| VeilError::SerializationError(format!("序列化 JSONC 失败: {error}")))?;
    update_node(node, &value);
    Ok(root.to_string())
}

/// 对象按键更新，数组按位置更新；注释仍附着在原来的字段或数组元素旁。
fn update_node(node: CstNode, value: &Value) {
    if let (Some(object), Value::Object(values)) = (node.as_object(), value) {
        let mut seen = HashSet::new();
        for property in object.properties() {
            let Some(name) = property.decoded_name() else {
                continue;
            };
            match values.get(&name) {
                Some(value) if seen.insert(name) => {
                    if let Some(node) = property.value() {
                        update_node(node, value);
                    }
                }
                _ => property.remove(),
            }
        }
        for (name, value) in values {
            if !seen.contains(name) {
                object.append(name, input_value(value));
            }
        }
    } else if let (Some(array), Value::Array(values)) = (node.as_array(), value) {
        let elements = array.elements();
        for (node, value) in elements.iter().zip(values) {
            update_node(node.clone(), value);
        }
        for node in elements.iter().skip(values.len()).rev() {
            node.clone().remove();
        }
        for value in values.iter().skip(elements.len()) {
            array.append(input_value(value));
        }
    } else if node.to_serde_value().as_ref() != Some(value) {
        let replacement = input_value(value);
        match node {
            CstNode::Container(CstContainerNode::Object(node)) => {
                node.replace_with(replacement);
            }
            CstNode::Container(CstContainerNode::Array(node)) => {
                node.replace_with(replacement);
            }
            CstNode::Leaf(CstLeafNode::BooleanLit(node)) => {
                node.replace_with(replacement);
            }
            CstNode::Leaf(CstLeafNode::NullKeyword(node)) => {
                node.replace_with(replacement);
            }
            CstNode::Leaf(CstLeafNode::NumberLit(node)) => {
                node.replace_with(replacement);
            }
            CstNode::Leaf(CstLeafNode::StringLit(node)) => {
                node.replace_with(replacement);
            }
            _ => unreachable!("只更新解析成功的 JSONC 值节点"),
        }
    }
}

/// 转成 CST 的插入值，数字以原始十进制文本传递，避免经过浮点数损失精度。
fn input_value(value: &Value) -> CstInputValue {
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(value) => CstInputValue::Bool(*value),
        Value::Number(value) => CstInputValue::Number(value.to_string()),
        Value::String(value) => CstInputValue::String(value.clone()),
        Value::Array(values) => CstInputValue::Array(values.iter().map(input_value).collect()),
        Value::Object(values) => CstInputValue::Object(
            values
                .iter()
                .map(|(name, value)| (name.clone(), input_value(value)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 注释和尾逗号可被读取，字符串内部的注释符号不受影响。
    #[test]
    fn parses_comments_and_trailing_commas() {
        let value: Value = from_str(
            r#"// 文档说明
            {
                /* 地址中的 // 是字符串内容 */
                "url": "https://example.com/a/*b*/",
                "values": [1, 2,],
            }"#,
        )
        .unwrap();
        assert_eq!(value["url"], "https://example.com/a/*b*/");
        assert_eq!(value["values"], json!([1, 2]));
    }

    /// JSON5 扩展与损坏输入不会因解析器的宽松默认值而被接受。
    #[test]
    fn rejects_non_jsonc_syntax() {
        for content in [
            "{unquoted: 1}",
            "{\"key\": 'single'}",
            "{\"value\": 0x10}",
            "{\"value\": +1}",
            "{\"value\": .5}",
            "{\"a\": 1 \"b\": 2}",
            "{\"value\": 1} {}",
            "/* 未闭合注释",
        ] {
            assert!(from_str::<Value>(content).is_err(), "错误接受: {content}");
        }
    }

    /// 深层字段修改及数组追加保留注释，大整数不会丢失精度。
    #[test]
    fn updates_preserve_comments_and_integer_precision() {
        let previous = r#"// 配置说明
{
  "user_settings": {
    // 提示说明
    "hint_level": "full", /* 行尾说明 */
  },
  "links": [
    // 第一个链接
    {"path": "one", "size": 1},
  ],
}
"#;
        let expected = json!({
            "user_settings": {"hint_level": "off"},
            "links": [{"path": "one", "size": u64::MAX}, {"path": "two", "size": 2}],
        });
        let updated = update(previous, &expected).unwrap();
        for comment in [
            "// 配置说明",
            "// 提示说明",
            "/* 行尾说明 */",
            "// 第一个链接",
        ] {
            assert!(updated.contains(comment));
        }
        assert_eq!(from_str::<Value>(&updated).unwrap(), expected);
        assert_eq!(update(&updated, &expected).unwrap(), updated);
    }

    /// 删除字段和数组元素后仍能解析，保留字段的注释不受影响。
    #[test]
    fn removes_obsolete_values_without_corrupting_document() {
        let previous = r#"{
  // 保留字段
  "keep": [1, /* 多余元素 */ 2, 3,],
  "remove": {"old": true},
}"#;
        let expected = json!({"keep": [false], "new": null});
        let updated = update(previous, &expected).unwrap();
        assert!(updated.contains("// 保留字段"));
        assert_eq!(from_str::<Value>(&updated).unwrap(), expected);
    }

    /// 保存失败时不会覆盖损坏的原文件，也不会静默重置为默认配置。
    #[test]
    fn save_leaves_invalid_file_untouched() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.jsonc");
        let original = "{\"user_settings\": ";
        fs::write(&path, original).unwrap();
        assert!(save(&path, &json!({"user_settings": {}})).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }
}
