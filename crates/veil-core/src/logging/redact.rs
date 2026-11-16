//! 仅在异常路径处理文本；规则只编译一次，输入和输出均有长度上限。
//!
//! # 业务定位
//!
//! 日志脱敏模块：日志里的错误文本可能携带敏感信息（路径、密码、密钥材料、
//! 文件名），本模块在异常路径（打日志前）统一处理，把敏感内容替换成
//! `[REDACTED]`、`[PATH:xxx]` 等占位符后再输出。设计要点：
//! - 主要用于日志文本处理；正则规则只编译一次（OnceLock）。
//! - 输入先截断 4096 字符、输出截断 768 字符，避免大错误对象拖慢日志。
//! - 登记的敏感值用 Zeroizing 保存，只有其列表的最后一个共享引用释放后才清零。
//!
//! # 两层脱敏
//!
//! 1. 上下文敏感值：调用方用 `remember`/`scope` 登记的已知敏感串
//!    （如本次操作的密码、路径），先做精确替换。
//! 2. 通用规则：键值对（password=…）、引号内容、路径、文件名、长十六进制
//!    密钥材料等模式化匹配替换；路径/文件名保留 12 位 BLAKE3 哈希指纹，
//!    便于日志关联又不暴露原文。
use regex::Regex;
use std::{
    cell::RefCell,
    sync::{Arc, OnceLock},
};
use zeroize::Zeroizing;

// 线程本地敏感值表：记录"当前线程这次操作里出现过的敏感串"，
// 供 sanitize 精确替换；跨线程不共享（每个工作线程有自己的上下文）。
thread_local! {
    static SECRETS: RefCell<Arc<Vec<Zeroizing<String>>>> = RefCell::new(Arc::new(Vec::new()));
}

/// 功能：为任务跨线程或作用域切换保留当前线程的脱敏上下文。
/// 入参：无，从当前线程 SECRETS 读取已登记敏感值。
/// 返回值：`Arc<Vec<Zeroizing<String>>>`，共享同一份列表，不深拷贝每条字符串。
/// 异常：不返回 Result；线程局部存储访问失败或 RefCell 借用冲突可能 panic。
/// 注意事项：持有返回值会延长这份敏感表及其材料的生命周期；本函数不将上下文自动绑定到别的线程。
pub(super) fn capture() -> Arc<Vec<Zeroizing<String>>> {
    SECRETS.with(|values| values.borrow().clone())
}

/// 敏感值作用域守卫：保存此前的线程表，离开时恢复；scope 追加登记，attach 整体替换。
///
/// 业务定位：RAII（资源获取即初始化）守卫，在作用域结束后恢复此前的上下文。
/// 敏感值由 Arc 共享；仅当最后一个持有者释放后，Zeroizing 才会清除其副本。
pub struct SensitiveScope(Arc<Vec<Zeroizing<String>>>);
impl Drop for SensitiveScope {
    /// 功能：恢复进入脱敏作用域前的敏感值表，避免后续请求沿用当前上下文。
    /// 入参：self 为保存上一份敏感表的 SensitiveScope。
    /// 返回值：无；当前线程表与守卫中的旧表交换。
    /// 异常：不返回错误；线程本地存储访问或 RefCell 借用冲突可能 panic。
    /// 注意事项：被换出的表要等最后一个 Arc 引用释放才清零；不承诺立即清理其他线程的副本。
    fn drop(&mut self) {
        // 恢复进入作用域前的表；被换出的表会在最后一个 Arc 引用释放时清理。
        SECRETS.with(|values| std::mem::swap(&mut *values.borrow_mut(), &mut self.0));
    }
}

/// 功能：在本次业务调用期间追加需要隐藏的敏感文本，结束后恢复进入前的脱敏表。
/// 入参：values: &[&str] 为敏感串列表，每项可为本次密码、路径等原文；空串、超长串和重复值按 remember 规则处理。
/// 返回值：SensitiveScope 守卫，保存原表；析构时恢复本线程先前上下文。
/// 异常：不以 Result 返回错误；线程局部存储或 RefCell 借用冲突可能 panic。
/// 注意事项：这是在旧表基础上追加，不是仅用新表替换旧上下文；最终仍受 16 个值上限约束。
/// 守卫应按嵌套顺序在绑定线程释放；不承诺恢复时立即清零仍被其他 Arc 持有的列表。
pub fn scope(values: &[&str]) -> SensitiveScope {
    // 先记住"进入前"的列表，作为离开时的恢复目标。
    let previous = capture();
    // 逐条登记新敏感值（去重、限长、限数，见 remember）。
    for value in values {
        remember(value);
    }
    SensitiveScope(previous)
}

