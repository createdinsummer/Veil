//! 统一的 JSONC 读写：支持注释与尾逗号，更新文本文件时保留未删除字段的注释。
//!
//! 所属模块：核心配置序列化层，供配置文件在保持人工注释的同时读写结构化数据。
//! 核心函数：from_str 读取 JSONC；save 原子保存；update / update_node 按语法树同步字段；input_value 将 JSON 值转为解析器的可插入节点。
//! 依赖能力：jsonc-parser 的具体语法树、Serde 模型转换、serde_json 值表示和原子文件写入。
//! 整体目标：支持标准 JSON 加注释和尾逗号，保存时尽量保留已有字段旁的注释与排版；遇到无效文件时不覆盖用户原文。

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

/// 功能：把带人工注释的配置文本还原为调用方需要的业务模型。
/// 入参：content: &str 为完整 JSONC；T: DeserializeOwned 指定目标模型，允许行/块注释和尾逗号。
/// 返回值：Ok(T) 为按目标类型反序列化后的值；不提供目标类型之外的业务校验。
/// 异常：语法、必需字段、字段类型或自定义反序列化检查失败时返回 SerializationError。
/// 注意事项：OPTIONS 明确拒绝单引号、未加引号键名、十六进制等 JSON5 扩展；模型是否忽略未知字段由 T 决定。
pub fn from_str<T: DeserializeOwned>(content: &str) -> Result<T, VeilError> {
    jsonc_parser::parse_to_serde_value(content, &OPTIONS)
        .map_err(|error| VeilError::SerializationError(format!("解析 JSONC 失败: {error}")))
}

/// 功能：将模型转成便于后续加密或传输的紧凑文本字节。
/// 入参：value: &T 为实现 Serialize 的业务对象，其字段含义由 T 的定义决定。
/// 返回值：Ok(`Vec<u8>`) 为 UTF-8 标准 JSON 字节；不带注释，标准 JSON 同时也是合法 JSONC。
/// 异常：模型无法表示为 JSON 时返回 SerializationError。
/// 注意事项：不加密、不脱敏、不写文件；可能包含明文密钥等敏感字段，输出缓冲由调用方负责管理。
pub(crate) fn to_vec<T: Serialize>(value: &T) -> Result<Vec<u8>, VeilError> {
    serde_json::to_vec(value)
        .map_err(|error| VeilError::SerializationError(format!("序列化 JSONC 失败: {error}")))
}

/// 功能：用新配置模型更新文本文件，尽量保留原有字段旁的注释，并完整替换最终文件。
/// 入参：path: &Path 为目标配置路径；value: &T 为实现 Serialize 的最终模型，模型缺少的旧字段会被移除。
/// 返回值：Ok(()) 表示原子写入入口成功；目标原先不存在时生成带文件说明的格式化 JSONC。
/// 异常：读取失败传播 I/O 错误；原文无法解析或模型无法序列化返回 SerializationError；写入/替换失败传播 I/O 错误。
/// 注意事项：无效原文不会被覆盖；此方法不加业务锁，多个写者应由上层协调，避免读旧值后互相覆盖。
/// 最终发布后的目录同步错误由通用 atomic_write 记告警，成功返回不等于所有断电持久性都已确认。
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

/// 功能：将最终配置同步到旧文本的可编辑语法树，在不整篇重新排版的情况下生成新文档。
/// 入参：previous: &str 为原 JSONC 文本；value: &T 为序列化后的目标配置模型。
/// 返回值：Ok(String) 为更新后的完整文本，包括仍附着在保留节点上的注释。
/// 异常：旧文档无法解析、文档没有根值或新模型不能转为 JSON 值时返回 SerializationError。
/// 注意事项：只改内存文档，不持久化文件；数组按索引匹配、对象按名称匹配，删除/替换节点不保证保留其原注释。
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

/// 功能：递归把一个配置节点同步为目标值，保留仍可复用的字段位置和注释。
/// 入参：node: CstNode 为已成功解析的旧值节点；value: &serde_json::Value 为此位置的最终值。
/// 返回值：无；通过可编辑节点句柄原地修改它所属的语法树。
/// 异常：不返回 Result；进入非预期的非值节点时 unreachable! 会 panic，递归深度也受线程栈限制。
/// 注意事项：对象按字段名保留首次匹配并删除重复/过时字段，新增字段追加到末尾；数组按位置而不是业务 ID 对齐。
/// 调用者应只传入解析成功的值节点，不得在递归过程中并发修改同一语法树。
fn update_node(node: CstNode, value: &Value) {
    if let (Some(object), Value::Object(values)) = (node.as_object(), value) {
        // 记录仍被新模型保留的旧字段；遍历旧字段是为了保住它们原有的位置与注释。
        // seen 保存已经成功匹配的旧字段名：既用于删重复键，也用于判断新模型哪些字段尚需追加。
        let mut seen = HashSet::new();
        for property in object.properties() {
            let Some(name) = property.decoded_name() else {
                continue;
            };
            match values.get(&name) {
                // 首次遇到且仍存在的字段递归更新；重复键或新模型已删除的字段会走删除分支。
                Some(value) if seen.insert(name) => {
                    if let Some(node) = property.value() {
                        update_node(node, value);
                    }
                }
                _ => property.remove(),
            }
        }
        // 新模型中尚未出现在旧文档的字段追加到对象末尾。
        for (name, value) in values {
            if !seen.contains(name) {
                object.append(name, input_value(value));
            }
        }
    } else if let (Some(array), Value::Array(values)) = (node.as_array(), value) {
        // 数组元素按索引对应，依次更新共有位置，再移除尾部多余项或追加新项。
        // elements 是更新前的数组节点快照；每一项对应一个位置，不是按对象中的业务主键匹配。
        let elements = array.elements();
        for (node, value) in elements.iter().zip(values) {
            update_node(node.clone(), value);
        }
        // 从尾部向前删除新配置不再需要的位置，避免删除较早元素影响后续位置的处理。
        for node in elements.iter().skip(values.len()).rev() {
            node.clone().remove();
        }
        // 新列表比旧列表更长时，只为新增后缀创建节点，原有公共位置已经递归更新。
        for value in values.iter().skip(elements.len()) {
            array.append(input_value(value));
        }
    } else if node.to_serde_value().as_ref() != Some(value) {
        // 标量值变化时替换对应节点；数值写入由 input_value 保持十进制精度。
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

/// 功能：把 JSON 业务值转成语法树可插入的节点材料，供新增字段或替换值使用。
/// 入参：value: &serde_json::Value，可为 null、布尔、数字、字符串、数组或对象。
/// 返回值：CstInputValue，拥有插入所需的文本或子节点；数组/对象递归转换并保持其迭代顺序。
/// 异常：不以 Result 返回错误；特别深的嵌套仍受线程栈和内存资源限制。
/// 注意事项：数字直接使用 serde_json 的文本表示，不先转浮点数，避免大整数经额外浮点转换失真；不承诺保留旧数字的写法。
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
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn save_leaves_invalid_file_untouched() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.jsonc");
        let original = "{\"user_settings\": ";
        fs::write(&path, original).unwrap();
        assert!(save(&path, &json!({"user_settings": {}})).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }
}
