//! 编译期内嵌的轻量国际化资源。
//!
//! `messages.jsonc` 在编译时嵌入二进制，启动后按 `VEIL_LANG` 选择语言。模块保持一个
//! 进程级只读消息表和语言代码，翻译查询不执行 I/O，占位符按名称替换。
//!
//! 核心清单：`init` 一次性加载消息表并固定语言；`t`/`t1`/`t2`/`t3`/`render` 查询文本
//! 并替换占位符；`clap_about`/`clap_long_about` 提供 clap 命令树的说明文本。

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

/// 一个消息键下的“语言代码 → 模板文本”映射，语言通常为zh/en，文本可含命名占位符。
type LangMap = HashMap<String, String>;
/// 所有语言的消息表。
///
/// 外层键为消息键（如add.source_not_found），内层LangMap保存该消息各语言模板。
type MsgStore = HashMap<String, LangMap>;

/// 启动时初始化一次的完整消息表。
static STORE: OnceLock<MsgStore> = OnceLock::new();
/// 启动时解析一次的当前语言代码。
static LANG: OnceLock<String> = OnceLock::new();

/// 解析内嵌消息表，并尝试固定本进程使用的语言。
/// 入参：无，读取VEIL_LANG，缺失或非Unicode时使用zh。
/// 返回值：无，STORE和LANG各自仅接受首次成功set。
/// 异常：内嵌JSONC解析失败expect panic。
/// 注意事项：重复调用仍先重新解析表，再忽略set失败；两个OnceLock分别初始化，不是整体原子发布。VEIL_LANG原样保存，不识别的语言查询回退键名。
pub fn init() {
    let store: MsgStore = veil_core::jsonc::from_str(include_str!("messages.jsonc"))
        .expect("i18n/messages.jsonc 格式错误");
    // OnceLock 只接受第一次写入，重复调用 init 时保留已有消息表。
    STORE.set(store).ok();

    let lang = std::env::var("VEIL_LANG").unwrap_or_else(|_| "zh".into());
    // 语言同样只在首次初始化时固定，后续调用不会改变当前进程语言。
    // 补充：固定语言可保证同一次进程输出的语言一致。
    LANG.set(lang).ok();
}

/// 取得本进程固定的显示语言代码。
/// 入参：无。
/// 返回值：&'static str，LANG已设置取保存文本，否则zh。
/// 异常：无错误返回。
/// 注意事项：不触发init、不重新读取环境；默认zh并不意味着消息表已加载。
fn lang() -> &'static str {
    LANG.get().map(|s| s.as_str()).unwrap_or("zh")
}

/// 根据消息键和当前语言取模板，缺失时保留键名便于排查。
/// 入参：key（&str）为消息键，如opening_veil。
/// 返回值：表中找到则返回模板借用；未初始化/键缺失/语言缺失均返回输入key。
/// 异常：无错误返回。
/// 注意事项：不回退到中文或英语，不做占位符替换；返回借用不能越过输入key寿命。
fn raw(key: &str) -> &str {
    STORE
        .get()
        .and_then(|store| store.get(key))
        .and_then(|map| map.get(lang()))
        .map(|s| s.as_str())
        .unwrap_or(key) // fallback: 显示 key，方便发现漏翻
}

/// 取得无需参数替换的本地化文案。
/// 入参：key（&str）为消息键。
/// 返回值：当前语言模板或缺失时原key。
/// 异常：无错误返回。
/// 注意事项：依赖启动时init；不解释模板中的花括号。
pub fn t(key: &str) -> &str {
    raw(key)
}

/// 用一份命名参数渲染本地化模板。
/// 入参：key为消息键；name（&str）为不带花括号的参数名；value（&str）为替换文本。
/// 返回值：String，把所有对应{name}替换为value，其他占位符保留。
/// 异常：无错误返回。
/// 注意事项：不做转义/格式化，不验证参数名存在；缺消息时对key本身执行替换。
pub fn t1(key: &str, name: &str, value: &str) -> String {
    // 占位符按 `{name}` 形式构造，调用方无需关心消息模板存储格式。
    raw(key).replace(&format!("{{{name}}}"), value)
}

/// 按给定顺序替换2份命名参数，生成CLI文案。
/// 入参：key为消息键；n1/v1（&str）分别为第1份参数名/替换值；n2/v2（&str）分别为第2份参数名/替换值。
/// 返回值：String，逐次替换后的模板。
/// 异常：无错误返回。
/// 注意事项：参数名不要求不同；后面的替换还会作用于前面插入值中的占位符，顺序可能影响结果。
pub fn t2(key: &str, n1: &str, v1: &str, n2: &str, v2: &str) -> String {
    // 顺序替换两个占位符，不检查名称唯一；后续替换可能继续修改先前插入值。
    raw(key)
        .replace(&format!("{{{n1}}}"), v1)
        .replace(&format!("{{{n2}}}"), v2)
}

/// 按给定顺序替换3份命名参数，生成CLI文案。
/// 入参：key为消息键；n1/v1（&str）分别为第1份参数名/替换值；n2/v2（&str）分别为第2份参数名/替换值；n3/v3（&str）分别为第3份参数名/替换值。
/// 返回值：String，逐次替换后的模板。
/// 异常：无错误返回。
/// 注意事项：参数名不要求不同；后面的替换还会作用于前面插入值中的占位符，顺序可能影响结果。
pub fn t3(key: &str, n1: &str, v1: &str, n2: &str, v2: &str, n3: &str, v3: &str) -> String {
    // 三个占位符只做字符串替换，不解析格式字符串或转义序列。
    raw(key)
        .replace(&format!("{{{n1}}}"), v1)
        .replace(&format!("{{{n2}}}"), v2)
        .replace(&format!("{{{n3}}}"), v3)
}

/// 用命名参数映射批量渲染错误或提示模板。
/// 入参：key为消息键；params（&`BTreeMap<&'static str,String>`）每项为参数名和已格式化值。
/// 返回值：按键名升序逐项替换后的String，未提供占位符保留。
/// 异常：无错误返回。
/// 注意事项：遍历每项替换全部同名占位符；插入值若含后续占位符也会被再次替换，不是一次性模板解析或转义系统。
pub fn render(key: &str, params: &BTreeMap<&'static str, String>) -> String {
    let mut message = raw(key).to_string();
    // BTreeMap 按键名有序遍历，渲染结果与参数插入顺序无关，输出稳定可复现；
    // 同名占位符在模板里出现多次也会被全部替换。
    for (name, value) in params {
        message = message.replace(&format!("{{{name}}}"), value);
    }
    message
}

/// 为命令帮助取得带当前构建版本的说明文本。
/// 入参：无，版本来自CARGO_PKG_VERSION。
/// 返回值：String，渲染clap.about模板的version参数。
/// 异常：无错误返回。
/// 注意事项：使用编译期版本，语言由全局LANG决定；缺失文案返回键名。
pub fn clap_about() -> String {
    t1("clap.about", "version", env!("CARGO_PKG_VERSION"))
}

/// 为命令帮助取得带当前构建版本的说明文本。
/// 入参：无，版本来自CARGO_PKG_VERSION。
/// 返回值：String，渲染clap.long_about模板的version参数。
/// 异常：无错误返回。
/// 注意事项：使用编译期版本，语言由全局LANG决定；缺失文案返回键名。
pub fn clap_long_about() -> String {
    t1("clap.long_about", "version", env!("CARGO_PKG_VERSION"))
}
