//! 统一日志入口。错误消息与错误链在此收敛为可安全导出的描述，业务层不得直接调用日志后端。
//! 正常流程不逐项记录；重复故障合并统计，调用堆栈默认关闭。
//! 写盘由独立线程完成，调用方只做有界入队；导出会排空队列，退出与切后台前必须调用 flush。
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub enabled: bool,
    pub minimum_level: Level,
    pub directory: PathBuf,
    pub max_file_bytes: u64,
    pub max_files: usize,
    /// 相同故障在此窗口内合并；导出时补上累计次数，零表示关闭合并。
    pub repeat_interval: Duration,
    /// 普通错误的堆栈默认关闭，排查时可集中打开。panic 总是采集一次。
    pub capture_backtrace: bool,
}
impl Config {
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    /// 文本策略版本：0 为旧版正则脱敏记录，1 为白名单记录。
    /// 导出按存储内容原样输出，不再过滤旧版文本。
    #[serde(default)]
    pub privacy_version: u8,
    pub timestamp: String,
    pub level: Level,
    pub component: String,
    pub operation: String,
    pub code: String,
    #[serde(default)]
    pub native_code: Option<i64>,
    pub version: String,
    pub platform: String,
    pub process_id: u32,
    pub source_file: String,
    pub source_line: u32,
    #[serde(default)]
    pub event: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub causes: Vec<String>,
    #[serde(default)]
    pub context: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub thread_id: String,
    #[serde(default)]
    pub suppressed_count: u64,
    #[serde(default)]
    pub repeat_summary: bool,
    #[serde(default)]
    pub backtrace: Option<String>,
}

