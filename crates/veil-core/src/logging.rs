//! 统一日志入口。错误消息与错误链在此收敛为可安全导出的描述，业务层不得直接调用日志后端。
//! 正常流程不逐项记录；重复故障合并统计，调用堆栈默认关闭。
//! 写盘由独立线程完成，调用方只做有界入队；导出会排空队列，退出与切后台前必须调用 flush。
//!
//! # 业务定位
//!
//! 这是 Veil 全应用唯一的日志通道：各业务层只调用本模块入口，不允许绕过它
//! 直接写日志后端。核心职责：
//! 1. 把错误/事件收敛成可安全导出的描述——错误链中只有"白名单已知文案"才会
//!    进入日志，任意明文（可能含路径、密码）一律不回声，杜绝敏感信息泄漏。
//! 2. 上下文字段只接受数值、静态标签与资源指纹（BLAKE3 哈希），禁止传密码、
//!    原始 SQL 参数或文件内容。
//! 3. 性能：等级关闭时尽早返回，省去记录构建与写盘；写盘在独立线程
//!    "veil-log-writer" 完成，业务线程只做有界入队（4096 容量，满则丢弃并计数）。
//! 4. 可靠性：日志自身的 panic 不能终止写线程；重复故障在 30 秒窗口内合并统计；
//!    导出（export/snapshot）前先排空队列，退出/切后台前必须 flush。
//!
//! # 文件组织
//!
//! 日志目录内是 JSONL（每行一条 JSON）轮转文件 errors-0.jsonl … errors-N.jsonl，
//! 单文件上限默认 1 MiB、最多 4 个；写满时整体后移轮转。目录加跨进程文件锁，
//! 权限收紧（Unix 0700 目录 / 0600 文件）并拒绝符号链接目录。
//!
//! 核心清单：Config 决定过滤/轮转/合并，Record/Field 表达记录与受限上下文；event/failure/client_error 收敛入口，
//! LogContext/RequestScope 跨执行片段传递关联和脱敏材料；enqueue/run_writer/write_now/append 完成缓冲、消费和文件维护。
//! flush/snapshot/export 供收尾与反馈使用。依赖 Serde JSONL、标准有界通道与后台线程、进程内状态锁、文件锁及 redact 规则。
//! 整体目标是尽力保留可诊断信息并减少秘密进入日志，记录失败通过健康计数体现，不改变原业务结果。
mod redact;
pub use redact::{SensitiveScope, remember as remember_sensitive, scope as sensitive_scope};
use serde::{Deserialize, Serialize};
use std::{
    backtrace::Backtrace,
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    error::Error,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, Once, OnceLock,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

/// 日志等级：低于当前最低等级的记录直接丢弃（提前返回）。
///
/// 业务定位：Debug < Info < Warn < Error；默认只记录 Warn 及以上。
/// 用 u8 表示便于原子变量比较（`Level::Warn as u8`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// 调试细节：默认关闭，排查时单独打开。
    Debug,
    /// 一般信息：如任务开始/完成。
    Info,
    /// 警告：可恢复的异常（如某条记录写入失败）。
    Warn,
    /// 错误：需要用户或开发者关注的故障。
    Error,
}

/// 日志系统运行配置。
///
/// 业务定位：决定日志开不开、记哪些等级、写到哪里、单文件多大、保留几个、
/// 重复故障如何合并、是否采集堆栈。由上层在启动时集中配置。
#[derive(Clone, Debug)]
pub struct Config {
    /// 是否启用日志；关闭时等级阈值置为最大，所有记录短路丢弃。
    pub enabled: bool,
    /// 只记录不低于该等级的记录。
    pub minimum_level: Level,
    /// 日志目录（JSONL 轮转文件所在位置）。
    pub directory: PathBuf,
    /// 单个日志文件字节上限（超出即轮转），允许 4 KiB..=16 MiB。
    pub max_file_bytes: u64,
    /// 轮转保留的最大文件数（1..=32）。
    pub max_files: usize,
    /// 相同故障在此窗口内合并；导出时补上累计次数，零表示关闭合并。
    /// 业务含义：30 秒内同一条故障只落盘一次，后续重复只累计计数，
    /// 避免刷屏/刷盘；导出时以 repeat_summary 记录补上累计次数。
    pub repeat_interval: Duration,
    /// 普通错误的堆栈默认关闭，排查时可集中打开。panic 总是采集一次。
    pub capture_backtrace: bool,
}
impl Config {
    /// 功能：为指定目录建立默认日志策略，供启动配置或按需初始化使用。
    /// 入参：directory: PathBuf 为计划保存 JSONL 的目录；此处仅保存路径，不检查是否绝对或可写。
    /// 返回值：Config，启用 Warn 及以上，单文件 1 MiB、4 个轮转文件、30 秒重复合并窗口，默认不采集普通错误堆栈。
    /// 异常：只构造内存配置，不返回错误。
    /// 注意事项：真正应用时仍要 validate；构造不会创建目录、启动写线程或改变当前阈值。
    pub fn new(directory: PathBuf) -> Self {
        Self {
            enabled: true,
            minimum_level: Level::Warn,
            directory,
            max_file_bytes: 1024 * 1024,
            max_files: 4,
            repeat_interval: Duration::from_secs(30),
            capture_backtrace: false,
        }
    }
}

/// 一条结构化日志记录（JSONL 的一行），正常由受限日志入口构造，也可从历史文件反序列化。
///
/// 业务定位：这是落盘和导出的数据形态；字段本身是公开字符串，类型不能证明任意构造值都已脱敏。
/// 新记录依赖入口白名单和字段转换，旧记录导出按原有内容解释，不做完整隐私重审。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    /// 文本策略版本：0 为旧版正则脱敏记录，1 为白名单记录。
    /// 导出按存储内容原样输出，不再过滤旧版文本。
    /// 业务含义：区分"旧版正则脱敏"与"新版白名单收敛"两代日志文本策略。
    #[serde(default)]
    pub privacy_version: u8,
    /// 记录产生时间（RFC 3339）。
    pub timestamp: String,
    /// 日志等级。
    pub level: Level,
    /// 业务组件名（如 "storage"、"runtime"、"client"）。
    pub component: String,
    /// 业务操作名（如 "write"、"cleanup"）。
    pub operation: String,
    /// 稳定错误分类码（如 "SQLITE_BUSY"、"IO_NOT_FOUND"），便于聚合统计。
    pub code: String,
    /// 底层原生错误码（如 SQLite 扩展码、OS errno）。
    #[serde(default)]
    pub native_code: Option<i64>,
    /// 产生记录的应用版本号。
    pub version: String,
    /// 运行平台（如 "macos"）。
    pub platform: String,
    /// 进程号。
    pub process_id: u32,
    /// 产生记录的文件名（不含目录，避免泄漏路径）。
    pub source_file: String,
    /// 产生记录的行号。
    pub source_line: u32,
    /// 事件标识（一般与 code 相同；panic 为 "PANIC"）。
    #[serde(default)]
    pub event: String,
    /// 主消息：白名单文案或安全错误描述；未识别错误回退为分类码。
    #[serde(default)]
    pub message: String,
    /// 错误链中其余可安全描述的原因列表。
    #[serde(default)]
    pub causes: Vec<String>,
    /// 上下文键值（只有数值/标签/指纹，无明文）。
    #[serde(default)]
    pub context: BTreeMap<String, serde_json::Value>,
    /// 关联的请求编号（用于把一次请求的多条日志串起来）。
    #[serde(default)]
    pub request_id: Option<String>,
    /// 产生记录的线程标识。
    #[serde(default)]
    pub thread_id: String,
    /// 该条故障在合并窗口内被抑制的重复次数（导出时补上）。
    #[serde(default)]
    pub suppressed_count: u64,
    /// 是否为重复故障的汇总记录。
    #[serde(default)]
    pub repeat_summary: bool,
    /// 堆栈文本（已脱敏；默认 None）。
    #[serde(default)]
    pub backtrace: Option<String>,
}