/// 功能：把已捕获的脱敏表绑定到当前执行线程，使迁移后的任务沿用原敏感值。
/// 入参：values: `Arc<Vec<Zeroizing<String>>>` 为可信执行上下文捕获的完整列表，取得一个共享引用的所有权。
/// 返回值：SensitiveScope，保存被替换的原线程表，析构时恢复。
/// 异常：不返回 Result；线程局部存储访问或 RefCell 借用冲突可能 panic。
/// 注意事项：直接整体替换，不逐项验证长度、数量或去重；调用方应传入本模块产生的上下文并按嵌套顺序恢复。
pub(super) fn attach(values: Arc<Vec<Zeroizing<String>>>) -> SensitiveScope {
    SensitiveScope(SECRETS.with(|current| current.replace(values)))
}

/// 功能：登记当前操作涉及的一条敏感原文，供后续日志清洗时定位并遮盖。
/// 入参：value: &str 为待隐藏的原始文本；空串或 UTF-8 字节数超过 4096 时忽略。
/// 返回值：无；重复值不追加，正常列表已有 16 项时移除最早一项，再保存当前值的受清零保护副本。
/// 异常：不返回 Result；线程局部存储或 RefCell 借用冲突可能 panic。
/// 注意事项：只登记当前线程；被共享的表通过复制后再修改，旧 Arc 可能继续持有材料。
/// 登记值可能一直保留到上下文恢复、淘汰或线程结束，不能笼统称为“调用结束就立即清零”。
pub fn remember(value: &str) {
    // 空串与超长值直接忽略。
    if value.is_empty() || value.len() > 4096 {
        return;
    }
    SECRETS.with(|values| {
        let mut values = values.borrow_mut();
        // 已登记过的值不重复添加。
        if values.iter().any(|v| v.as_str() == value) {
            return;
        }
        // Arc 被其他上下文共享时先复制一份，再修改；独占时直接修改原列表。
        let values = Arc::make_mut(&mut *values);
        // 达到 16 个上限时丢弃最早登记的（队列式淘汰）。
        if values.len() == 16 {
            values.remove(0);
        }
        // Zeroizing 仅在这一份字符串实际析构时清零；共享上下文或复制出的旧表可能仍持有其他副本。
        values.push(Zeroizing::new(value.to_owned()));
    });
}

