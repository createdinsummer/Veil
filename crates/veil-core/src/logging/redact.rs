//! 仅在异常路径处理文本；规则只编译一次，输入和输出均有长度上限。
use regex::Regex;
use std::{
    cell::RefCell,
    sync::{Arc, OnceLock},
};
use zeroize::Zeroizing;

thread_local! {
    static SECRETS: RefCell<Arc<Vec<Zeroizing<String>>>> = RefCell::new(Arc::new(Vec::new()));
}

pub(super) fn capture() -> Arc<Vec<Zeroizing<String>>> {
    SECRETS.with(|values| values.borrow().clone())
}

pub struct SensitiveScope(Arc<Vec<Zeroizing<String>>>);
impl Drop for SensitiveScope {
    fn drop(&mut self) {
        SECRETS.with(|values| std::mem::swap(&mut *values.borrow_mut(), &mut self.0));
    }
}

pub fn scope(values: &[&str]) -> SensitiveScope {
    let previous = capture();
    for value in values {
        remember(value);
    }
    SensitiveScope(previous)
}

pub(super) fn attach(values: Arc<Vec<Zeroizing<String>>>) -> SensitiveScope {
    SensitiveScope(SECRETS.with(|current| current.replace(values)))
}

/// 密码只在内存中短暂参与脱敏；每个上下文最多保留 16 个值，每个值至多 4 KiB。
pub fn remember(value: &str) {
    if value.is_empty() || value.len() > 4096 {
        return;
    }
    SECRETS.with(|values| {
        let mut values = values.borrow_mut();
        if values.iter().any(|v| v.as_str() == value) {
            return;
        }
        let values = Arc::make_mut(&mut *values);
        if values.len() == 16 {
            values.remove(0);
        }
        values.push(Zeroizing::new(value.to_owned()));
    });
}

pub fn sanitize(input: &str) -> String {
    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let rules = RULES.get_or_init(|| vec![
        (Regex::new(r#"(?i)(password|passwd|pwd|token|authorization|secret|api[_-]?key|key[_-]?material|密码|密钥|口令|凭证)\s*[:=：]\s*(?:"(?:\\.|[^"\\])*"|'[^']*'|[^\s,;，；]+)"#).unwrap(), "$1=[REDACTED]"),
        (Regex::new(r#""(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|`[^`]*`"#).unwrap(), "[QUOTED]"),
        (Regex::new(r#"(?i)[a-z]:[\\/][^\s,;，；)\]}]+|\\\\[^\s,;，；)\]}]+"#).unwrap(), "[PATH]"),
        (Regex::new(r#"(^|[\s(:=])(?:file://)?/[^\s,;，；)\]}]+"#).unwrap(), "${1}[PATH]"),
        (Regex::new(r#"[^\s/:;,，；()\[\]{}"'=<>]+\.[\pL][\pL\pN_-]{0,15}\b"#).unwrap(), "[FILE]"),
        (Regex::new(r"\b[0-9a-fA-F]{48,}\b").unwrap(), "[KEY]"),
    ]);
    // 先截断输入，避免错误对象携带大块文件内容时增加异常路径开销。
    let mut text: String = input.chars().take(4096).collect();
    SECRETS.with(|values| {
        for value in values.borrow().iter().rev() {
            if value.chars().count() >= 4 {
                text = text.replace(value.as_str(), "[REDACTED]");
            } else {
                let mut output = String::new();
                let mut end = 0;
                for (start, matched) in text.match_indices(value.as_str()) {
                    let next = start + matched.len();
                    if !text[..start]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_alphanumeric)
                        && !text[next..]
                            .chars()
                            .next()
                            .is_some_and(char::is_alphanumeric)
                    {
                        output.push_str(&text[end..start]);
                        output.push_str("[REDACTED]");
                        end = next;
                    }
                }
                output.push_str(&text[end..]);
                text = output;
            }
        }
    });
    for (regex, replacement) in rules {
        if *replacement == "[PATH]" || *replacement == "${1}[PATH]" || *replacement == "[FILE]" {
            text = regex
                .replace_all(&text, |captures: &regex::Captures<'_>| {
                    let full = captures.get(0).unwrap().as_str();
                    if *replacement == "[FILE]"
                        && [
                            "operation_locks.",
                            "file_info.",
                            "space_info.",
                            "container_info.",
                            "main.",
                            "sqlite_",
                        ]
                        .iter()
                        .any(|p| full.starts_with(p))
                    {
                        return full.to_owned();
                    }
                    let prefix = captures.get(1).map_or("", |m| m.as_str());
                    let value = full.strip_prefix(prefix).unwrap_or(full);
                    let kind = if *replacement == "[FILE]" {
                        "FILE"
                    } else {
                        "PATH"
                    };
                    format!(
                        "{prefix}[{kind}:{}]",
                        &blake3::hash(value.as_bytes()).to_hex()[..12]
                    )
                })
                .into_owned();
        } else {
            text = regex.replace_all(&text, *replacement).into_owned();
        }
    }
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(768)
        .collect()
}