/// 上下文只接受数值、静态标签和资源指纹，禁止传入密码、原始 SQL 参数或文件内容。
///
/// 业务定位：日志上下文的"白名单类型"。每种取值在 `fields` 里被安全转换：
/// 资源/路径转换为 16 个十六进制字符的 BLAKE3 指纹；静态标签和键名仍须由调用方选择安全内容。
#[derive(Clone, Copy)]
pub enum Field<'a> {
    /// 纯计数（如失败条数），原样输出数值。
    Count(u64),
    /// 时长，统一换算成毫秒输出。
    Duration(Duration),
    /// 编译期静态标签（如 "full"、"dedicated"），只允许代码内写死的白名单文本。
    Label(&'static str),
    /// 资源标识（文件 ID 等），输出 16 位 BLAKE3 指纹。
    Resource(&'a str),
    /// 文件路径，输出 16 位 BLAKE3 指纹（用 OsStr 原始字节哈希）。
    Path(&'a Path),
}
/// 功能：把受限类型的业务上下文转成日志 JSON 字段，路径和资源标识仅保留指纹。
/// 入参：values: &[(&'static str, Field)] 为键和值列表，每项是一条诊断维度，最多处理前 24 对。
/// 返回值：`BTreeMap<String, Value>`，计数/标签按原值输出，时长换算毫秒并截到 u64，资源/路径为 16 字符哈希。
/// 异常：不返回 Result，不访问磁盘。
/// 注意事项：重复键由后项覆盖，静态键和 Label 依赖调用方选择安全文本；指纹不等于不可猜测的匿名化。
/// 路径按编码字节散列，不处理大小写、符号链接或路径别名。
fn fields(values: &[(&'static str, Field<'_>)]) -> BTreeMap<String, serde_json::Value> {
    values
        .iter()
        // 上限 24 对：防止调用方塞入超大上下文拖慢日志。
        .take(24)
        .map(|(name, value)| {
            let value = match value {
                Field::Count(value) => serde_json::json!(value),
                Field::Duration(value) => {
                    // 时长以毫秒输出，并封顶到 u64 上限防止溢出。
                    serde_json::json!(value.as_millis().min(u64::MAX as u128) as u64)
                }
                Field::Label(value) => serde_json::json!(value),
                // 资源标识 → 16 位十六进制 BLAKE3 指纹。
                Field::Resource(value) => {
                    serde_json::json!(&blake3::hash(value.as_bytes()).to_hex()[..16])
                }
                // 路径 → 按平台编码字节哈希的 16 位指纹；不做大小写、符号链接或路径别名归一化。
                Field::Path(value) => serde_json::json!(
                    &blake3::hash(value.as_os_str().as_encoded_bytes()).to_hex()[..16]
                ),
            };
            ((*name).into(), value)
        })
        .collect()
}

// 线程本地请求编号：一次请求/任务内的日志共享同一编号，便于跨线程关联。
thread_local! { static REQUEST: RefCell<Option<Arc<str>>> = const { RefCell::new(None) }; }
/// 请求编号作用域：进入时替换当前线程的请求编号，离开时恢复。
///
/// 业务定位：RAII（资源获取即初始化）守卫，保证请求编号不会串到无关线程
/// 的后续日志里。
pub struct RequestScope(Option<Arc<str>>);
impl Drop for RequestScope {
    /// 功能：恢复进入当前日志作用域前的请求编号，结束本次请求的日志关联。
    /// 入参：self 为当前线程的 RequestScope 守卫。
    /// 返回值：无；线程本地编号恢复为先前的 Some 或 None。
    /// 异常：不返回错误；线程本地存储销毁阶段访问失败或 RefCell 重复借用可能 panic。
    /// 注意事项：守卫按嵌套顺序释放；编号不会自动传播到新建线程。
    fn drop(&mut self) {
        // 离开作用域恢复之前的编号（可能是 None）。
        REQUEST.with(|id| *id.borrow_mut() = self.0.take());
    }
}
/// 功能：读取当前线程日志的请求关联编号，便于将同一次操作的记录串联。
/// 入参：无，从线程本地 REQUEST 读取。
/// 返回值：Some(String) 为编号的独立文本副本，未绑定时为 None。
/// 异常：TLS 或 RefCell 借用冲突可能 panic，不返回 Result。
/// 注意事项：不生成新编号，也不自动读取其他线程的上下文。
pub fn request_id() -> Option<String> {
    REQUEST.with(|id| id.borrow().as_ref().map(|id| id.to_string()))
}
/// 功能：为当前调用链建立或继承日志关联编号，并在离开时恢复原线程状态。
/// 入参：无；已有请求号时沿用，否则使用进程号、首次生成请求号时的 UTC 微秒时间和递增序号拼接。
/// 返回值：RequestScope，保存替换前的编号，析构时恢复。
/// 异常：不返回 Result，线程局部存储访问或 RefCell 借用冲突可能 panic。
/// 注意事项：编号用于诊断关联，不是安全令牌；时间基点不是精确进程启动时刻，也不承诺跨所有进程历史绝无碰撞。
pub fn request_scope() -> RequestScope {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    static INSTANCE: OnceLock<i64> = OnceLock::new();
    let id = request_id().unwrap_or_else(|| {
        // 用 PID、首次生成编号的时间和递增序号减少关联号冲突；不把它当作随机安全令牌或永久全局身份。
        format!(
            "{}-{}-{}",
            std::process::id(),
            INSTANCE.get_or_init(|| chrono::Utc::now().timestamp_micros()),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    });
    attach_request(Some(id))
}
/// 功能：把一个已有请求号绑定到当前工作线程，使异步或跨线程日志保持关联。
/// 入参：id: `Option<String>` 为需要绑定的号，Some 原样接入，None 临时清空当前编号。
/// 返回值：RequestScope，持有旧编号并在析构时恢复。
/// 异常：TLS 或 RefCell 借用冲突可能 panic，不返回 Result。
/// 注意事项：不校验编号格式、不脱敏编号；调用方应传入系统生成的诊断 ID，并按嵌套顺序在绑定线程释放守卫。
pub fn attach_request(id: Option<String>) -> RequestScope {
    RequestScope(REQUEST.with(|current| current.replace(id.map(Arc::from))))
}

/// 仅在内存中传递，不能 Debug 或序列化（其中包含脱敏所需的密码）。
///
/// 业务定位：跨线程传递"日志上下文"（请求编号 + 脱敏所需的敏感值集合）。
/// 敏感值含密码，因此本结构不实现 Debug/序列化，防止误打印。
#[derive(Clone)]
pub struct LogContext {
    /// 请求编号（跨线程关联日志用）。
    request: Option<Arc<str>>,
    /// 脱敏所需的敏感值集合（Zeroizing 包装，析构清零）。
    secrets: Arc<Vec<zeroize::Zeroizing<String>>>,
}
/// 功能：捕获当前工作关联号和敏感值表，供受管任务迁移到其他执行线程。
/// 入参：无，读取本线程 REQUEST 与脱敏上下文。
/// 返回值：LogContext，以 Arc 共享编号和敏感表，不深拷贝每条秘密。
/// 异常：TLS 或 RefCell 借用冲突可能 panic，不返回 Result。
/// 注意事项：捕获不会自动绑定目标线程；共享引用会延长敏感值表的生命周期，不能据原作用域结束推断所有副本已清零。
pub fn capture_context() -> LogContext {
    LogContext {
        request: REQUEST.with(|id| id.borrow().clone()),
        secrets: redact::capture(),
    }
}
impl LogContext {
    /// 功能：在实际执行线程恢复捕获的日志关联和脱敏表，结束时恢复该线程此前状态。
    /// 入参：self: &LogContext 为任务创建时捕获的上下文。
    /// 返回值：(RequestScope, SensitiveScope)，分别控制请求号与敏感表恢复。
    /// 异常：TLS 或 RefCell 借用冲突可能 panic，不返回 Result。
    /// 注意事项：两个守卫都要保持到此次执行片段结束；只影响当前线程，嵌套使用按既有作用域顺序释放。
    pub fn attach(&self) -> (RequestScope, SensitiveScope) {
        (
            RequestScope(REQUEST.with(|current| current.replace(self.request.clone()))),
            redact::attach(self.secrets.clone()),
        )
    }
}

/// 重复故障合并条目：记录首次落盘时间、最近一条记录与抑制计数。
struct Repeated {
    /// 首次（或最近）落盘时间，用于判断是否超过合并窗口。
    emitted: Instant,
    /// 该故障最近一次的代表记录（含最新上下文）。
    record: Record,
    /// 窗口内被抑制的重复次数（导出时补写成汇总记录）。
    count: u64,
}
/// 日志全局状态：配置、写失败统计与重复合并表。
struct State {
    /// 当前生效配置（首次写入时自动初始化默认配置）。
    config: Option<Config>,
    /// 累计写失败次数。
    failed_writes: u64,
    /// 最近一次写失败的 IO 错误类型。
    last_failure: Option<io::ErrorKind>,
    /// 已经以"日志恢复"事件上报过的失败数（避免重复上报）。
    reported_failures: u64,
    /// 写失败后的退避截止时间：此前不再尝试文件系统操作。
    retry_after: Option<Instant>,
    /// 重复故障合并表：key=故障特征串，value=合并条目。
    repeated: Option<HashMap<String, Repeated>>,
}
// 全局状态互斥锁：写盘、配置切换、导出共用。
static STATE: Mutex<State> = Mutex::new(State {
    config: None,
    failed_writes: 0,
    last_failure: None,
    reported_failures: 0,
    retry_after: None,
    repeated: None,
});
// 当前最低记录等级（原子读，调用线程零锁判断是否丢弃）。
static MINIMUM_LEVEL: AtomicU8 = AtomicU8::new(Level::Warn as u8);
// 是否采集普通错误堆栈（原子读）。
static CAPTURE_BACKTRACE: AtomicBool = AtomicBool::new(false);
// 因状态锁争用而放弃写入的计数。
static CONTENDED_WRITES: AtomicU64 = AtomicU64::new(0);
// 因入队队列满而丢弃的记录计数。
static DROPPED_RECORDS: AtomicU64 = AtomicU64::new(0);
// 日志写线程的命令队列：业务线程只 try_send 入队，满则丢弃。
static QUEUE: OnceLock<Option<mpsc::SyncSender<Command>>> = OnceLock::new();
// 写线程自身的 ThreadId：防止写线程上排空队列时等待自己。
static WRITER: OnceLock<std::thread::ThreadId> = OnceLock::new();
// 跨进程目录锁的最长等待。
const LOCK_WAIT: Duration = Duration::from_millis(500);
// 全局状态锁的最长等待。
const STATE_WAIT: Duration = Duration::from_millis(500);
// 命令队列容量：业务线程入队上限，满则丢弃并计数。
const QUEUE_CAPACITY: usize = 4096;
// 常规排空等待上限（flush/导出/切配置共用）。
const DRAIN_WAIT: Duration = Duration::from_secs(2);
// panic 时排空等待上限：进程随时可能退出，只能短等。
const PANIC_DRAIN_WAIT: Duration = Duration::from_millis(200);

/// 写线程收到的命令：写一条记录，或排空并回执。
enum Command {
    /// 写一条日志记录（Box 减少栈上搬移）。
    Write(Box<Record>),
    /// 请求排空当前队列；完成后用通道回执成功与否。
    Flush(mpsc::Sender<bool>),
}

/// 功能：快速判断指定级别是否值得构造日志，减少关闭等级的异常文本和指纹处理开销。
/// 入参：level: Level 为待产生记录的等级。
/// 返回值：等级编码不低于当前 MINIMUM_LEVEL 时为 true，日志关闭时阈值最大，因此所有等级为 false。
/// 异常：只做原子读取与比较，不返回错误。
/// 注意事项：这是瞬时过滤结果，配置之后仍可能改变；true 不保证入队或写盘成功。
pub fn enabled(level: Level) -> bool {
    level as u8 >= MINIMUM_LEVEL.load(Ordering::Relaxed)
}

/// 功能：有界竞争日志配置、合并表和健康计数的共享状态，避免日志异常路径一直等待自己。
/// 入参：无，使用全局 STATE 互斥锁。
/// 返回值：Ok(`MutexGuard<State>`) 为状态独占守卫，调用方持有期间其他状态操作等待。
/// 异常：正在 panic 且锁被占用，或轮询已达 500 毫秒时返回 TimedOut；锁中毒则取回内部状态。
/// 注意事项：每轮尝试后睡眠 1 毫秒，不保证墙钟耗时精确小于 500 毫秒；恢复中毒访问不等于自动修复所有业务副作用。
fn state_lock() -> io::Result<MutexGuard<'static, State>> {
    let started = Instant::now();
    loop {
        match STATE.try_lock() {
            Ok(state) => return Ok(state),
            // 毒化锁：之前持锁线程 panic 过，状态本身可继续使用。
            Err(std::sync::TryLockError::Poisoned(error)) => return Ok(error.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                // 正在 panic 的线程不能无限等锁；超时同样放弃。
                if std::thread::panicking() || started.elapsed() >= STATE_WAIT {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "日志状态锁等待超时",
                    ));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

/// 功能：在日志存储失败时累计可诊断状态，并暂缓重复碰触故障存储。
/// 入参：state: &mut State 为已独占的日志状态；error: &io::Error 为此次失败，保存其类别而非原文。
/// 返回值：无；failed_writes 饱和加一，last_failure 更新，retry_after 设为当前单调时刻后一秒。
/// 异常：不执行 I/O，不返回错误。
/// 注意事项：调用方须持状态锁；只设置退避状态，不自动重投本次失败的记录或启动重试线程。
fn note_failure(state: &mut State, error: &io::Error) {
    state.failed_writes = state.failed_writes.saturating_add(1);
    state.last_failure = Some(error.kind());
    // 退避截止点：存储故障时避免每条记录都去碰坏掉的文件系统。
    state.retry_after = Some(Instant::now() + Duration::from_secs(1));
}

/// 功能：向上层提供日志系统累计丢失/写入失败线索，便于显示诊断健康状况。
/// 入参：无，读取 STATE 与争用/丢弃两个原子计数。
/// 返回值：(u64, `Option<io::ErrorKind>`)，数量为三类计数的饱和总和，类别优先真实失败，再锁争用，再队列丢弃。
/// 异常：STATE 锁中毒时恢复内部状态，不返回 Result。
/// 注意事项：这里使用阻塞 STATE.lock 而非有界 state_lock，可能等待正在读盘/写盘的线程；返回只是瞬时计数，不证明未上报日志已恢复。
pub fn health() -> (u64, Option<io::ErrorKind>) {
    let state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let contended = CONTENDED_WRITES.load(Ordering::Relaxed);
    let dropped = DROPPED_RECORDS.load(Ordering::Relaxed);
    (
        state
            .failed_writes
            .saturating_add(contended)
            .saturating_add(dropped),
        // 错误类型优先级：真实写失败 > 锁争用(TimedOut) > 队列丢弃(WouldBlock)。
        state
            .last_failure
            .or((contended > 0).then_some(io::ErrorKind::TimedOut))
            .or((dropped > 0).then_some(io::ErrorKind::WouldBlock)),
    )
}
/// 功能：应用新的日志目录、过滤级别、轮转和合并策略，尽量收尾旧配置下的排队记录。
/// 入参：config: Config 为完整新配置，按值保存到全局状态。
/// 返回值：校验和取得状态锁成功后返回 Ok(())，更新级别、堆栈开关及配置。
/// 异常：目录不是绝对路径、文件大小/数量越界返回 InvalidInput；状态锁等待失败返回 TimedOut。
/// 注意事项：先尝试 drain 两秒但忽略其失败，旧摘要写入失败也只记健康状态；因此成功切换不保证所有旧记录都已写到旧目录。
/// 这里只验证配置形状，不预先保证目录存在、无符号链接或可写，实际写盘时另行检查。
pub fn configure(config: Config) -> io::Result<()> {
    validate(&config)?;
    // 先按旧配置排空已入队记录，再切换目录与级别。
    let _ = drain(DRAIN_WAIT);
    let mut state = state_lock()?;
    // 切换配置前把合并窗口内欠着的重复摘要补写掉（用旧配置路径）。
    match flush_pending(&mut state) {
        Ok(()) => state.repeated = None,
        Err(error) => note_failure(&mut state, &error),
    }
    // 清退避状态：新配置可能换了健康目录。
    state.retry_after = None;
    // enabled=false 时阈值置最大，所有记录短路；否则按配置等级。
    MINIMUM_LEVEL.store(
        if config.enabled {
            config.minimum_level as u8
        } else {
            u8::MAX
        },
        Ordering::Relaxed,
    );
    CAPTURE_BACKTRACE.store(config.capture_backtrace, Ordering::Relaxed);
    state.config = Some(config);
    Ok(())
}
/// 功能：只更换日志位置并保留已有策略，供应用数据根目录确定后切换诊断存储。
/// 入参：directory: PathBuf 为新日志目录，应为绝对路径；目录完全相同则不更新配置。
/// 返回值：Ok(()) 表示配置保持或目录已切换；未配置时用默认策略初始化。
/// 异常：新配置不合法或无法取得状态锁时返回 io::Error；旧摘要写入失败只记健康状态。
/// 注意事项：drain 失败被忽略，不能保证旧队列完全处理后才切换；不预先创建或探测目标目录，不更新其他策略字段。
pub fn set_directory(directory: PathBuf) -> io::Result<()> {
    // 先按旧目录排空已入队记录，避免切目录后写错位置。
    let _ = drain(DRAIN_WAIT);
    let mut state = state_lock()?;
    // 目录没变：无需任何动作。
    if state
        .config
        .as_ref()
        .is_some_and(|c| c.directory == directory)
    {
        return Ok(());
    }
    // 尚未配置过时用默认配置打底，仅替换目录。
    let mut config = state
        .config
        .clone()
        .unwrap_or_else(|| Config::new(directory.clone()));
    config.directory = directory;
    if let Err(error) = validate(&config) {
        note_failure(&mut state, &error);
        return Err(error);
    }
    // 同样先补写旧目录欠着的重复摘要。
    match flush_pending(&mut state) {
        Ok(()) => state.repeated = None,
        Err(error) => note_failure(&mut state, &error),
    }
    state.retry_after = None;
    state.config = Some(config);
    Ok(())
}
/// 功能：拒绝会使日志轮转边界失效的基本配置。
/// 入参：config: &Config，检查 directory、max_file_bytes 和 max_files。
/// 返回值：目录绝对、文件上限为 4 KiB～16 MiB、文件数量为 1～32 时 Ok(())。
/// 异常：任一条件不符返回 InvalidInput。
/// 注意事项：不检查目录权限、符号链接、剩余空间或 repeat_interval 等业务取值，不创建目录。
fn validate(config: &Config) -> io::Result<()> {
    if !config.directory.is_absolute()
        || !(4096..=16 * 1024 * 1024).contains(&config.max_file_bytes)
        || !(1..=32).contains(&config.max_files)
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "日志配置无效"));
    }
    Ok(())
}
/// 功能：按应用数据目录约定定位默认日志位置，供首次写入或导出初始化使用。
/// 入参：无，优先读 VEIL_APP_DATA_DIR，否则取平台本地数据目录并加 Veil。
/// 返回值：Ok(Config)，目录为选定应用根的 logs 子目录，其他字段为默认策略。
/// 异常：无法定位平台目录返回 NotFound，环境注入导致路径非绝对等配置非法返回 InvalidInput。
/// 注意事项：环境变量优先但不自动绝对化；不创建或写入目录，实际可访问性后续才确定。
fn default_config() -> io::Result<Config> {
    // 显式注入的应用数据目录（测试/部署场景）优先。
    let directory = if let Some(path) = std::env::var_os("VEIL_APP_DATA_DIR") {
        PathBuf::from(path)
    } else {
        // 桌面平台的数据目录（如 ~/Library/Application Support）下的 Veil。
        dirs::data_local_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "无法定位日志目录"))?
            .join("Veil")
    };
    let config = Config::new(directory.join("logs"));
    validate(&config)?;
    Ok(config)
}