/// 功能：为异常日志清洗文本，遮盖上下文敏感值以及常见密码赋值、引号、路径、文件名和密钥模式。
/// 入参：input: &str 为原始错误或字段文本；只处理前 4096 个 Unicode 字符。
/// 返回值：String，先按当前线程敏感表和规则清洗，再去掉大多数控制字符，最终最多 768 个字符。
/// 异常：没有 Result 返回；首次编译内置正则若失败会 unwrap panic，线程上下文借用冲突也可能 panic。
/// 注意事项：4 字符以上的登记值做子串替换，1～3 字符还要求前后不为字母数字；规则有意不覆盖所有可能的秘密。
/// 路径/文件名保留短哈希便于关联，不是不可猜测的匿名身份；调用方仍应优先提供固定文案和分类字段。
pub fn sanitize(input: &str) -> String {
    // 规则集只初始化一次：编译正则开销大，复用提升异常路径性能。
    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let rules = RULES.get_or_init(|| vec![
        // 规则1：键值对赋值（password/token/密钥等键 + 分隔符 + 值），
        // 把值整段替换为 [REDACTED]，键名保留。
        (Regex::new(r#"(?i)(password|passwd|pwd|token|authorization|secret|api[_-]?key|key[_-]?material|密码|密钥|口令|凭证)\s*[:=：]\s*(?:"(?:\\.|[^"\\])*"|'[^']*'|[^\s,;，；]+)"#).unwrap(), "$1=[REDACTED]"),
        // 规则2：任何引号包裹的内容整体替换为 [QUOTED]（可能是密码/命令）。
        (Regex::new(r#""(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|`[^`]*`"#).unwrap(), "[QUOTED]"),
        // 规则3：Windows 盘符路径（C:\...）或 UNC 路径（\\...）。
        (Regex::new(r#"(?i)[a-z]:[\\/][^\s,;，；)\]}]+|\\\\[^\s,;，；)\]}]+"#).unwrap(), "[PATH]"),
        // 规则4：行首或分隔符后的 / 开头绝对路径（含 file:// 前缀）。
        (Regex::new(r#"(^|[\s(:=])(?:file://)?/[^\s,;，；)\]}]+"#).unwrap(), "${1}[PATH]"),
        // 规则5：形如 name.ext 的文件名（含目录分隔符以外的字符组合）。
        (Regex::new(r#"[^\s/:;,，；()\[\]{}"'=<>]+\.[\pL][\pL\pN_-]{0,15}\b"#).unwrap(), "[FILE]"),
        // 规则6：48 位以上连续十六进制（密钥材料/哈希）替换为 [KEY]。
        (Regex::new(r"\b[0-9a-fA-F]{48,}\b").unwrap(), "[KEY]"),
    ]);
    // 先截断输入，避免错误对象携带大块文件内容时增加异常路径开销。
    let mut text: String = input.chars().take(4096).collect();
    // 第一层：从最新登记的值往旧值遍历；每一项代表当前线程已知的一段敏感原文，
    // 依长度使用子串或单词边界替换。重叠值按这个顺序处理，不能当作完整语义识别。
    SECRETS.with(|values| {
        for value in values.borrow().iter().rev() {
            if value.chars().count() >= 4 {
                // 4 字符以上：直接全局替换，误伤子串风险低。
                text = text.replace(value.as_str(), "[REDACTED]");
            } else {
                // 短敏感值（1~3 字符）：必须检查边界——前后都不是字母数字
                // 才算独立出现，避免把 "a" 替换进每个单词。
                let mut output = String::new();
                let mut end = 0;
                // 逐次找到敏感值出现的位置。
                for (start, matched) in text.match_indices(value.as_str()) {
                    let next = start + matched.len();
                    // 前一个字符和后一个字符都不能是字母/数字（单词边界）。
                    if !text[..start]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_alphanumeric)
                        && !text[next..]
                            .chars()
                            .next()
                            .is_some_and(char::is_alphanumeric)
                    {
                        // 保留中间未匹配部分，把该处替换为 [REDACTED]。
                        output.push_str(&text[end..start]);
                        output.push_str("[REDACTED]");
                        end = next;
                    }
                }
                // 收尾：追加最后一段未处理文本。
                output.push_str(&text[end..]);
                text = output;
            }
        }
    });
    // 第二层：通用规则替换。
    for (regex, replacement) in rules {
        // 路径/文件名规则使用带指纹的替换函数（保留哈希便于日志关联）。
        if *replacement == "[PATH]" || *replacement == "${1}[PATH]" || *replacement == "[FILE]" {
            text = regex
                .replace_all(&text, |captures: &regex::Captures<'_>| {
                    let full = captures.get(0).unwrap().as_str();
                    // 文件名规则的白名单：数据库内部对象名/日志目标前缀不脱敏，
                    // 避免把 "file_info.xxx"、"sqlite_xxx" 等系统名改坏。
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
                    // 规则4 捕获了前导分隔符（组1），替换时保留它。
                    let prefix = captures.get(1).map_or("", |m| m.as_str());
                    let value = full.strip_prefix(prefix).unwrap_or(full);
                    // FILE/PATH 两类占位符共用这套带指纹逻辑。
                    let kind = if *replacement == "[FILE]" {
                        "FILE"
                    } else {
                        "PATH"
                    };
                    // 占位符带上 12 位 BLAKE3 指纹：同一条路径多次出现时
                    // 指纹一致，便于日志关联，又不暴露原文。
                    format!(
                        "{prefix}[{kind}:{}]",
                        &blake3::hash(value.as_bytes()).to_hex()[..12]
                    )
                })
                .into_owned();
        } else {
            // 其他规则（键值对、引号、密钥）直接整体替换。
            text = regex.replace_all(&text, *replacement).into_owned();
        }
    }
    // 最后过滤控制字符（保留换行与制表符），并截断到 768 字符作为输出上限。
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(768)
        .collect()
}
