use std::collections::HashMap;
use std::sync::OnceLock;

type LangMap = HashMap<String, String>;
type MsgStore = HashMap<String, LangMap>;

static STORE: OnceLock<MsgStore> = OnceLock::new();
static LANG: OnceLock<String> = OnceLock::new();

/// 启动时调用一次。从 `VEIL_LANG` 环境变量读取语言（默认 `zh`），
/// 并加载编译时嵌入的 `messages.json`。
pub fn init() {
    let store: MsgStore =
        serde_json::from_str(include_str!("messages.json")).expect("i18n/messages.json 格式错误");
    STORE.set(store).ok();

    let lang = std::env::var("VEIL_LANG").unwrap_or_else(|_| "zh".into());
    LANG.set(lang).ok();
}

// ---- 内部辅助 ----

/// 获取当前语言代码
fn lang() -> &'static str {
    LANG.get().map(|s| s.as_str()).unwrap_or("zh")
}

/// 原始消息查找（不替换占位符）
fn raw(key: &str) -> &str {
    STORE
        .get()
        .and_then(|store| store.get(key))
        .and_then(|map| map.get(lang()))
        .map(|s| s.as_str())
        .unwrap_or(key) // fallback: 显示 key，方便发现漏翻
}

// ---- 公开 API ----

/// 获取翻译文本（无占位符替换）
///
/// ```text
/// let text = i18n::t("opening_container"); // "正在打开容器..." 或 "Opening container..."
/// ```
pub fn t(key: &str) -> &str {
    raw(key)
}

/// 获取翻译文本，替换一个占位符 `{name}` → `value`
///
/// ```text
/// let text = i18n::t1("add.source_not_found", "path", "/some/file.txt");
/// ```
pub fn t1(key: &str, name: &str, value: &str) -> String {
    raw(key).replace(&format!("{{{name}}}"), value)
}

/// 获取翻译文本，替换两个占位符
pub fn t2(key: &str, n1: &str, v1: &str, n2: &str, v2: &str) -> String {
    raw(key)
        .replace(&format!("{{{n1}}}"), v1)
        .replace(&format!("{{{n2}}}"), v2)
}

/// 获取翻译文本，替换三个占位符。
pub fn t3(key: &str, n1: &str, v1: &str, n2: &str, v2: &str, n3: &str, v3: &str) -> String {
    raw(key)
        .replace(&format!("{{{n1}}}"), v1)
        .replace(&format!("{{{n2}}}"), v2)
        .replace(&format!("{{{n3}}}"), v3)
}

/// clap about 文本（含版本号）
pub fn clap_about() -> String {
    t1("clap.about", "version", env!("CARGO_PKG_VERSION"))
}

/// clap long_about 文本（含版本号）
pub fn clap_long_about() -> String {
    t1("clap.long_about", "version", env!("CARGO_PKG_VERSION"))
}