/// 功能：安装统一 Rust panic 钩子，为未正常完成的执行保留位置和可清洗堆栈。
/// 入参：无，通过 Once 在本进程安装一次。
/// 返回值：无；设置全局 panic hook，错误日志启用时写 PANIC 记录并短暂等候写线程回执。
/// 异常：不返回 Result；安装或钩子执行中的 panic 仍受 Rust 运行时规则约束，此入口不捕获所有异常。
/// 注意事项：替换而非链式调用已有钩子；panic 载荷不回显，堆栈由 write_record 采集；200 毫秒是排空等待预算，不是整段钩子的硬耗时上限。
/// 等待失败被忽略，也不承诺进程异常退出时日志必定持久化。
pub fn initialize() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            // 日志本身关闭时不记录 panic。
            if !enabled(Level::Error) {
                return;
            }
            let location = info.location();
            let details = Details {
                event: "PANIC",
                // panic 参数可能包含明文或凭证；使用位置及堆栈标定崩溃点。
                message: "Rust panic（载荷已省略）".into(),
                ..Details::default()
            };
            write_record(
                Level::Error,
                "runtime",
                "panic",
                "PANIC",
                location.map_or("unknown", |l| l.file()),
                location.map_or(0, |l| l.line()),
                details,
            );
            // panic 后进程可能立即退出；有界等待写线程落盘。
            let _ = drain(PANIC_DRAIN_WAIT);
        }))
    });
}

