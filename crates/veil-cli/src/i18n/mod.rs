//! 编译期内嵌的轻量国际化资源。
//!
//! `messages.json` 在编译时嵌入二进制，启动后按 `VEIL_LANG` 选择语言。模块保持一个
//! 进程级只读消息表和语言代码，翻译查询不执行 I/O，占位符按名称替换。

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

/// 单种语言下“消息键 → 文本”的映射。
type LangMap = HashMap<String, String>;
/// 所有语言的消息表。
type MsgStore = HashMap<String, LangMap>;

/// 启动时初始化一次的完整消息表。
static STORE: OnceLock<MsgStore> = OnceLock::new();
/// 启动时解析一次的当前语言代码。
static LANG: OnceLock<String> = OnceLock::new();

/// 启动时调用一次。从 `VEIL_LANG` 环境变量读取语言（默认 `zh`），
/// 并加载编译时嵌入的 `messages.json`。
///
/// # Panics
/// 内嵌的 `messages.json` 不是合法 JSON 时 panic，因为这属于构建产物错误。
pub fn init() {
    let store: MsgStore =
        serde_json::from_str(include_str!("messages.json")).expect("i18n/messages.json 格式错误");
    // OnceLock 只接受第一次写入，重复调用 init 时保留已有消息表。
    STORE.set(store).ok();

    let lang = std::env::var("VEIL_LANG").unwrap_or_else(|_| "zh".into());
    // 语言同样只在首次初始化时固定，后续调用不会改变当前进程语言。
    LANG.set(lang).ok();
}

/// 获取当前语言代码
fn lang() -> &'static str {
    LANG.get().map(|s| s.as_str()).unwrap_or("zh")
}

/// 查询原始消息，不执行占位符替换。
///
/// 找不到语言或消息键时回退为消息键本身，便于定位缺失翻译。
fn raw(key: &str) -> &str {
    STORE
        .get()
        .and_then(|store| store.get(key))
        .and_then(|map| map.get(lang()))
        .map(|s| s.as_str())
        .unwrap_or(key) // fallback: 显示 key，方便发现漏翻
}

/// 获取翻译文本（无占位符替换）
///
/// # 参数
/// - `key`：消息表键名。
///
/// # 返回
/// 返回当前语言的文本；键不存在时返回 `key` 本身。
///
/// ```text
/// let text = i18n::t("opening_container"); // "正在打开容器..." 或 "Opening container..."
/// ```
pub fn t(key: &str) -> &str {
    raw(key)
}

/// 获取翻译文本，替换一个占位符 `{name}` → `value`
///
/// # 参数
/// - `key`：消息表键名。
/// - `name`：不带花括号的占位符名称。
/// - `value`：替换文本。
///
/// # 返回
/// 返回替换后的新字符串；消息缺失时仍会以键名执行替换。
///
/// ```text
/// let text = i18n::t1("add.source_not_found", "path", "/some/file.txt");
/// ```
pub fn t1(key: &str, name: &str, value: &str) -> String {
    // 占位符按 `{name}` 形式构造，调用方无需关心消息模板存储格式。
    raw(key).replace(&format!("{{{name}}}"), value)
}

/// 获取翻译文本并依次替换两个命名占位符。
///
/// 参数按“名称、值、名称、值”成对传入，返回替换后的新字符串。
pub fn t2(key: &str, n1: &str, v1: &str, n2: &str, v2: &str) -> String {
    // 两个占位符顺序替换，名称互不相同，结果与调用方参数顺序一致。
    raw(key)
        .replace(&format!("{{{n1}}}"), v1)
        .replace(&format!("{{{n2}}}"), v2)
}

/// 获取翻译文本并依次替换三个命名占位符。
///
/// 参数按“名称、值”成对传入，返回替换后的新字符串。
pub fn t3(key: &str, n1: &str, v1: &str, n2: &str, v2: &str, n3: &str, v3: &str) -> String {
    // 三个占位符只做字符串替换，不解析格式字符串或转义序列。
    raw(key)
        .replace(&format!("{{{n1}}}"), v1)
        .replace(&format!("{{{n2}}}"), v2)
        .replace(&format!("{{{n3}}}"), v3)
}

/// 使用命名参数映射渲染消息模板。
///
/// 未提供的占位符保持原样，便于开发阶段发现遗漏参数。
pub fn render(key: &str, params: &BTreeMap<&'static str, String>) -> String {
    let mut message = raw(key).to_string();
    for (name, value) in params {
        message = message.replace(&format!("{{{name}}}"), value);
    }
    message
}

/// 返回 clap 简短说明，并注入当前 crate 版本。
pub fn clap_about() -> String {
    t1("clap.about", "version", env!("CARGO_PKG_VERSION"))
}

/// 返回 clap 长说明，并注入当前 crate 版本。
pub fn clap_long_about() -> String {
    t1("clap.long_about", "version", env!("CARGO_PKG_VERSION"))
}