/// 上下文只接受数值、静态标签和资源指纹，禁止传入密码、原始 SQL 参数或文件内容。
#[derive(Clone, Copy)]
pub enum Field<'a> {
    Count(u64),
    Duration(Duration),
    Label(&'static str),
    Resource(&'a str),
    Path(&'a Path),
}
fn fields(values: &[(&'static str, Field<'_>)]) -> BTreeMap<String, serde_json::Value> {
    values
        .iter()
        .take(24)
        .map(|(name, value)| {
            let value = match value {
                Field::Count(value) => serde_json::json!(value),
                Field::Duration(value) => {
                    serde_json::json!(value.as_millis().min(u64::MAX as u128) as u64)
                }
                Field::Label(value) => serde_json::json!(value),
                Field::Resource(value) => {
                    serde_json::json!(&blake3::hash(value.as_bytes()).to_hex()[..16])
                }
                Field::Path(value) => serde_json::json!(
                    &blake3::hash(value.as_os_str().as_encoded_bytes()).to_hex()[..16]
                ),
            };
            ((*name).into(), value)
        })
        .collect()
}

thread_local! { static REQUEST: RefCell<Option<Arc<str>>> = const { RefCell::new(None) }; }
pub struct RequestScope(Option<Arc<str>>);
impl Drop for RequestScope {
    fn drop(&mut self) {
        REQUEST.with(|id| *id.borrow_mut() = self.0.take());
    }
}
pub fn request_id() -> Option<String> {
    REQUEST.with(|id| id.borrow().as_ref().map(|id| id.to_string()))
}
pub fn request_scope() -> RequestScope {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    static INSTANCE: OnceLock<i64> = OnceLock::new();
    let id = request_id().unwrap_or_else(|| {
        format!(
            "{}-{}-{}",
            std::process::id(),
            INSTANCE.get_or_init(|| chrono::Utc::now().timestamp_micros()),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    });
    attach_request(Some(id))
}
/// 工作线程显式继承关联编号，不增加正常日志。
pub fn attach_request(id: Option<String>) -> RequestScope {
    RequestScope(REQUEST.with(|current| current.replace(id.map(Arc::from))))
}

/// 仅在内存中传递，不能 Debug 或序列化（其中包含脱敏所需的密码）。
#[derive(Clone)]
pub struct LogContext {
    request: Option<Arc<str>>,
    secrets: Arc<Vec<zeroize::Zeroizing<String>>>,
}
pub fn capture_context() -> LogContext {
    LogContext {
        request: REQUEST.with(|id| id.borrow().clone()),
        secrets: redact::capture(),
    }
}
impl LogContext {
    pub fn attach(&self) -> (RequestScope, SensitiveScope) {
        (
            RequestScope(REQUEST.with(|current| current.replace(self.request.clone()))),
            redact::attach(self.secrets.clone()),
        )
    }
}

struct Repeated {
    emitted: Instant,
    record: Record,
    count: u64,
}
struct State {
    config: Option<Config>,
    failed_writes: u64,
    last_failure: Option<io::ErrorKind>,
    reported_failures: u64,
    retry_after: Option<Instant>,
    repeated: Option<HashMap<String, Repeated>>,
}
static STATE: Mutex<State> = Mutex::new(State {
    config: None,
    failed_writes: 0,
    last_failure: None,
    reported_failures: 0,
    retry_after: None,
    repeated: None,
});
static MINIMUM_LEVEL: AtomicU8 = AtomicU8::new(Level::Warn as u8);
static CAPTURE_BACKTRACE: AtomicBool = AtomicBool::new(false);
static CONTENDED_WRITES: AtomicU64 = AtomicU64::new(0);
static DROPPED_RECORDS: AtomicU64 = AtomicU64::new(0);
static QUEUE: OnceLock<Option<mpsc::SyncSender<Command>>> = OnceLock::new();
static WRITER: OnceLock<std::thread::ThreadId> = OnceLock::new();
const LOCK_WAIT: Duration = Duration::from_millis(500);
const STATE_WAIT: Duration = Duration::from_millis(500);
const QUEUE_CAPACITY: usize = 4096;
const DRAIN_WAIT: Duration = Duration::from_secs(2);
const PANIC_DRAIN_WAIT: Duration = Duration::from_millis(200);

enum Command {
    Write(Box<Record>),
    Flush(mpsc::Sender<bool>),
}

/// 等级关闭时不遍历错误链、不格式化文本，也不采集资源指纹。
pub fn enabled(level: Level) -> bool {
    level as u8 >= MINIMUM_LEVEL.load(Ordering::Relaxed)
}

fn state_lock() -> io::Result<MutexGuard<'static, State>> {
    let started = Instant::now();
    loop {
        match STATE.try_lock() {
            Ok(state) => return Ok(state),
            Err(std::sync::TryLockError::Poisoned(error)) => return Ok(error.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
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

fn note_failure(state: &mut State, error: &io::Error) {
    state.failed_writes = state.failed_writes.saturating_add(1);
    state.last_failure = Some(error.kind());
    state.retry_after = Some(Instant::now() + Duration::from_secs(1));
}

pub fn health() -> (u64, Option<io::ErrorKind>) {
    let state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let contended = CONTENDED_WRITES.load(Ordering::Relaxed);
    let dropped = DROPPED_RECORDS.load(Ordering::Relaxed);
    (
        state
            .failed_writes
            .saturating_add(contended)
            .saturating_add(dropped),
        state
            .last_failure
            .or((contended > 0).then_some(io::ErrorKind::TimedOut))
            .or((dropped > 0).then_some(io::ErrorKind::WouldBlock)),
    )
}
pub fn configure(config: Config) -> io::Result<()> {
    validate(&config)?;
    // 先按旧配置排空已入队记录，再切换目录与级别。
    let _ = drain(DRAIN_WAIT);
    let mut state = state_lock()?;
    match flush_pending(&mut state) {
        Ok(()) => state.repeated = None,
        Err(error) => note_failure(&mut state, &error),
    }
    state.retry_after = None;
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
pub fn set_directory(directory: PathBuf) -> io::Result<()> {
    // 先按旧目录排空已入队记录，避免切目录后写错位置。
    let _ = drain(DRAIN_WAIT);
    let mut state = state_lock()?;
    if state
        .config
        .as_ref()
        .is_some_and(|c| c.directory == directory)
    {
        return Ok(());
    }
    let mut config = state
        .config
        .clone()
        .unwrap_or_else(|| Config::new(directory.clone()));
    config.directory = directory;
    if let Err(error) = validate(&config) {
        note_failure(&mut state, &error);
        return Err(error);
    }
    match flush_pending(&mut state) {
        Ok(()) => state.repeated = None,
        Err(error) => note_failure(&mut state, &error),
    }
    state.retry_after = None;
    state.config = Some(config);
    Ok(())
}
fn validate(config: &Config) -> io::Result<()> {
    if !config.directory.is_absolute()
        || !(4096..=16 * 1024 * 1024).contains(&config.max_file_bytes)
        || !(1..=32).contains(&config.max_files)
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "日志配置无效"));
    }
    Ok(())
}
fn default_config() -> io::Result<Config> {
    let directory = if let Some(path) = std::env::var_os("VEIL_APP_DATA_DIR") {
        PathBuf::from(path)
    } else {
        dirs::data_local_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "无法定位日志目录"))?
            .join("Veil")
    };
    let config = Config::new(directory.join("logs"));
    validate(&config)?;
    Ok(config)
}

pub fn initialize() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
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

#[derive(Default)]
struct Details {
    event: &'static str,
    native_code: Option<i64>,
    message: String,
    causes: Vec<String>,
    context: BTreeMap<String, serde_json::Value>,
    backtrace: Option<String>,
}
#[track_caller]
pub fn record(level: Level, component: &'static str, operation: &'static str, code: &'static str) {
    event(level, component, operation, code, code, &[]);
}
#[track_caller]
pub fn event(
    level: Level,
    component: &'static str,
    operation: &'static str,
    code: &'static str,
    message: &'static str,
    context: &[(&'static str, Field<'_>)],
) {
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
#[track_caller]
pub fn error(component: &'static str, operation: &'static str, code: &'static str) {
    record(Level::Error, component, operation, code);
}

/// 文本仅在 core 中脱敏；ABI 调用方不得自行写入日志后端。
pub fn client_error(category: u32, source: &str, line: u32, native_code: i64, message: &str) {
    if !enabled(Level::Error) {
        return;
    }
    let code = match category {
        1 => "CLIENT_ENCODING_ERROR",
        2 => "CLIENT_DECODING_ERROR",
        3 => "CLIENT_FILE_ERROR",
        4 => "CLIENT_RESPONSE_ERROR",
        5 => "CLIENT_VALIDATION_ERROR",
        _ => "CLIENT_LOCAL_ERROR",
    };
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
    let end = message
        .char_indices()
        .nth(4096)
        .map_or(message.len(), |(offset, _)| offset);
    let mut messages = message[..end]
        .split("\nCaused by: ")
        .take(8)
        .map(|value| match value {
            "编码失败：invalid_value"
            | "解码失败：missing_key"
            | "解码失败：type_mismatch"
            | "解码失败：missing_value"
            | "解码失败：corrupted_data" => value.to_owned(),
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
                .unwrap_or_else(|| code.into()),
        });
    let details = Details {
        event: code,
        native_code: Some(native_code),
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

#[track_caller]
pub fn error_chain(
    component: &'static str,
    operation: &'static str,
    error: &(dyn Error + 'static),
) {
    error_with_code(component, operation, "UNCLASSIFIED_ERROR", error);
}
#[track_caller]
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
fn safe_error_message(error: &(dyn Error + 'static)) -> Option<String> {
    if let Some(error) = error.downcast_ref::<crate::error::VeilError>() {
        use crate::error::VeilError;
        let reason = match error {
            VeilError::InvalidFormat(reason) | VeilError::WorkError(reason) => {
                Some(reason.as_str())
            }
            _ => None,
        };
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
            rusqlite::Error::QueryReturnedNoRows => "SQLITE_NO_ROWS".into(),
            rusqlite::Error::QueryReturnedMoreThanOneRow => "SQLITE_TOO_MANY_ROWS".into(),
            rusqlite::Error::InvalidColumnType(..)
            | rusqlite::Error::FromSqlConversionFailure(..) => "SQLITE_COLUMN_CONVERSION".into(),
            rusqlite::Error::InvalidColumnName(..) | rusqlite::Error::InvalidColumnIndex(..) => {
                "SQLITE_INVALID_COLUMN".into()
            }
            rusqlite::Error::InvalidParameterName(..)
            | rusqlite::Error::InvalidParameterCount(..) => "SQLITE_INVALID_PARAMETER".into(),
            _ => "DATABASE_ERROR".into(),
        });
    }
    if let Some(error) = error.downcast_ref::<rusqlite::ffi::Error>() {
        return Some(error.to_string());
    }
    if let Some(error) = error.downcast_ref::<io::Error>() {
        return Some(error.raw_os_error().map_or_else(
            || format!("I/O 错误：{:?}", error.kind()),
            |code| io::Error::from_raw_os_error(code).to_string(),
        ));
    }
    if let Some(error) = error.downcast_ref::<serde_json::Error>() {
        return Some(format!(
            "JSON 解析失败：{:?}（第 {} 行，第 {} 列）",
            error.classify(),
            error.line(),
            error.column()
        ));
    }
    None
}

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
#[track_caller]
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
    let mut current = Some(error);
    let mut code = event;
    let mut native_code = None;
    let mut messages = Vec::new();
    let mut context = fields(context);
    for _ in 0..8 {
        let Some(error) = current else { break };
        if let Some(error) = error.downcast_ref::<crate::error::VeilError>() {
            code = error.log_code();
        }
        if let Some(error) = error.downcast_ref::<rusqlite::Error>() {
            code = match error {
                rusqlite::Error::SqliteFailure(failure, _) => {
                    native_code = Some(i64::from(failure.extended_code));
                    match failure.code {
                        rusqlite::ErrorCode::DatabaseBusy => "SQLITE_BUSY",
                        rusqlite::ErrorCode::DatabaseLocked => "SQLITE_LOCKED",
                        _ => "DATABASE_ERROR",
                    }
                }
                _ => "DATABASE_ERROR",
            };
        }
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
        current = error
            .downcast_ref::<io::Error>()
            .and_then(|e| e.get_ref().map(|e| e as &(dyn Error + 'static)))
            .or_else(|| error.source());
    }
    let location = std::panic::Location::caller();
    let message = messages.first().cloned().unwrap_or_else(|| code.into());
    let details = Details {
        event,
        native_code,
        message,
        causes: messages.into_iter().skip(1).collect(),
        context,
        ..Details::default()
    };
    write_record(
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

fn flush_pending(state: &mut State) -> io::Result<()> {
    let Some(config) = &state.config else {
        return Ok(());
    };
    if !config.enabled {
        return Ok(());
    }
    if let Some(repeated) = &mut state.repeated {
        for entry in repeated.values_mut().filter(|e| e.count > 0) {
            let mut record = entry.record.clone();
            record.suppressed_count = entry.count;
            record.repeat_summary = true;
            append(config, &record)?;
            entry.count = 0;
        }
    }
    Ok(())
}

/// 正常退出、进入后台或用户导出时排空队列并补写重复故障摘要。
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

/// 调用线程只构造记录并尝试入队，写盘由独立线程完成。
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

fn enqueue(record: Record) {
    let Some(sender) = queue() else {
        // 写线程无法启动时退回同步写入，宁可阻塞也不丢记录。
        write_now(record);
        return;
    };
    if sender.try_send(Command::Write(Box::new(record))).is_err() {
        DROPPED_RECORDS.fetch_add(1, Ordering::Relaxed);
    }
}

fn queue() -> Option<&'static mpsc::SyncSender<Command>> {
    QUEUE
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
            std::thread::Builder::new()
                .name("veil-log-writer".into())
                .spawn(move || run_writer(receiver))
                .ok()
                .map(|_| sender)
        })
        .as_ref()
}

fn run_writer(receiver: mpsc::Receiver<Command>) {
    let _ = WRITER.set(std::thread::current().id());
    while let Ok(command) = receiver.recv() {
        // 日志自身的 panic 不能终止写线程；刷新命令始终回执，避免调用方空等。
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match command {
            Command::Write(record) => write_now(*record),
            Command::Flush(ack) => {
                let flushed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(flush_now))
                    .unwrap_or(false);
                let _ = ack.send(flushed);
            }
        }));
    }
}

/// 有界等待写线程处理完此前入队的记录并补写重复摘要。
fn drain(wait: Duration) -> bool {
    let Some(sender) = queue() else {
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
            Ok(()) => break,
            Err(mpsc::TrySendError::Full(returned)) => {
                if started.elapsed() >= wait {
                    return false;
                }
                command = returned;
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
        }
    }
    ack_receiver
        .recv_timeout(wait.saturating_sub(started.elapsed()))
        .unwrap_or(false)
}

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

fn write_now(mut record: Record) {
    let mut state = match state_lock() {
        Ok(state) => state,
        Err(_) => {
            CONTENDED_WRITES.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
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
        if state.config.is_none() {
            state.config = Some(default_config()?);
        }
        let config = state.config.as_ref().unwrap().clone();
        if !config.enabled || record.level < config.minimum_level {
            return Ok(());
        }
        if !config.repeat_interval.is_zero() {
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
                if entry.emitted.elapsed() < config.repeat_interval {
                    entry.count = entry.count.saturating_add(1);
                    entry.record = record;
                    return Ok(());
                }
                record.suppressed_count = entry.count;
            }
            if repeated.len() >= 128 && !repeated.contains_key(&key) {
                // 有界缓存；淘汰前保留重复统计。
                if let Some(oldest) = repeated
                    .iter()
                    .min_by_key(|(_, e)| e.emitted)
                    .map(|(k, _)| k.clone())
                {
                    if let Some(old) = repeated.get(&oldest) {
                        if old.count > 0 {
                            let mut summary = old.record.clone();
                            summary.repeat_summary = true;
                            summary.suppressed_count = old.count;
                            append(&config, &summary)?;
                        }
                    }
                    repeated.remove(&oldest);
                }
            }
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
        append(&config, &record)
    })();
    match result {
        Err(error) => note_failure(&mut state, &error),
        Ok(()) => {
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

fn open_private(path: &Path, append: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).append(append);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(file)
}

fn lock_directory(config: &Config) -> io::Result<File> {
    fs::create_dir_all(&config.directory)?;
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

fn log_path(config: &Config, index: usize) -> PathBuf {
    config.directory.join(format!("errors-{index}.jsonl"))
}

fn append(config: &Config, record: &Record) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(record)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > config.max_file_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "日志记录超过单文件上限",
        ));
    }
    let _lock = lock_directory(config)?;
    // 配置缩小保留数量后，同样清理旧的轮转文件。
    for entry in fs::read_dir(&config.directory)? {
        let entry = entry?;
        let name = entry.file_name();
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
    let mut last = [b'\n'];
    if size > 0 {
        file.seek(SeekFrom::End(-1))?;
        file.read_exact(&mut last)?;
    }
    let needs_separator = last[0] != b'\n';
    if size + bytes.len() as u64 + u64::from(needs_separator) > config.max_file_bytes {
        drop(file);
        let oldest = log_path(config, config.max_files - 1);
        if oldest.try_exists()? {
            fs::remove_file(oldest)?;
        }
        for index in (1..config.max_files).rev() {
            let from = log_path(config, index - 1);
            if from.try_exists()? {
                fs::rename(from, log_path(config, index))?;
            }
        }
        file = open_private(&current, true)?;
    } else if needs_separator {
        file.write_all(b"\n")?;
    }
    file.write_all(&bytes)
}

/// 旧版把安全描述放在 causes，message 只写占位文案；导出时恢复根因便于阅读。
const LEGACY_PLACEHOLDER: &str = "错误详情已省略（非结构化文本）";
fn normalize_message(record: &mut Record) {
    if record.message == LEGACY_PLACEHOLDER {
        record.message = record.causes.pop().unwrap_or_else(|| record.code.clone());
    } else if record.message.is_empty() {
        record.message = record.code.clone();
    }
}

/// 返回完整轮转集合的快照，供各端导出反馈；读取与写入使用同一个跨进程锁。
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
    for index in (0..config.max_files).rev() {
        let path = log_path(config, index);
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = match options.open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let mut reader = BufReader::new(file.take(config.max_file_bytes));
        let mut line = Vec::new();
        while reader.read_until(b'\n', &mut line)? > 0 {
            // 进程意外退出可能留下不完整末行；保留其余可解码记录。
            // 旧版记录按存储内容导出，仅修复占位文案与空 message。
            if let Ok(mut record) = serde_json::from_slice::<Record>(&line) {
                normalize_message(&mut record);
                records.push(record);
            }
            line.clear();
        }
    }
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

/// 输出 JSON 数据，由调用方的保存或分享入口决定导出位置。
pub fn export() -> io::Result<Vec<u8>> {
    Ok(serde_json::to_vec_pretty(&snapshot()?)?)
}