/// 一条待落盘记录的内部组装形态（Record 落盘前的草稿）。
#[derive(Default)]
struct Details {
    /// 事件标识。
    event: &'static str,
    /// 底层原生错误码。
    native_code: Option<i64>,
    /// 主消息（已收敛为安全文本）。
    message: String,
    /// 错误链其余原因。
    causes: Vec<String>,
    /// 上下文键值（已脱敏）。
    context: BTreeMap<String, serde_json::Value>,
    /// 堆栈（已脱敏）。
    backtrace: Option<String>,
}
/// 记录一条事件：消息文本与 code 相同，无额外上下文。
///
/// 入参：`component`/`operation` 是业务归属；`code` 同时作为事件标识与消息。
#[track_caller]
/// 功能：记录仅有固定分类码的事件，供无需额外错误正文的业务边界使用。
/// 入参：level: Level 为等级；component/operation: &'static str 为固定组件和动作；code: &'static str 同时作为事件、分类和消息。
/// 返回值：无；满足阈值时转交 event 构造和投递。
/// 异常：不返回写入结果，队列满或后端失败通过日志健康计数体现。
/// 注意事项：track_caller 保留调用位置；固定字符串应为安全分类标签，不传入经泄漏成静态引用的业务秘密。
pub fn record(level: Level, component: &'static str, operation: &'static str, code: &'static str) {
    event(level, component, operation, code, code, &[]);
}
/// 记录一条带消息与上下文的事件。
///
/// 入参：`message` 必须是编译期静态文案（白名单）；`context` 是
/// [`Field`] 键值对，内部会脱敏。
///
/// 注意事项：使用 `#[track_caller]` 自动取调用位置（文件+行号）。
#[track_caller]
/// 功能：写入一条带固定业务说明和受限上下文的诊断事件。
/// 入参：level: Level 为级别；component/operation/code/message: &'static str 为组件、动作、分类及安全固定文案；
/// context: &[(&'static str, Field)] 为诊断字段，每项是一条维度，转换时最多取前 24 对。
/// 返回值：无；等级不足直接结束，否则带调用位置转交 write_record。
/// 异常：不返回存储错误；上下文转换、TLS 访问等内部失败可能 panic，后端写失败另记健康状态。
/// 注意事项：静态消息原样使用而非任意文本清洗；资源/路径字段做指纹，队列受容量约束，调用返回不等于记录已写入文件。
pub fn event(
    level: Level,
    component: &'static str,
    operation: &'static str,
    code: &'static str,
    message: &'static str,
    context: &[(&'static str, Field<'_>)],
) {
    // 等级不足直接返回：不取位置、不做任何格式化（提前返回）。
    if !enabled(level) {
        return;
    }
    let location = std::panic::Location::caller();
    write_record(
        level,
        component,
        operation,
        code,
        location.file(),
        location.line(),
        Details {
            event: code,
            message: message.into(),
            context: fields(context),
            ..Details::default()
        },
    );
}
/// 记录一条 Error 级事件（record 的快捷入口）。
#[track_caller]
/// 功能：以 Error 等级记录固定错误分类，简化不带详细来源的失败边界。
/// 入参：component/operation/code: &'static str，分别是固定组件、动作和分类码。
/// 返回值：无，委托 record 并保留 track_caller 的调用位置。
/// 异常：不向业务调用方返回写盘错误。
/// 注意事项：没有原始错误链或上下文；若需保留底层安全原因应调用 failure/error_with_code，而不是拼接动态正文到分类码。
pub fn error(component: &'static str, operation: &'static str, code: &'static str) {
    record(Level::Error, component, operation, code);
}

/// 功能：接收原生客户端经桥接上报的失败，将不可信文案收敛成固定分类和允许的系统错误描述。
/// 入参：category: u32，1～5 分别为编码/解码/文件/响应/校验失败，其他为本地通用失败；source: &str 为源码位置；
/// line: u32 为行号；native_code: i64 为系统数字错误码；message: &str 为原文，只处理前 4096 字符及最多八层约定分段。
/// 返回值：无；允许的编码/解码文案、原生域和整数码被保留，其余正文替换为分类码。
/// 异常：不返回写盘错误或解析错误，无法识别的输入按兜底安全文本处理。
/// 注意事项：source 只按 / 取最后一段并限制字符集，不能把它当作本地文件读取路径；原始 message 不应在客户端另行绕过此入口写日志。
pub fn client_error(category: u32, source: &str, line: u32, native_code: i64, message: &str) {
    if !enabled(Level::Error) {
        return;
    }
    // 客户端分类码映射为稳定的日志分类码。
    let code = match category {
        1 => "CLIENT_ENCODING_ERROR",
        2 => "CLIENT_DECODING_ERROR",
        3 => "CLIENT_FILE_ERROR",
        4 => "CLIENT_RESPONSE_ERROR",
        5 => "CLIENT_VALIDATION_ERROR",
        _ => "CLIENT_LOCAL_ERROR",
    };
    // 来源文件名只保留最后一段路径，并校验为安全字符集，否则退回 "client"。
    let source = source.rsplit('/').next().unwrap_or("client");
    let source = if source.len() <= 96
        && source
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        source
    } else {
        "client"
    };
    // ABI 输入同样不可信；只接受客户端固定分类以及白名单原生域和数值错误码。
    // 先把消息截断到 4096 字符，避免超长文本拖慢异常路径。
    let end = message
        .char_indices()
        .nth(4096)
        .map_or(message.len(), |(offset, _)| offset);
    let mut messages = message[..end]
        .split("\nCaused by: ")
        .take(8)
        .map(|value| match value {
            // 客户端序列化错误的标准文案：白名单直通。
            "编码失败：invalid_value"
            | "解码失败：missing_key"
            | "解码失败：type_mismatch"
            | "解码失败：missing_value"
            | "解码失败：corrupted_data" => value.to_owned(),
            // 形如 "客户端异常：Cocoa code=42"：只接受白名单域 + 纯数字错误码。
            _ => value
                .strip_prefix("客户端异常：")
                .and_then(|value| value.split_once(" code="))
                .and_then(|(domain, number)| {
                    matches!(domain, "Cocoa" | "POSIX" | "URL" | "OSStatus" | "local")
                        .then(|| {
                            number
                                .parse::<i64>()
                                .ok()
                                .map(|number| format!("客户端异常：{domain} code={number}"))
                        })
                        .flatten()
                })
                // 无法归类的内容整体替换成分类码，绝不回显原文。
                .unwrap_or_else(|| code.into()),
        });
    let details = Details {
        event: code,
        native_code: Some(native_code),
        // 第一段为 message，其余段进 causes（最多 8 段）。
        message: messages.next().unwrap_or_else(|| code.into()),
        causes: messages.collect(),
        ..Details::default()
    };
    write_record(
        Level::Error,
        "client",
        "local_operation",
        code,
        source,
        line,
        details,
    );
}

/// 记录错误链：把整条 error 链收敛为安全描述后落盘。
///
/// 入参：`error` 是任意错误对象（可能是多层包装）；分类码回退为
/// "UNCLASSIFIED_ERROR"。
#[track_caller]
/// 功能：用通用兜底分类记录一条多层来源错误，保留可安全解释的原因。
/// 入参：component/operation: &'static str 为固定归属；error: &(dyn Error + 'static) 为借用的错误链起点。
/// 返回值：无，委托 error_with_code，兜底分类为 UNCLASSIFIED_ERROR。
/// 异常：不返回日志写入结果，错误对象的自定义 source 或清洗路径 panic 不在此捕获。
/// 注意事项：不取得错误所有权，不执行重试；层数、隐私和等级过滤沿 failure 的约定。
pub fn error_chain(
    component: &'static str,
    operation: &'static str,
    error: &(dyn Error + 'static),
) {
    error_with_code(component, operation, "UNCLASSIFIED_ERROR", error);
}
/// 记录错误链并指定分类码回退值。
///
/// 入参：`fallback` 是错误链里没有可识别分类时的兜底分类码。
#[track_caller]
/// 功能：在错误链无法识别时仍保留调用方明确指定的失败分类。
/// 入参：component/operation: &'static str 为固定业务位置；fallback: &'static str 为初始分类与事件；error 为借用的 Error 来源链。
/// 返回值：无，按 Error 级委托 failure。
/// 异常：不返回写盘错误，来源检查中的 panic 不在这里捕获。
/// 注意事项：更深层可识别错误可能覆盖 code，而 event 保留 fallback，调用者不能假设两者最终相同。
pub fn error_with_code(
    component: &'static str,
    operation: &'static str,
    fallback: &'static str,
    error: &(dyn Error + 'static),
) {
    failure(Level::Error, component, operation, fallback, error, &[]);
}
// 只输出已知错误的安全表示；无法归类时返回 None，由上层跳过该层包装错误。
// 任意明文的脱敏不可靠，因此未识别的错误不回显 Display 文本。
/// 功能：为错误链的单层来源提取允许写入日志的原因，拒绝随意回显 Display 文本。
/// 入参：error: &(dyn Error + 'static) 为当前层，可能是 VeilError、rusqlite、I/O、JSON 或其他类型。
/// 返回值：Some(String) 为白名单固定原因、底层分类/数字码或 JSON 行列说明；不认识或不允许展示为 None。
/// 异常：不返回 Result，不递归来源链；类型本身格式化固定数据的行为沿其实现。
/// 注意事项：VeilError 文本仅允许指定白名单，I/O 自定义消息不回显；None 表示跳过这一层，不表示错误已处理成功。
fn safe_error_message(error: &(dyn Error + 'static)) -> Option<String> {
    if let Some(error) = error.downcast_ref::<crate::error::VeilError>() {
        use crate::error::VeilError;
        // InvalidFormat/WorkError 携带的原因文本需过白名单才可回显。
        let reason = match error {
            VeilError::InvalidFormat(reason) | VeilError::WorkError(reason) => {
                Some(reason.as_str())
            }
            _ => None,
        };
        // 白名单：这些文案是代码内写死、不含用户输入的固定文本。
        if let Some(reason) = reason.filter(|reason| {
            matches!(
                *reason,
                "锁等待超时"
                    | "文件锁租约已失效，请重新申请"
                    | "文件锁凭证已失效"
                    | "文件锁释放期间凭证发生变化"
                    | "文件锁父目录不匹配"
                    | "文件锁父目录不存在"
                    | "关联锁必须整体交接"
                    | "锁类型不匹配"
                    | "租约状态不可用"
                    | "缺少数据主密钥"
                    | "数据主密钥重复"
                    | "文件缺少 file_id"
                    | "文件版本历史为空"
                    | "文件分块数量过多"
                    | "文件分块序号越界"
                    | "文件分块偏移溢出"
                    | "密文长度超出元数据声明"
                    | "没有可用的数据主密钥"
                    | "文件密钥编码无效"
                    | "文件密钥长度无效"
                    | "文件密钥轮换状态无效"
                    | "文件密钥重复"
                    | "文件密钥 ID 无效"
                    | "文件密钥必须使用统一的小写十六进制编码"
                    | "缺少 nonce"
                    | "nonce 长度无效"
                    | "文件大小缺失"
                    | "加密时间缺失"
                    | "容器根目录缺失"
                    | "目录树存在孤立节点或环"
                    | "文件引用未知密钥"
                    | "文件专用字段缺失或无效"
                    | "目录包含文件专用字段"
                    | "包内数据库被截断"
                    | "包内密文被截断"
                    | "源密文长度变化"
                    | "打包文件存在多余数据"
                    | "打包密文名称重复"
                    | "包内密文名称重复"
                    | "数据库快照大小无效"
                    | "节点身份或类型无效"
                    | "根目录字段无效"
                    | "时间戳超出范围"
            )
        }) {
            return Some(reason.into());
        }
        // 分类码已在 code 字段中；这里返回 None，让更深层的根因成为 message。
        return None;
    }
    if let Some(error) = error.downcast_ref::<rusqlite::Error>() {
        return Some(match error {
            // SQLite 附带文本可能包含 SQL、路径或字段值；ffi 错误表示仅含错误码及固定说明。
            rusqlite::Error::SqliteFailure(failure, _) => failure.to_string(),
            // 以下固定文案：不含任何用户输入，可以安全回显。
            rusqlite::Error::QueryReturnedNoRows => "SQLITE_NO_ROWS".into(),
            rusqlite::Error::QueryReturnedMoreThanOneRow => "SQLITE_TOO_MANY_ROWS".into(),
            rusqlite::Error::InvalidColumnType(..)
            | rusqlite::Error::FromSqlConversionFailure(..) => "SQLITE_COLUMN_CONVERSION".into(),
            rusqlite::Error::InvalidColumnName(..) | rusqlite::Error::InvalidColumnIndex(..) => {
                "SQLITE_INVALID_COLUMN".into()
            }
            rusqlite::Error::InvalidParameterName(..)
            | rusqlite::Error::InvalidParameterCount(..) => "SQLITE_INVALID_PARAMETER".into(),
            // 其他 rusqlite 错误统一收敛，不回声细节。
            _ => "DATABASE_ERROR".into(),
        });
    }
    // 底层 SQLite C 错误码对象：to_string 只含错误码与固定说明。
    if let Some(error) = error.downcast_ref::<rusqlite::ffi::Error>() {
        return Some(error.to_string());
    }
    if let Some(error) = error.downcast_ref::<io::Error>() {
        return Some(error.raw_os_error().map_or_else(
            // 无 errno 时输出错误类别（Debug 表示，内容固定）。
            || format!("I/O 错误：{:?}", error.kind()),
            // 有 errno 时用系统标准文案（"No such file or directory (os error 2)"）。
            |code| io::Error::from_raw_os_error(code).to_string(),
        ));
    }
    if let Some(error) = error.downcast_ref::<serde_json::Error>() {
        return Some(format!(
            // 只输出错误类别与行列号（数值），不含文档内容。
            "JSON 解析失败：{:?}（第 {} 行，第 {} 列）",
            error.classify(),
            error.line(),
            error.column()
        ));
    }
    None
}

/// 功能：为诊断采集当前调用线程的简短堆栈，同时去掉易暴露构建路径的源码行。
/// 入参：无，强制采集当前线程 Backtrace。
/// 返回值：String，先移除 trim 后以 at 开头的行、保留前 48 行，再经 redact 清洗并截到输出上限。
/// 异常：不返回 Result，采集和清洗依赖运行时、正则及线程上下文。
/// 注意事项：采集可能有开销；规则只能减少已知泄露形式，不能把任意堆栈输出视作绝对匿名化。
fn safe_backtrace() -> String {
    // 标准堆栈的源文件行可能含构建机器用户名及带空格路径，整行省略。
    let stack = Backtrace::force_capture().to_string();
    redact::sanitize(
        &stack
            .lines()
            .filter(|line| !line.trim_start().starts_with("at "))
            .take(48)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// 保留安全的错误类别、底层原因、原生错误码及调用方提供的上下文。
///
/// 功能：日志系统最核心的入口。沿错误链最多走 8 层：提取分类码（VeilError
/// log_code、SQLite 错误码、IO 错误码）、原生错误码、冲突资源指纹与可安全
/// 回显的 message/causes；已取消（CANCELLED）降为 Info 级记录。
///
/// 入参：`event` 是调用方提供的事件名；`error` 是错误链起点；`context` 是
/// 脱敏上下文。
///
/// 注意事项：错误链中无法归类的层被跳过，最外层可安全描述的错误成为
/// message，其余进入 causes；绝不回显任意 Display 文本。
#[track_caller]
/// 功能：沿底层错误链收集安全原因和数字错误码，为一次业务失败生成可聚合的结构化日志。
/// 入参：level: Level 为请求等级；component/operation/event: &'static str 为固定组件、动作和事件名；
/// error: &(dyn Error + 'static) 为起始原因；context: &[(&'static str, Field)] 为调用方诊断维度。
/// 返回值：无；最多检查八层，最外层可安全显示原因作 message，其余作 causes，最后识别到的分类作 code。
/// 异常：不返回写盘结果；错误对象自定义 source、TLS 或内部清洗发生的 panic 不在此处统一捕获。
/// 注意事项：event 保持原值，code/native_code 可被更深层来源覆盖；CANCELLED 降为 Info 后还会再次经过等级过滤。
/// 未知包装层不回显原文；日志成功与否不能用来判断原业务提交是否成功，也不会替原操作重试。
pub fn failure(
    level: Level,
    component: &'static str,
    operation: &'static str,
    event: &'static str,
    error: &(dyn Error + 'static),
    context: &[(&'static str, Field<'_>)],
) {
    if !enabled(level) {
        return;
    }
    // 沿错误链逐层下钻：current 是当前层。
    let mut current = Some(error);
    // 分类码从事件名起步，逐层被更具体的分类覆盖。
    let mut code = event;
    // 底层原生错误码（SQLite 扩展码 / OS errno）。
    let mut native_code = None;
    // 可安全回显的 message/causes 列表。
    let mut messages = Vec::new();
    let mut context = fields(context);
    // 最多下钻 8 层，防超长错误链拖慢异常路径。
    for _ in 0..8 {
        let Some(error) = current else { break };
        // VeilError：用业务分类码覆盖。
        if let Some(error) = error.downcast_ref::<crate::error::VeilError>() {
            code = error.log_code();
        }
        // SQLite 错误：提取扩展码并映射分类码。
        if let Some(error) = error.downcast_ref::<rusqlite::Error>() {
            code = match error {
                rusqlite::Error::SqliteFailure(failure, _) => {
                    native_code = Some(i64::from(failure.extended_code));
                    // 忙/锁是最常见的并发信号，单独给稳定码。
                    match failure.code {
                        rusqlite::ErrorCode::DatabaseBusy => "SQLITE_BUSY",
                        rusqlite::ErrorCode::DatabaseLocked => "SQLITE_LOCKED",
                        _ => "DATABASE_ERROR",
                    }
                }
                _ => "DATABASE_ERROR",
            };
        }
        // IO 错误：提取 errno 并按错误类别映射分类码。
        if let Some(error) = error.downcast_ref::<io::Error>() {
            native_code = error.raw_os_error().map(i64::from);
            code = match error.kind() {
                io::ErrorKind::NotFound => "IO_NOT_FOUND",
                io::ErrorKind::PermissionDenied => "IO_PERMISSION_DENIED",
                io::ErrorKind::AlreadyExists => "IO_ALREADY_EXISTS",
                io::ErrorKind::TimedOut => "IO_TIMED_OUT",
                io::ErrorKind::StorageFull => "IO_STORAGE_FULL",
                _ => "IO_ERROR",
            };
        }
        // 锁冲突：把冲突资源 ID 以 16 位指纹记入上下文（不落明文 ID）。
        if let Some(crate::error::VeilError::LockConflict { scope, .. }) =
            error.downcast_ref::<crate::error::VeilError>()
        {
            context.insert(
                "conflicting_resource".into(),
                serde_json::json!(&blake3::hash(scope.as_bytes()).to_hex()[..16]),
            );
        }
        // 包装错误的 Display 可能夹杂任意文本；跳过无法归类的层级，
        // 让最外层可安全描述的错误成为 message，其余进入 causes。
        if let Some(message) = safe_error_message(error)
            && messages.last() != Some(&message)
        {
            messages.push(message);
        }
        // 向下一层：IO 包装错误先看内部源，否则走标准 source 链。
        current = error
            .downcast_ref::<io::Error>()
            .and_then(|e| e.get_ref().map(|e| e as &(dyn Error + 'static)))
            .or_else(|| error.source());
    }
    let location = std::panic::Location::caller();
    // message 取第一层安全描述；全部不可回显时退回分类码。
    let message = messages.first().cloned().unwrap_or_else(|| code.into());
    let details = Details {
        event,
        native_code,
        message,
        // 剩余层进入 causes。
        causes: messages.into_iter().skip(1).collect(),
        context,
        ..Details::default()
    };
    write_record(
        // 用户主动取消是正常流程，降为 Info 级记录。
        if code == "CANCELLED" {
            Level::Info
        } else {
            level
        },
        component,
        operation,
        code,
        location.file(),
        location.line(),
        details,
    );
}

/// 功能：把重复合并窗口中尚未写出的次数补成汇总记录，便于配置切换与导出保留诊断量。
/// 入参：state: &mut State 为已持全局状态锁的日志状态；repeated 每项是一个故障特征对应的代表记录和抑制次数。
/// 返回值：无配置、日志关闭或全部摘要追加成功时 Ok(())。
/// 异常：任一摘要序列化、目录锁、轮转或写入失败返回 io::Error，已成功条目的 count 已归零。
/// 注意事项：只追加摘要，不排空命令队列或对全部日志文件执行 fsync；未成功部分保留待后续尝试，异常退出仍可能丢失。
fn flush_pending(state: &mut State) -> io::Result<()> {
    let Some(config) = &state.config else {
        return Ok(());
    };
    if !config.enabled {
        return Ok(());
    }
    if let Some(repeated) = &mut state.repeated {
        // 每个有抑制计数的条目补写一条汇总记录。
        for entry in repeated.values_mut().filter(|e| e.count > 0) {
            let mut record = entry.record.clone();
            record.suppressed_count = entry.count;
            record.repeat_summary = true;
            append(config, &record)?;
            // 计数清零：已补写。
            entry.count = 0;
        }
    }
    Ok(())
}

/// 功能：在退出、切后台或导出前尽量处理此前排队的日志，并补写重复故障摘要。
/// 入参：无，使用 DRAIN_WAIT 两秒回执等待预算。
/// 返回值：drain 回报成功时 Ok(())，否则返回 TimedOut 类型的 io::Error。
/// 异常：底层任何回执 false/失败都收敛为该错误，不一定只代表真正超时。
/// 注意事项：成功表示这次排空协议完成，不撤回此前已丢弃记录，也不保证文件 fsync 或掉电持久性；新并发入队消息可能排在回执之后。
pub fn flush() -> io::Result<()> {
    if drain(DRAIN_WAIT) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "日志写线程未能及时刷新",
        ))
    }
}

/// 功能：为已按入口规则收敛的日志草稿补齐时间、应用、进程、线程和请求关联信息，再提交写入。
/// 入参：level: Level 为级别；component/operation/code: &str 为内部安全分类；source: &str 与 line: u32 为源码位置；details: Details 为消息、原因、上下文与可选堆栈草稿。
/// 返回值：无；等级允许时创建 privacy_version=1 的 Record 并 enqueue。
/// 异常：不返回存储结果；采集堆栈、TLS/RefCell 等路径可能 panic。
/// 注意事项：此内部函数信任草稿已经安全，不重新清洗所有字符串；panic 总采集堆栈，普通 Error 由配置决定，采集在调用线程完成。
fn write_record(
    level: Level,
    component: &str,
    operation: &str,
    code: &str,
    source: &str,
    line: u32,
    details: Details,
) {
    if !enabled(level) {
        return;
    }
    let mut details = details;
    // 堆栈必须在调用线程采集，写线程的堆栈没有诊断价值。
    if details.backtrace.is_none()
        && (code == "PANIC" || level == Level::Error && CAPTURE_BACKTRACE.load(Ordering::Relaxed))
    {
        details.backtrace = Some(safe_backtrace());
    }
    // 组装落盘形态，信任调用入口已筛选固定文本和字段；这里不再逐字段执行任意字符串清洗。
    let record = Record {
        privacy_version: 1,
        timestamp: chrono::Utc::now().to_rfc3339(),
        level,
        component: component.into(),
        operation: operation.into(),
        code: code.into(),
        native_code: details.native_code,
        version: env!("CARGO_PKG_VERSION").into(),
        platform: std::env::consts::OS.into(),
        process_id: std::process::id(),
        // 源码路径只保留文件名，不泄漏本地目录结构。
        source_file: Path::new(source)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into(),
        source_line: line,
        event: details.event.into(),
        message: details.message,
        causes: details.causes,
        context: details.context,
        request_id: request_id(),
        thread_id: format!("{:?}", std::thread::current().id()),
        suppressed_count: 0,
        repeat_summary: false,
        backtrace: details.backtrace,
    };
    enqueue(record);
}

/// 功能：将诊断记录从业务线程交给独立写线程，缓冲短时间故障峰值。
/// 入参：record: Record 为已组装的单条记录，按值转移所有权。
/// 返回值：无；队列满或断开时丢弃并增加 DROPPED_RECORDS。
/// 异常：不返回写盘错误；首次写线程无法创建时走同步 write_now，而非继续后台重试创建线程。
/// 注意事项：常规路径不等队列容量，但创建线程、记录构建和同步兜底都可能耗时；“异步入口”不等于所有调用都不阻塞。
fn enqueue(record: Record) {
    let Some(sender) = queue() else {
        // 写线程无法启动时尝试同步写入；仍可能因锁竞争、退避或存储失败丢失，健康状态记录其线索。
        write_now(record);
        return;
    };
    // try_send 失败（满/断开）→ 丢弃并计数，供健康状态展示。
    if sender.try_send(Command::Write(Box::new(record))).is_err() {
        DROPPED_RECORDS.fetch_add(1, Ordering::Relaxed);
    }
}

/// 功能：取得日志写线程的命令发送端，在首次使用时尝试建立有界通道与后台消费者。
/// 入参：无，使用进程共享 OnceLock。
/// 返回值：Some(&`SyncSender<Command>`) 为容量 4096 的固定通道，启动失败为 None。
/// 异常：线程创建错误被转换为 None，不返回 io::Error。
/// 注意事项：失败结果也缓存，不会在后续调用中自动重建；通道用于内存缓冲，进程崩溃不能恢复未写消息。
fn queue() -> Option<&'static mpsc::SyncSender<Command>> {
    QUEUE
        .get_or_init(|| {
            // sync_channel：容量 4096 的有界队列（业务线程入队上限）。
            let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
            std::thread::Builder::new()
                .name("veil-log-writer".into())
                .spawn(move || run_writer(receiver))
                .ok()
                .map(|_| sender)
        })
        .as_ref()
}

/// 功能：串行消费日志写入和排空命令，让业务线程不直接承担常规文件轮转工作。
/// 入参：receiver: `mpsc::Receiver<Command>` 为专属接收端，每条消息是一条 Record 或一个 Flush 回执请求。
/// 返回值：通道全部发送端消失后结束循环，无返回业务数据。
/// 异常：每条命令执行放在 catch_unwind 中，Flush 另外捕获摘要刷新 panic 并回执 false；不向业务线程返回原 panic。
/// 注意事项：只能捕获可展开的 Rust panic，不覆盖进程 abort；串行执行保证已入队的先前消息先于 Flush 命令被消费，不保证每条均写盘成功。
fn run_writer(receiver: mpsc::Receiver<Command>) {
    // 登记写线程身份：排空逻辑用它避免"写线程等待自己"。
    let _ = WRITER.set(std::thread::current().id());
    while let Ok(command) = receiver.recv() {
        // 日志自身的 panic 不能终止写线程；刷新命令始终回执，避免调用方空等。
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match command {
            // 写命令：处理一条记录。
            Command::Write(record) => write_now(*record),
            // 排空命令：执行并回执；内部再兜一层 catch，保证 ack 一定发出。
            Command::Flush(ack) => {
                let flushed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(flush_now))
                    .unwrap_or(false);
                let _ = ack.send(flushed);
            }
        }));
    }
}

/// 功能：在有界等待内请求写线程处理先前队列消息并返回刷新结果，供退出或导出收尾。
/// 入参：wait: Duration 为尝试入队 Flush 加等待其回执的总预算。
/// 返回值：收到 true 回执为 true，超时、通道断开或刷新失败为 false；无后台线程或本就在写线程也直接为 true。
/// 异常：不返回 Result，回执接收失败收敛为 false。
/// 注意事项：自身写线程不能等待自己；直接 true 的路径不代表所有日志文件已被同步。
/// 队列满时每毫秒重试，系统调度和其他步骤可能越过预算；Flush 后并发追加的消息不在此次边界内。
fn drain(wait: Duration) -> bool {
    let Some(sender) = queue() else {
        // 没有写线程：没有队列积压可言，视为已排空。
        return true;
    };
    // 日志自身在写线程上 panic 时不能等待自己。
    if WRITER.get() == Some(&std::thread::current().id()) {
        return true;
    }
    let (ack_sender, ack_receiver) = mpsc::channel();
    let started = Instant::now();
    let mut command = Command::Flush(ack_sender);
    loop {
        match sender.try_send(command) {
            // 排空命令已入队：等回执。
            Ok(()) => break,
            // 队列满：退避重试，但总等待不得超过 wait。
            Err(mpsc::TrySendError::Full(returned)) => {
                if started.elapsed() >= wait {
                    return false;
                }
                command = returned;
                std::thread::sleep(Duration::from_millis(1));
            }
            // 写线程已消失：无法排空。
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
        }
    }
    // 回执等待时间扣掉已消耗的入队等待。
    ack_receiver
        .recv_timeout(wait.saturating_sub(started.elapsed()))
        .unwrap_or(false)
}

/// 功能：在写线程或收尾路径补写已合并的故障次数，向排空协议反馈状态。
/// 入参：无，先取得全局日志状态锁。
/// 返回值：取得锁且 flush_pending 成功为 true；锁超时或摘要写入失败为 false。
/// 异常：返回布尔结果，锁失败计入争用，存储失败更新健康计数及退避状态。
/// 注意事项：不重新投递之前丢失的记录，不对文件执行 fsync；调用方若需捕获 panic 应使用 run_writer 的回执包装。
fn flush_now() -> bool {
    // 日志自身发生 panic 时不能等待已持有的锁，其他争用同样有等待上限。
    let mut state = match state_lock() {
        Ok(state) => state,
        Err(_) => {
            CONTENDED_WRITES.fetch_add(1, Ordering::Relaxed);
            return false;
        }
    };
    match flush_pending(&mut state) {
        Ok(()) => true,
        Err(error) => {
            note_failure(&mut state, &error);
            false
        }
    }
}

/// 功能：在消费者线程按当前配置写一条记录，执行级别过滤、故障退避、重复合并、轮转与健康恢复摘要。
/// 入参：record: Record 为单条诊断数据，按值取得所有权；可能被合并为最新代表记录而不立即追加。
/// 返回值：无；不可写时更新健康状态，锁争用时计数并丢弃本条。
/// 异常：默认配置、目录锁、序列化、轮转或写入错误在此转为 note_failure，不以 Result 传播。
/// 注意事项：持 STATE 锁进行相关文件操作，其他状态查询可等待；同类故障按特征键合并，表最多保留 128 键。
/// 退避期间新记录也可能直接计为失败而不保留；一次成功返回不保证该条独立存在于文件。
fn write_now(mut record: Record) {
    let mut state = match state_lock() {
        Ok(state) => state,
        Err(_) => {
            // 拿不到状态锁：计入争用并放弃本条（防日志路径阻塞业务）。
            CONTENDED_WRITES.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    // 配置关闭或等级不足：丢弃。
    if state
        .config
        .as_ref()
        .is_some_and(|config| !config.enabled || record.level < config.minimum_level)
    {
        return;
    }
    // 存储不可用时短暂退避，避免每条故障反复进行文件系统操作。
    if state
        .retry_after
        .is_some_and(|until| Instant::now() < until)
    {
        state.failed_writes = state.failed_writes.saturating_add(1);
        return;
    }
    let result = (|| {
        // 首次写入时初始化默认配置。
        if state.config.is_none() {
            state.config = Some(default_config()?);
        }
        let config = state.config.as_ref().unwrap().clone();
        if !config.enabled || record.level < config.minimum_level {
            return Ok(());
        }
        // 合并窗口非零：启用重复故障合并。
        if !config.repeat_interval.is_zero() {
            // 合并键 = 组件/操作/行号/事件/分类码/原生码/消息 + 关键上下文字段。
            // 同一故障的重复记录会命中同一键，只在窗口内累计计数。
            let key = format!(
                "{}:{}:{}:{}:{}:{:?}:{}:{:?}:{:?}:{:?}:{:?}:{:?}",
                record.component,
                record.operation,
                record.source_line,
                record.event,
                record.code,
                record.native_code,
                record.message,
                record.context.get("database"),
                record.context.get("resource"),
                record.context.get("conflicting_resource"),
                record.context.get("phase"),
                record.context.get("outcome")
            );
            let repeated = state.repeated.get_or_insert_with(HashMap::new);
            if let Some(entry) = repeated.get_mut(&key) {
                // 仍在窗口内：不落盘，仅累计抑制次数并更新代表记录。
                if entry.emitted.elapsed() < config.repeat_interval {
                    entry.count = entry.count.saturating_add(1);
                    entry.record = record;
                    return Ok(());
                }
                // 已过窗口：本条照常写，并带上此前的抑制次数。
                record.suppressed_count = entry.count;
            }
            // 合并表有界（128 键）：超出时淘汰最旧条目。
            if repeated.len() >= 128 && !repeated.contains_key(&key) {
                // 有界缓存；淘汰前保留重复统计。
                if let Some(oldest) = repeated
                    .iter()
                    .min_by_key(|(_, e)| e.emitted)
                    .map(|(k, _)| k.clone())
                {
                    if let Some(old) = repeated.get(&oldest) {
                        if old.count > 0 {
                            // 被淘汰前把欠着的重复次数补写成汇总记录。
                            let mut summary = old.record.clone();
                            summary.repeat_summary = true;
                            summary.suppressed_count = old.count;
                            append(&config, &summary)?;
                        }
                    }
                    repeated.remove(&oldest);
                }
            }
            // 正式落盘本条，并把合并条目（计数清零）放回表。
            append(&config, &record)?;
            repeated.insert(
                key,
                Repeated {
                    emitted: Instant::now(),
                    record,
                    count: 0,
                },
            );
            return Ok(());
        }
        // 关闭合并：直接追加写。
        append(&config, &record)
    })();
    match result {
        Err(error) => note_failure(&mut state, &error),
        Ok(()) => {
            // 写成功：若此前累计失败数超过已上报数，补一条"恢复"记录。
            let total = state
                .failed_writes
                .saturating_add(CONTENDED_WRITES.load(Ordering::Relaxed))
                .saturating_add(DROPPED_RECORDS.load(Ordering::Relaxed));
            if total > state.reported_failures {
                if let Some(config) = &state.config {
                    match append(config, &health_record(&state, "LOGGING_RECOVERED")) {
                        Ok(()) => {
                            state.reported_failures = total;
                            state.retry_after = None;
                        }
                        Err(error) => note_failure(&mut state, &error),
                    }
                }
            }
        }
    }
}

/// 功能：把日志子系统曾丢失或无法写入的数量组装为可展示的健康汇总。
/// 入参：state: &State 为当前健康状态；event: &str 为调用方提供的内部固定事件名。
/// 返回值：Record，Warn 等级、logging/write 归属，context 包含写失败、争用和队列丢弃三类计数。
/// 异常：不执行 I/O，不返回错误。
/// 注意事项：只构造记录，不自动追加或标记“已上报”；请求号为空，数量是累计数而非本次新丢失的单条原因。
fn health_record(state: &State, event: &str) -> Record {
    Record {
        privacy_version: 1,
        timestamp: chrono::Utc::now().to_rfc3339(),
        level: Level::Warn,
        component: "logging".into(),
        operation: "write".into(),
        code: event.into(),
        native_code: None,
        version: env!("CARGO_PKG_VERSION").into(),
        platform: std::env::consts::OS.into(),
        process_id: std::process::id(),
        source_file: "logging.rs".into(),
        source_line: line!(),
        event: event.into(),
        message: "部分诊断记录未能写入".into(),
        causes: Vec::new(),
        // 三类失败计数进上下文：失败/争用/丢弃。
        context: fields(&[
            ("failed_records", Field::Count(state.failed_writes)),
            (
                "contended_records",
                Field::Count(CONTENDED_WRITES.load(Ordering::Relaxed)),
            ),
            (
                "dropped_records",
                Field::Count(DROPPED_RECORDS.load(Ordering::Relaxed)),
            ),
        ]),
        request_id: None,
        thread_id: String::new(),
        suppressed_count: 0,
        repeat_summary: false,
        backtrace: None,
    }
}

/// 功能：为日志正文或锁文件打开私有句柄，减少本地其他用户读取和符号链接跳转风险。
/// 入参：path: &Path 为目标文件；append: bool 为 true 时追加，为 false 时读写打开且不主动截断原内容。
/// 返回值：Ok(File) 为可读写且允许缺失时创建的句柄；Unix 创建模式请求 0600，旧文件权限过宽时也尝试收紧。
/// 异常：打开、元数据读取或权限调整失败返回 io::Error。
/// 注意事项：Unix O_NOFOLLOW 只拒绝最终文件链接，不校验全部祖先；非 Unix 依赖平台访问控制默认行为。
fn open_private(path: &Path, append: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).append(append);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // 0600 仅当前用户读写；O_NOFOLLOW 拒绝符号链接文件。
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // 已存在的文件若权限过宽，收紧为 0600。
        if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(file)
}

/// 功能：保护同一日志目录中的追加、轮转和导出，避免遵守协议的多个进程交错修改文件。
/// 入参：config: &Config 为日志目录和保留策略，调用方应已完成基本配置校验。
/// 返回值：Ok(File) 为已加排他锁的 .lock 句柄，调用方保持存活到本次目录操作完成，关闭后释放锁。
/// 异常：建目录、元数据、权限、打开锁文件或加锁失败返回 I/O 错误；争用超过 500 毫秒或 panic 中等待返回 TimedOut。
/// 注意事项：检查目录最终节点非符号链接，Unix 收紧为 0700；不是对所有父目录或并发路径替换的全面保证。
/// 锁是文件系统协作锁，未遵守本协议的外部修改仍可能影响日志。
fn lock_directory(config: &Config) -> io::Result<File> {
    fs::create_dir_all(&config.directory)?;
    // symlink_metadata 不跟随链接：目录本身是符号链接必须拒绝，
    // 防止日志被写到攻击者指定的位置。
    let metadata = fs::symlink_metadata(&config.directory)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "日志目录不能为符号链接",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // 目录权限收紧为 0700（仅当前用户）。
        if metadata.permissions().mode() & 0o777 != 0o700 {
            fs::set_permissions(&config.directory, fs::Permissions::from_mode(0o700))?;
        }
    }
    let file = open_private(&config.directory.join(".lock"), false)?;
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(fs::TryLockError::Error(error)) => return Err(error),
            Err(fs::TryLockError::WouldBlock) => {
                // 其他进程正持锁：退避重试；panic 中或超时不等待。
                if std::thread::panicking() || started.elapsed() >= LOCK_WAIT {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "日志文件锁等待超时",
                    ));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
    Ok(file)
}

/// 功能：按轮转序号生成当前配置下的一份日志文件位置。
/// 入参：config: &Config 为目录；index: usize 为文件序号，0 表示当前最新文件，较大编号对应较旧轮转内容。
/// 返回值：PathBuf，形如 <directory>/errors-<index>.jsonl。
/// 异常：只拼接路径，不返回错误。
/// 注意事项：不检查 index 是否小于 max_files，不验证文件存在；调用者负责限制访问范围。
fn log_path(config: &Config, index: usize) -> PathBuf {
    config.directory.join(format!("errors-{index}.jsonl"))
}

/// 功能：在跨进程目录锁内把一条 JSONL 日志追加到当前文件，并按容量整理旧文件和轮转。
/// 入参：config: &Config 为已校验策略；record: &Record 为可序列化记录，字段隐私由上游构造入口保证。
/// 返回值：记录编码、保留集合清理、必要轮转及 write_all 成功时为 Ok(())。
/// 异常：单条含换行超过单文件上限为 InvalidInput；序列化、目录锁、枚举、删除、改名、读取末字节或写入失败返回 io::Error。
/// 注意事项：末行缺换行时只补分隔，不修复旧半行内容；失败可能已轮转、删除旧文件或写入部分新行，不提供整体回滚。
/// 调用者通常持 STATE 锁，内部再持目录文件锁；这里只 write_all，不执行文件/目录持久化同步。
fn append(config: &Config, record: &Record) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(record)?;
    // JSONL：每条记录一行，追加换行符。
    bytes.push(b'\n');
    // 单条记录本身不得超过单文件上限。
    if bytes.len() as u64 > config.max_file_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "日志记录超过单文件上限",
        ));
    }
    // 持目录锁直到本次写入完成：与读导出、轮转互斥。
    let _lock = lock_directory(config)?;
    // 配置缩小保留数量后，同样清理旧的轮转文件。
    for entry in fs::read_dir(&config.directory)? {
        let entry = entry?;
        let name = entry.file_name();
        // 解析 errors-N.jsonl 的序号 N；N >= max_files 说明已超出保留数量。
        if let Some(index) = name
            .to_str()
            .and_then(|s| s.strip_prefix("errors-"))
            .and_then(|s| s.strip_suffix(".jsonl"))
            .and_then(|s| s.parse::<usize>().ok())
            && index >= config.max_files
        {
            fs::remove_file(entry.path())?;
        }
    }
    let current = log_path(config, 0);
    let mut file = open_private(&current, true)?;
    let size = file.metadata()?.len();
    // 被中断的写入可能留下半行；分隔后再追加，避免吞掉下一条完整记录。
    // 读最后一个字节判断是否以换行结尾。
    let mut last = [b'\n'];
    if size > 0 {
        file.seek(SeekFrom::End(-1))?;
        file.read_exact(&mut last)?;
    }
    let needs_separator = last[0] != b'\n';
    // 写不下（含可能的补换行）：先轮转再写。
    if size + bytes.len() as u64 + u64::from(needs_separator) > config.max_file_bytes {
        drop(file);
        // 删除最老的一个文件（序号 max_files-1）。
        let oldest = log_path(config, config.max_files - 1);
        if oldest.try_exists()? {
            fs::remove_file(oldest)?;
        }
        // errors-0 → errors-1 → … 整体后移，空出 errors-0。
        for index in (1..config.max_files).rev() {
            let from = log_path(config, index - 1);
            if from.try_exists()? {
                fs::rename(from, log_path(config, index))?;
            }
        }
        file = open_private(&current, true)?;
    } else if needs_separator {
        // 不需要轮转但末尾缺换行：先补一个换行再追加。
        file.write_all(b"\n")?;
    }
    file.write_all(&bytes)
}

/// 旧版把安全描述放在 causes，message 只写占位文案；导出时恢复根因便于阅读。
const LEGACY_PLACEHOLDER: &str = "错误详情已省略（非结构化文本）";
/// 功能：让旧版可解码日志在导出时有可读主消息，兼容历史占位文本。
/// 入参：record: &mut Record 为从 JSONL 读取的单条记录。
/// 返回值：无；占位消息用最后一个 cause 取出回填，没有 cause 则用 code，空消息同样用 code。
/// 异常：内存修改，不返回错误。
/// 注意事项：会从 causes 移除用于回填的一项；不重新脱敏或验证历史字段，不能把规范化当作隐私重新审查。
fn normalize_message(record: &mut Record) {
    if record.message == LEGACY_PLACEHOLDER {
        record.message = record.causes.pop().unwrap_or_else(|| record.code.clone());
    } else if record.message.is_empty() {
        record.message = record.code.clone();
    }
}

/// 功能：读取当前保留的轮转日志和未上报健康汇总，为反馈导出形成一份内存快照。
/// 入参：无；先尝试排空两秒，再持状态锁与目录文件锁读取当前配置的文件。
/// 返回值：Ok(`Vec<Record>`)，按较旧轮转文件到最新文件、每文件原行序追加；无记录可为空，有未上报丢失时末尾加健康记录。
/// 异常：状态锁、默认配置、目录锁、打开或读取失败返回 io::Error；缺失文件跳过，无法解码的 JSONL 行跳过。
/// 注意事项：排空失败及摘要追加失败只记健康状态，仍尽量导出已有记录；每文件最多读取 max_file_bytes，不保证囊括所有历史。
/// 旧版字段按已存内容读取，只修正占位消息，不重新清洗所有字段；快照会占内存，也可能在锁内阻塞其他日志状态操作。
pub fn snapshot() -> io::Result<Vec<Record>> {
    // 先让写线程把队列落盘，否则刚发生的故障不会出现在导出中。
    if !drain(DRAIN_WAIT) {
        DROPPED_RECORDS.fetch_add(1, Ordering::Relaxed);
    }
    let mut state = state_lock()?;
    if state.config.is_none() {
        state.config = Some(default_config()?);
    }
    // 摘要写入失败不能吞掉已经落盘的诊断记录。
    if let Err(error) = flush_pending(&mut state) {
        note_failure(&mut state, &error);
    }
    let config = state.config.as_ref().unwrap();
    let _lock = lock_directory(config)?;
    let mut records = Vec::new();
    // 序号越大文件越旧；倒序枚举编号实际上按旧文件到新文件读取，每个文件内部保留原行顺序。
    for index in (0..config.max_files).rev() {
        let path = log_path(config, index);
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        // 不存在的轮转文件跳过；其他错误向上返回。
        let file = match options.open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        // 只读单文件上限字节数，防异常大文件拖慢导出。
        let mut reader = BufReader::new(file.take(config.max_file_bytes));
        let mut line = Vec::new();
        // 逐行解析 JSONL。
        while reader.read_until(b'\n', &mut line)? > 0 {
            // 进程意外退出可能留下不完整末行；保留其余可解码记录。
            // 旧版记录按存储内容导出，仅修复占位文案与空 message。
            if let Ok(mut record) = serde_json::from_slice::<Record>(&line) {
                normalize_message(&mut record);
                records.push(record);
            }
            // 行缓冲区复用，清空后读下一行。
            line.clear();
        }
    }
    // 有未上报的丢失记录：追加一条健康汇总（含三类计数）。
    if state
        .failed_writes
        .saturating_add(CONTENDED_WRITES.load(Ordering::Relaxed))
        .saturating_add(DROPPED_RECORDS.load(Ordering::Relaxed))
        > state.reported_failures
    {
        records.push(health_record(&state, "LOG_RECORDS_LOST"));
    }
    Ok(records)
}

/// 功能：把日志快照转成便于保存和分享的完整 JSON 文档。
/// 入参：无，通过 snapshot 读取保留记录。
/// 返回值：Ok(`Vec<u8>`)，为格式化 UTF-8 JSON 数组字节，不是逐行 JSONL 文件。
/// 异常：快照读取或 JSON 编码失败以 io::Error 返回。
/// 注意事项：不决定导出路径、不加密导出数据；继承快照的历史字段策略，调用方按反馈流程处理这些诊断资料。
pub fn export() -> io::Result<Vec<u8>> {
    Ok(serde_json::to_vec_pretty(&snapshot()?)?)
}
