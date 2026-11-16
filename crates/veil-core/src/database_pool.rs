//! SQLCipher 连接池；密码仅在连接池生命周期内保留，借还连接不重复派生密钥。
//!
//! 所属业务模块：veil-core 的存储底层。本模块管理"数据库路径 → 连接池"的全局登记
//! 表（Registry）：同一数据库（含密码）的并发请求复用同一个池，避免每次借用都重新
//! 执行昂贵的 SQLCipher KDF（密钥派生函数）解锁；密码只保存在池管理器内，并在池
//! 管理器最终销毁时清零；失效不会强行中断仍被借出的连接。
//!
//! 文件内核心清单：
//! - `PoolKey`：池的全局身份——路径 + 密码哈希 + 文件身份（dev/inode 或创建时间），
//!   防止文件被替换或外部改密后错误复用旧连接。
//! - `Manager`：r2d2 连接管理器，负责创建/校验连接、记录失败原因。
//! - `Slot`/`Registry`：全局登记表的槽位，每个数据库一个槽，支持上限淘汰。
//! - `Connection`：对调用方暴露的池化连接包装，归还时自动回滚未结束事务。
//! - `open`：借出连接的统一入口（首个连接同步校验密码）。
//! - `invalidate`/`PoolScope`：改密、删除、移动数据库前废弃对应连接池。
//!
//! 依赖能力：
//! - `r2d2`：通用连接池框架，管理连接创建、借出、归还与上限。
//! - `database` 模块：`open_native`/`verify_readable` 等原生连接构建步骤。
//! - `zeroize`：密码与密钥哈希的内存清零，降低残留内存泄露风险。
//! - `blake3`/`getrandom`：进程级秘密密钥与密码的键控哈希，密码不直接进键名。
//!
//! 整体业务目标：让并发的流式加解密、元数据事务共享少量已解锁连接；密码错误时
//! 不进入无限重试；改密/删除/移动时撤销旧池的后续复用资格。仍在使用的连接需要
//! 调用方自行协调结束；密码在管理器最终释放时才清零，失效不等于立即擦除全部副本。
use crate::{
    database,
    error::{Result, VeilError},
};
use r2d2::{ManageConnection, Pool, PooledConnection};
use rusqlite::Connection as NativeConnection;
use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

/// 单个数据库连接池允许同时持有的最大连接数。
const MAX_CONNECTIONS: u32 = 16;
/// 全局登记表的淘汰触发阈值；达到 32 个时尝试淘汰最久未用的空闲池。
/// 活跃池不会为满足阈值而被强制移除，因此登记表数量可能暂时超过 32。
const MAX_DATABASES: usize = 32;
/// 全局登记表类型：数据库键 → (池槽位, 最近一次使用时间)。
/// 每个键代表一个"路径 + 密码 + 文件身份"组合，装的是该数据库的连接池槽位，
/// 最近使用时间用于超出上限时挑选可淘汰的池。
type Registry = HashMap<PoolKey, (Arc<Slot>, Instant)>;
/// 功能：取得进程内数据库池登记表，统一协调池的查找、新建和失效。
/// 入参：无。
/// 返回值：进程生命周期内有效的互斥锁引用；锁内映射按数据库身份存放池槽位与最近使用时间。
/// 异常：本函数不返回错误；取得引用并不代表已成功加锁。
/// 注意事项：登记表锁只保护池身份登记，不覆盖借出连接上的 SQL 执行。
fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

/// 连接池的全局身份键：同一数据库 + 同一密码 + 同一文件身份才复用同一个池。
///
/// `authentication` 是"是否带密码 + 密码内容"的键控哈希，密码本身不进键名；
/// `identity` 记录文件的设备/节点（或创建时间），文件被替换或改密后键会变化，
/// 从而阻止复用指向旧文件的连接；同一文件原地改密未必改变文件身份，
/// 仍需借出时的可读性验证识别旧密钥。
#[derive(Clone, Hash, PartialEq, Eq)]
struct PoolKey {
    /// 规范化后的数据库绝对路径。
    path: PathBuf,
    /// 密码的键控哈希：32 字节；无密码与有密码、不同密码得到不同键。
    authentication: [u8; 32],
    /// 文件身份：unix 为 (设备号, inode)，其他平台为文件创建时间；
    /// 文件尚不存在（待创建）时为 None。
    #[cfg(unix)]
    identity: Option<(u64, u64)>,
    /// 文件身份：非 unix 平台使用文件创建时间。
    #[cfg(not(unix))]
    identity: Option<std::time::SystemTime>,
}

/// 把路径规范化为"已存在部分 + 未创建后缀"的绝对路径。
///
/// 功能：数据库文件可能尚不存在（首次创建），`fs::canonicalize` 只对存在的路径
/// 有效，因此逐级向上找到最近的已存在祖先并解析它，再拼回不存在的后缀部分，
/// 得到稳定的绝对路径键。这样同一逻辑路径在不同写法（相对/绝对/含 `..`）下
/// 可减少路径别名导致的重复建池。不存在的后缀原样拼回，不承诺消除其中所有 `..`。
///
/// 入参：`path: &Path`，待打开数据库的路径，可为相对路径或尚不存在的目标。
/// 返回值：`Ok(PathBuf)` 为解析已存在祖先后的绝对路径。
/// 异常：路径绝对化、读取祖先失败时返回 I/O 错误；无法取得路径段或父级时返回 InvalidFormat。
/// 注意事项：解析结果只是本次文件系统快照；之后的文件替换仍需通过文件身份与认证检测。
fn canonical(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut ancestor = absolute.as_path();
    // suffix 收集从最深到最浅的"尚不存在"的路径段，稍后逆序拼回
    let mut suffix = Vec::new();
    loop {
        match std::fs::canonicalize(ancestor) {
            Ok(mut path) => {
                // 找到存在的祖先：把缺失的后缀按从浅到深拼回去
                for part in suffix.iter().rev() {
                    path.push(part);
                }
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 这一段尚不存在：记下段名，向上一层继续找
                suffix.push(
                    ancestor
                        .file_name()
                        .ok_or_else(|| VeilError::InvalidFormat("数据库路径无效".into()))?
                        .to_owned(),
                );
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| VeilError::InvalidFormat("数据库路径无效".into()))?;
            }
            Err(e) => return Err(e.into()),
        }
    }
}
impl PoolKey {
    /// 由路径与可选密码构建池键。
    ///
    /// 功能：计算密码的键控哈希（进程级秘密密钥 + blake3），读取文件身份，产出
    /// 在本进程中区分密码与可读取的文件身份；它不是业务对象的永久 ID。
    /// 入参：`path: &Path` 为数据库路径；`password: Option<&str>` 为解锁口令，None 与空口令不同。
    /// 返回值：带规范化路径、口令摘要及可选文件身份的池查找键。
    /// 异常：路径解析失败返回错误；首次获取进程随机密钥失败会 panic。
    /// 注意事项：读取文件元数据失败时身份为 None；原地改密还需借出阶段验证，不仅依靠此键。
    fn new(path: &Path, password: Option<&str>) -> Result<Self> {
        // 进程级随机秘密：只在本进程生成一次，用来给密码做键控哈希。
        // 键控哈希使攻击者无法离线预计算常见密码的键值枚举连接池。
        static SECRET: OnceLock<Zeroizing<[u8; 32]>> = OnceLock::new();
        let secret = SECRET.get_or_init(|| {
            let mut key = Zeroizing::new([0; 32]);
            getrandom::getrandom(&mut *key).expect("初始化连接池随机密钥");
            key
        });
        let mut input = blake3::Hasher::new_keyed(secret);
        // 是否带密码也参与哈希：无密码与"空密码"是不同状态
        input.update(&[u8::from(password.is_some())]);
        if let Some(password) = password {
            input.update(password.as_bytes());
        }
        let path = canonical(path)?;
        // 文件可能尚不存在；身份缺失会在创建完成后补齐
        let metadata = std::fs::metadata(&path).ok();
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            metadata.map(|m| (m.dev(), m.ino()))
        };
        #[cfg(not(unix))]
        let identity = metadata.and_then(|m| m.created().ok());
        Ok(Self {
            path,
            authentication: *input.finalize().as_bytes(),
            identity,
        })
    }
}
/// 连接池记录的一次性失败原因，供后续借出失败时翻译成具体错误。
enum PoolFailure {
    /// 密码错误/数据损坏：后续借出都按解密错误返回。
    Decryption(String),
    /// 其他失败：如不支持格式；记录文本原因。
    Other(String),
}
/// 功能：为连接池失败分类判断 SQLite 是否暂时被其他读写占用，允许后续安全重试。
/// 入参：error: &rusqlite::Error 为连接创建或数据库访问失败原因。
/// 返回值：SQLite DatabaseBusy/DatabaseLocked 为 true，其他错误为 false。
/// 异常：只读取错误码，不返回错误。
/// 注意事项：不执行重试，也不证明原业务可重放；池是否失效由调用处按错误类别决定。
fn busy(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}
/// r2d2 连接管理器：知道如何创建一条已解锁的数据库连接，并记住失败原因。
struct Manager {
    /// 目标数据库的规范化路径。
    path: PathBuf,
    /// 解锁密码；用 Zeroizing 保证析构时内存清零。密码只在池生命周期内保留。
    password: Option<Zeroizing<String>>,
    /// 失效标记：与所属槽位共享；置真后不再创建新连接，旧连接校验也失败。
    invalid: Arc<AtomicBool>,
    /// 首个连接同步打开后的种子连接：供池里第一条连接直接复用，
    /// 避免重复执行 KDF；取走后为空。
    seed: Mutex<Option<NativeConnection>>,
    /// 首次失败的原因快照：借出失败时翻译给调用方。
    failure: Arc<Mutex<Option<PoolFailure>>>,
    /// 失败是否已上报日志：避免同一原因反复刷屏。
    reported_failure: AtomicBool,
}
impl ManageConnection for Manager {
    type Connection = NativeConnection;
    type Error = VeilError;
    /// 创建一条新的数据库连接（r2d2 在池空闲不足时调用）。
    ///
    /// 功能：先复用同步打开阶段留下的种子连接（省一次 KDF），否则用本管理器保存
    /// 的密码执行完整解锁；失败时把原因存入共享的 `failure` 快照并标记池失效，
    /// 使借出失败时能报告已记录的原因；临时 BUSY/LOCKED 不使整个池失效。
    /// 入参：`self: &Manager`，提供路径、可选密码及池状态，由 r2d2 调用。
    /// 返回值：`Ok(NativeConnection)` 为种子连接或新解锁连接；失败返回 VeilError。
    /// 异常：池失效返回 DatabasePool；新建连接传播数据库、解密、格式或 I/O 错误。
    /// 注意事项：敏感口令登记仅作用于当前线程日志上下文；本方法不执行调用方业务事务。
    fn connect(&self) -> Result<NativeConnection> {
        // 密码是敏感数据：把本管理器密码登记到当前线程的脱敏上下文
        let _secrets = crate::logging::sensitive_scope(
            &self
                .password
                .as_deref()
                .map(|value| value.as_str())
                .into_iter()
                .collect::<Vec<_>>(),
        );
        // 池已失效（改密/删除/密码错误）：拒绝创建任何新连接
        if self.invalid.load(Ordering::Acquire) {
            return Err(VeilError::DatabasePool("连接池已关闭".into()));
        }
        // 有种子连接就直接复用：open() 已同步验证过密码，无需再派生密钥
        if let Some(conn) = self.seed.lock().unwrap_or_else(|e| e.into_inner()).take() {
            return Ok(conn);
        }
        let result = database::open_native(
            &self.path,
            self.password.as_deref().map(|s| s.as_str()),
            false,
            database::BUSY_TIMEOUT,
        );
        if let Err(error) = &result {
            // 同池并发连接都失败时只上报一次日志，避免刷屏
            if !self.reported_failure.swap(true, Ordering::Relaxed) {
                crate::logging::failure(
                    crate::logging::Level::Warn,
                    "database",
                    "pool_connect",
                    "DB_CONNECT_FAILED",
                    error,
                    &[("database", crate::logging::Field::Path(&self.path))],
                );
            }
            let failure = match error {
                // 解密失败 = 密码错误或数据损坏：记录下来，后续借出统一按此报错
                VeilError::DecryptionError(message) => {
                    Some(PoolFailure::Decryption(message.clone()))
                }
                // 临时繁忙不算池级失败：不缓存原因、不失效，下次仍可重试
                VeilError::Database(error) if busy(error) => None,
                _ => Some(PoolFailure::Other(error.to_string())),
            };
            if let Some(failure) = failure {
                *self.failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(failure);
                // 密码错误/格式错误是持久问题：立即失效整个池，杜绝后台无限重试
                self.invalid.store(true, Ordering::Release);
            }
        } else {
            // 成功创建后清掉失败上报标记，供下一次失败重新上报
            self.reported_failure.store(false, Ordering::Relaxed);
        }
        result
    }
    /// 功能：提供 r2d2 的连接有效性检查，拒绝已失效的池或残留事务的连接。
    /// 入参：`self: &Manager` 为所属池；`conn: &mut NativeConnection` 为待检查连接。
    /// 返回值：可继续复用返回 Ok(())，否则返回 DatabasePool。
    /// 异常：这里只检查内存标志和自动提交状态，不实际访问业务表。
    /// 注意事项：本池关闭了借出时的框架检查；open 另行验证可读性，归还检查由 has_broken 承担。
    fn is_valid(&self, conn: &mut NativeConnection) -> Result<()> {
        if self.has_broken(conn) {
            Err(VeilError::DatabasePool("连接不可复用".into()))
        } else {
            Ok(())
        }
    }
    /// 功能：供 r2d2 判断连接归还后是否应丢弃，避免失效或脏事务连接再次借出。
    /// 入参：`self: &Manager` 为池状态；`conn: &mut NativeConnection` 为待归还连接。
    /// 返回值：池已失效或连接不处于自动提交模式时为 true，否则为 false。
    /// 异常：不返回错误，不执行 SQL。
    /// 注意事项：false 只表示此处未发现问题，不代表磁盘内容、密码或模式已通过验证。
    fn has_broken(&self, conn: &mut NativeConnection) -> bool {
        self.invalid.load(Ordering::Acquire) || !conn.is_autocommit()
    }
}
/// 登记表里的一个槽位：一个数据库（键）对应的全部池状态。
///
/// open 执行期间通过 Arc 持有槽位；借出的 Connection 持有 r2d2 池化连接，
/// 并不直接保存槽位。移除登记项不会强制撤销在用连接，失效标记由池管理器共享。
struct Slot {
    /// 首次失败原因快照，与 Manager 共享。
    failure: Arc<Mutex<Option<PoolFailure>>>,
    /// 实际的 r2d2 池；槽位创建时为空，首次借用时惰性构建。
    pool: Mutex<Option<Pool<Manager>>>,
    /// 失效标记：改密/删除/密码错误后置真，与 Manager 共享。
    invalid: Arc<AtomicBool>,
}
impl Slot {
    /// 功能：为首次访问的数据库登记一份尚未建池的状态。
    /// 入参：无。
    /// 返回值：pool 和 failure 都为空、invalid 为 false 的独立槽位。
    /// 异常：不返回业务错误。
    /// 注意事项：这里只分配共享状态；真正打开文件与验证密码在 open 中进行。
    fn new() -> Self {
        Self {
            failure: Arc::new(Mutex::new(None)),
            pool: Mutex::new(None),
            invalid: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// 持有一个独占借用的连接；析构时归还连接池，不关闭健康连接。
/// Deref 保留现有 SQL 和事务接口，连接池类型不会渗入业务模块。
pub struct Connection {
    /// r2d2 的池化连接包装：析构时自动归还到池。
    inner: PooledConnection<Manager>,
}
impl Deref for Connection {
    type Target = NativeConnection;
    /// 功能：让业务查询沿用原生 SQLite 连接接口。
    /// 入参：`self: &Connection`，当前借出的连接包装。
    /// 返回值：与当前包装同生命周期的原生连接共享引用。
    /// 异常：不执行 SQL，不返回错误。
    /// 注意事项：引用不能超过池化连接的借用期；它不会额外借出连接。
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
impl DerefMut for Connection {
    /// 功能：让业务层在独占借用期间开启事务等需要可变引用的操作。
    /// 入参：`self: &mut Connection`，当前连接的独占引用。
    /// 返回值：底层原生连接的可变引用。
    /// 异常：不执行 SQL，不返回错误。
    /// 注意事项：事务须在归还连接前提交或回滚；析构回滚只用于错误路径兜底。
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
impl Drop for Connection {
    /// 功能：归还连接前清理遗留事务，避免下一位业务调用继承未提交状态。
    /// 入参：`self: &mut Connection`，即将析构的连接。
    /// 返回值：无；析构结束后内部池化连接执行归还。
    /// 异常：回滚失败只记日志；仍未退出事务的连接会被 has_broken 拒绝复用。
    /// 注意事项：可能执行同步数据库回滚，不应代替业务显式提交或回滚。
    fn drop(&mut self) {
        // 手动 BEGIN 的错误路径不能将未结束事务交给下一位借用者。
        // 尝试回滚；若仍有事务残留，随后由池的 has_broken 检查将连接丢弃。
        if !self.inner.is_autocommit() {
            if let Err(error) = self.inner.execute_batch("ROLLBACK") {
                crate::logging::error_with_code("database", "rollback", "ROLLBACK_FAILED", &error);
            }
        }
    }
}

/// 借出一条可用的池化连接；必要时创建数据库文件并首次解锁。
///
/// 功能：找到（或新建）该"路径 + 密码 + 文件身份"对应的池槽位；首次借用时同步
/// 打开种子连接验证密码（错误直接返回，不把错误密码交给后台线程无限重试），
/// 之后每次借用都重新认证当前磁盘内容并恢复连接级约束，防止复用外部改密前的
/// 旧连接或已被其他借用者改乱的连接状态。
///
/// 入参：
/// - `path`：数据库文件路径（应用库 app.db 或容器库 veil.db）。
/// - `password`：容器库解锁密码；应用库为 `None`。
/// - `create`：为 true 时允许创建不存在的文件（首次初始化）。
/// - `timeout`：借出连接的最长等待时间；池满时按 SQLite BUSY 语义报错。
///
/// 返回值：`Ok(Connection)` 已验证可读且约束已恢复的连接。
///
/// 注意事项：
/// - 首个连接同步校验密码；借出后的每次认证复用已派生的密钥，不重复执行 KDF。
/// - 改密后旧密码借出会认证失败并自动使池失效，下一次可用新密码重建。
/// 异常：路径/文件访问失败、密码错误、池失效、借出超时、PRAGMA 或认证失败均返回 VeilError；
/// 无已记录失败原因的借出超时映射为 Database(SQLITE_BUSY)，不是密码错误。首次随机数获取失败会 panic。
/// timeout 不是整个函数的总耗时预算：初始化、借出和数据库忙等待分别使用各自等待步骤。
pub(crate) fn open(
    path: &Path,
    password: Option<&str>,
    create: bool,
    timeout: Duration,
) -> Result<Connection> {
    let key = PoolKey::new(path, password)?;
    let (slot, evicted) = {
        let mut slots = registry().lock().unwrap_or_else(|e| e.into_inner());
        let mut evicted = None;
        // 键还在但槽位已失效（如旧密码认证失败）：先移除，随后按当前键重建
        if slots
            .get(&key)
            .is_some_and(|(s, _)| s.invalid.load(Ordering::Acquire))
        {
            evicted = slots.remove(&key);
        }
        // 达到数据库数量上限：尝试淘汰一个"最久未用且无借用者"的池
        if !slots.contains_key(&key) && slots.len() >= MAX_DATABASES {
            if let Some(oldest) = slots
                .iter()
                // 活跃池不能淘汰，否则并行任务会反复重建同一数据库的连接池。
                // 候选条件：槽位没有其他持有者（Arc 强引用只剩登记表这一份），
                // 且池内所有连接都空闲（没有正在使用或被创建中的连接）。
                .filter(|(_, (slot, _))| {
                    if Arc::strong_count(slot) > 1 {
                        return false;
                    }
                    let Ok(pool) = slot.pool.try_lock() else {
                        return false;
                    };
                    pool.as_ref().is_none_or(|pool| {
                        let state = pool.state();
                        state.connections == state.idle_connections
                    })
                })
                // 淘汰最久未用的那个（最近使用时间最小）
                .min_by_key(|(_, (_, time))| *time)
                .map(|(k, _)| k.clone())
            {
                evicted = slots.remove(&oldest);
            }
        }
        let (slot, time) = slots
            .entry(key.clone())
            // 键不存在时新建槽位；同时刷新最近使用时间
            .or_insert_with(|| (Arc::new(Slot::new()), Instant::now()));
        *time = Instant::now();
        (slot.clone(), evicted)
    };
    // 锁已释放后销毁被淘汰的槽位：此刻它已无借用者，池与密码随之清理
    drop(evicted);
    let pool = {
        let mut pool = slot.pool.lock().unwrap_or_else(|e| e.into_inner());
        if pool.is_none() {
            // 首个连接同步校验密码，错误直接返回，不让连接池后台无限重试错误密码。
            // 种子连接随后交给池的第一条连接，省去重复执行 SQLCipher KDF。
            let seed = database::open_native(&key.path, password, create, timeout)?;
            let manager = Manager {
                path: key.path.clone(),
                password: password.map(|p| Zeroizing::new(p.to_owned())),
                invalid: slot.invalid.clone(),
                seed: Mutex::new(Some(seed)),
                failure: slot.failure.clone(),
                reported_failure: AtomicBool::new(false),
            };
            *pool = Some(
                Pool::builder()
                    .max_size(MAX_CONNECTIONS)
                    // 不预建连接：连接都按需创建，密码错误不会产生一堆失败线程
                    .min_idle(Some(0))
                    // 借出时不额外校验：借出后的统一认证由本函数完成
                    .test_on_check_out(false)
                    .build_unchecked(manager),
            );
        }
        pool.as_ref().unwrap().clone()
    };
    let checkout_started = Instant::now();
    let inner = pool
        .get_timeout(timeout.max(Duration::from_millis(1)))
        .map_err(|_| {
            match slot
                .failure
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                // 池里已记录过密码错误：直接给出原样原因
                Some(PoolFailure::Decryption(message)) => {
                    VeilError::DecryptionError(message.clone())
                }
                // 其他已记录失败：按池错误返回
                Some(PoolFailure::Other(message)) => VeilError::DatabasePool(message.clone()),
                // 连接池资源争用与 SQLite 忙沿用相同的重试语义；不能让目录锁短轮询误报失败。
                // 没有失败记录说明只是暂时拿不到连接：伪装成 SQLITE_BUSY 让上层重试。
                None => VeilError::Database(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                    Some("数据库连接池忙，请重试".into()),
                )),
            }
        })
        .inspect_err(|error| {
            let state = pool.state();
            crate::logging::failure(
                // 目录锁的短轮询会重试 BUSY，最终结果由操作边界记录。
                // 临时繁忙只记 Debug，真正的借出失败记 Error。
                if matches!(error, VeilError::Database(error) if busy(error)) {
                    crate::logging::Level::Debug
                } else {
                    crate::logging::Level::Error
                },
                "database",
                "checkout",
                "POOL_CHECKOUT_FAILED",
                error,
                &[
                    ("database", crate::logging::Field::Path(path)),
                    (
                        "elapsed_ms",
                        crate::logging::Field::Duration(checkout_started.elapsed()),
                    ),
                    ("timeout_ms", crate::logging::Field::Duration(timeout)),
                    (
                        "pool_connections",
                        crate::logging::Field::Count(state.connections as u64),
                    ),
                    (
                        "pool_idle",
                        crate::logging::Field::Count(state.idle_connections as u64),
                    ),
                    (
                        "pool_limit",
                        crate::logging::Field::Count(MAX_CONNECTIONS as u64),
                    ),
                ],
            );
        })?;
    let connection = Connection { inner };
    connection.busy_timeout(timeout)?;
    // 丢弃旧页缓存后认证当前磁盘内容，防止复用外部改密前的旧连接。
    // 复用的是已经派生的密钥，不重新执行 SQLCipher KDF。
    connection.execute_batch("PRAGMA shrink_memory; PRAGMA query_only=OFF;")?;
    if let Err(error) = database::verify_readable(&connection) {
        // 认证失败（密码错误/数据损坏）说明磁盘已被改密或损坏：废弃整个池
        if matches!(error, VeilError::DecryptionError(_)) {
            invalidate(path);
        }
        return Err(error);
    }
    // 日志模式也是固定磁盘格式的一部分：被改成 WAL 等模式即拒绝
    let mode: String = connection.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    if mode != "delete" {
        return Err(VeilError::InvalidFormat("数据库日志模式不受支持".into()));
    }
    // 恢复连接级约束：外键检查、完整持久化、安全删除、禁止改结构/跳过检查、内存安全。
    // 复用连接可能被上一个借用者改过这些 PRAGMA，这里统一复位。
    connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON; PRAGMA writable_schema=OFF; PRAGMA ignore_check_constraints=OFF; PRAGMA cipher_memory_security=ON;")?;
    // 首次创建文件后补齐文件身份，使下一次借用直接命中同一个连接池。
    // 创建前 identity 为 None，键不包含文件身份；创建后补上，避免与新键不一致。
    if key.identity.is_none() {
        let current = PoolKey::new(path, password)?;
        if current != key {
            let mut slots = registry().lock().unwrap_or_else(|e| e.into_inner());
            // 确认槽位还是我们创建的这一个（期间没有被并发借用替换），再迁移键
            if slots
                .get(&key)
                .is_some_and(|(entry, _)| Arc::ptr_eq(entry, &slot))
            {
                let entry = slots.remove(&key).unwrap();
                slots.entry(current).or_insert(entry);
            }
        }
    }
    Ok(connection)
}

/// 临时数据库的连接池生命周期，确保删除或移动数据库前释放闲置连接及密码。
///
/// 功能：RAII 作用域守卫——构造时记录目标路径，析构时废弃对应连接池。删除或
/// 移动容器目录前可用它安排作用域结束时撤销池的复用资格；构造本身不失效池。
/// 在用连接与管理器的释放时机取决于剩余引用，不能用此守卫代替业务停机或并发协调。
pub struct PoolScope {
    /// 要失效的数据库路径（已尽量规范化）。
    path: PathBuf,
}
impl PoolScope {
    /// 功能：记录结束临时数据库操作时需要失效的路径。
    /// 入参：`path: impl AsRef<Path>`，临时数据库文件路径。
    /// 返回值：保存规范化路径的 PoolScope；规范化失败时保存原路径。
    /// 异常：构造不返回错误，路径解析失败采用原值。
    /// 注意事项：只有析构时才调用 invalidate；原路径回退不保证届时一定能匹配登记项。
    pub fn new(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        Self {
            path: canonical(path).unwrap_or_else(|_| path.to_owned()),
        }
    }
}
impl Drop for PoolScope {
    /// 功能：在临时数据库作用域结束时撤销对应连接池的后续复用。
    /// 入参：`self: &mut PoolScope`，保存待失效路径的守卫。
    /// 返回值：无。
    /// 异常：invalidate 无错误返回；不能解析路径时静默跳过。
    /// 注意事项：在用连接不被中断，管理器中的密码要等最终释放才清零。
    fn drop(&mut self) {
        invalidate(&self.path);
    }
}

/// 改密或关闭容器时废弃其连接池；已借出的连接归还后也不会再次使用。
///
/// 功能：从全局登记表移除该路径（所有密码版本）的槽位，置失效标记并取出池
/// 对象销毁。已借出的连接不受影响，但归还时会被拒绝复用并关闭。
///
/// 入参：`path` 数据库路径；无法规范化（如已被删除）时静默返回。
///
/// 返回值：无；没有匹配槽位或路径无法规范化时直接结束。
/// 异常：不向调用方返回路径解析错误；互斥锁中毒时取回内部状态继续清理。
/// 注意事项：已删除的路径仍可能由已存在祖先规范化并命中登记项；没有槽位时重复调用无影响。
/// 在用连接不被中断，调用方须协调它们结束；本函数不保证立即清零所有口令副本。
pub fn invalidate(path: &Path) {
    let Ok(path) = canonical(path) else {
        return;
    };
    let removed = {
        let mut slots = registry().lock().unwrap_or_else(|e| e.into_inner());
        // 收集该路径的所有键（同一路径可能有不同密码版本的池，一并废弃）
        let keys = slots
            .keys()
            .filter(|key| key.path == path)
            .cloned()
            .collect::<Vec<_>>();
        keys.into_iter()
            .filter_map(|key| slots.remove(&key).map(|(slot, _)| slot))
            .collect::<Vec<_>>()
    };
    // 每个槽位对应此路径的一个密码/文件身份版本：先置失效，再移除槽位持有的池引用。
    // 其他引用仍可延长池或管理器的生命周期；在用连接归还时才被拒绝复用。
    for slot in removed {
        slot.invalid.store(true, Ordering::Release);
        let pool = slot.pool.lock().unwrap_or_else(|e| e.into_inner()).take();
        drop(pool);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// 功能：为连接复用和事务清理测试创建一份可读的最小数据库夹具。
    /// 入参：path: &Path 为隔离测试数据库位置；password: `Option<&str>` 为加密口令，None 使用普通库。
    /// 返回值：无；建立 data(value INTEGER) 表并插入数值 1。
    /// 异常：打开、建表或插入失败时 unwrap panic，使调用测试失败。
    /// 注意事项：只用于新测试库，不采用 IF NOT EXISTS，不能对已初始化夹具重复执行；连接在方法结束时归还池。
    fn initialized(path: &Path, password: Option<&str>) {
        let conn = database::open(path, password, true).unwrap();
        conn.execute_batch("CREATE TABLE data(value INTEGER); INSERT INTO data VALUES(1)")
            .unwrap();
    }
    /// 验证连接归还后仍可复用，且归还前未结束的事务会被回滚。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn returning_a_connection_preserves_it_and_rolls_back_unfinished_transactions() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        initialized(&path, None);
        let conn = database::open(&path, None, false).unwrap();
        // 开事务插入一行后直接丢弃连接：模拟借用者未提交/未回滚就归还
        conn.execute_batch("CREATE TEMP TABLE reused(value); BEGIN; INSERT INTO data VALUES(2)")
            .unwrap();
        // 记录底层连接指针，稍后验证复用还是同一连接
        let identity = unsafe { conn.handle() } as usize;
        drop(conn);
        let reused = database::open(&path, None, false).unwrap();
        // 归还即复用：同一底层连接，且事务已被自动回滚
        assert_eq!(unsafe { reused.handle() } as usize, identity);
        assert!(reused.is_autocommit());
        assert_eq!(
            reused
                .query_row("SELECT count(*) FROM data", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        // 临时表仍存在：验证复用是同一连接而不是新建
        assert!(reused.execute("INSERT INTO reused VALUES(1)", []).is_ok());
        drop(reused);
        invalidate(&path);
    }
    /// 验证并发借用者各自拿到不同连接，且都能正常读取数据。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn concurrent_borrowers_get_distinct_connections() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        initialized(&path, None);
        // 4 个线程用屏障同步，尽量同时借用连接
        let barrier = std::sync::Barrier::new(4);
        let handles = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let path = &path;
                let barrier = &barrier;
                let handles = &handles;
                scope.spawn(move || {
                    let conn = database::open(path, None, false).unwrap();
                    // 收集每个线程拿到的连接指针
                    handles
                        .lock()
                        .unwrap()
                        .push(unsafe { conn.handle() } as usize);
                    barrier.wait();
                    assert_eq!(
                        conn.query_row("SELECT value FROM data", [], |r| r.get::<_, i64>(0))
                            .unwrap(),
                        1
                    );
                });
            }
        });
        let handles = handles
            .into_inner()
            .unwrap()
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        // 4 个线程拿到 4 条不同连接，而不是共享一条
        assert_eq!(handles.len(), 4);
        invalidate(&path);
    }
    /// 验证 invalidate 后旧连接不再复用，临时表等旧状态随之消失。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn invalidation_closes_idle_connections() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        initialized(&path, None);
        let conn = database::open(&path, None, false).unwrap();
        // 建一张临时表证明这是"旧连接"
        conn.execute_batch("CREATE TEMP TABLE old_connection(value)")
            .unwrap();
        drop(conn);
        // 废弃池后重新借用：必须得到新连接，旧临时表不存在
        invalidate(&path);
        let conn = database::open(&path, None, false).unwrap();
        assert!(conn.prepare("SELECT * FROM old_connection").is_err());
        drop(conn);
        invalidate(&path);
    }
    /// 验证外部改密后，旧密码借出认证失败；换新密码可正常借用并继续改密。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn cached_connection_rejects_external_rekey_and_accepts_new_password() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        initialized(&path, Some("old"));
        // 先借一次建立带旧密码的池，再归还
        drop(database::open(&path, Some("old"), false).unwrap());
        // 模拟其他进程用旧密码打开后把数据库改密为 "new"
        let external =
            database::open_native(&path, Some("old"), false, database::BUSY_TIMEOUT).unwrap();
        assert_eq!(
            unsafe { rusqlite::ffi::sqlite3_rekey(external.handle(), b"new".as_ptr().cast(), 3) },
            rusqlite::ffi::SQLITE_OK
        );
        drop(external);
        // 池里缓存的旧密码连接认证失败：旧密码被拒绝，且池已失效
        assert!(matches!(
            database::open(&path, Some("old"), false),
            Err(VeilError::DecryptionError(_))
        ));
        // 新密码可用：正常借出并读到数据
        let conn = database::open(&path, Some("new"), false).unwrap();
        assert_eq!(
            conn.query_row("SELECT value FROM data", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        // 再用原生接口改密为 "next"，验证 rekey 前后池都能正确切换
        database::rekey(&conn, "next").unwrap();
        drop(conn);
        assert!(database::open(&path, Some("new"), false).is_err());
        drop(database::open(&path, Some("next"), false).unwrap());
        invalidate(&path);
    }
    /// 验证某个数据库的池被占满不会拖慢其他数据库的连接创建。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn a_blocked_database_does_not_delay_other_databases() {
        let root = tempfile::tempdir().unwrap();
        let mut held = Vec::new();
        let mut pools = Vec::new();
        let mut paths = Vec::new();
        // 建立 8 个各自独立、且都因 EXCLUSIVE 锁无法创建新连接的数据库池
        for index in 0..8 {
            let path = root.path().join(format!("blocked-{index}"));
            initialized(&path, None);
            let conn = database::open(&path, None, false).unwrap();
            // 占住独占写锁，让该库的后台连接创建卡在等待
            conn.execute_batch("BEGIN EXCLUSIVE").unwrap();
            let key = PoolKey::new(&path, None).unwrap();
            let slot = registry().lock().unwrap().get(&key).unwrap().0.clone();
            let pool = slot.pool.lock().unwrap().as_ref().unwrap().clone();
            // 触发需要等待此数据库写锁的后台连接创建。
            assert!(pool.get_timeout(Duration::from_millis(1)).is_err());
            held.push(conn);
            pools.push(pool);
            paths.push(path);
        }
        let independent = root.path().join("independent");
        let result =
            database::open_with_timeout(&independent, None, true, Duration::from_millis(300));
        drop(held);
        assert!(result.is_ok(), "独立数据库不能被其他数据库的连接创建阻塞");
        drop(result);
        drop(pools);
        for path in paths {
            invalidate(&path);
        }
        invalidate(&independent);
    }
    /// 验证池满时借出失败表现为 SQLite BUSY 语义，供上层统一重试。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn pool_contention_preserves_sqlite_busy_retry_semantics() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        initialized(&path, None);
        // 借满 MAX_CONNECTIONS 条连接并全部持有，让池无法再借出
        let held = (0..MAX_CONNECTIONS)
            .map(|_| database::open(&path, None, false).unwrap())
            .collect::<Vec<_>>();
        assert!(
            matches!(database::open_with_timeout(&path,None,false,Duration::from_millis(5)),Err(VeilError::Database(error)) if busy(&error))
        );
        // 归还后可正常借用
        drop(held);
        assert!(database::open(&path, None, false).is_ok());
        invalidate(&path);
    }
    /// 验证复用连接会恢复连接级约束，且不支持的日志模式会被拒绝。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn reused_connections_restore_constraints_and_reject_unsupported_journal_mode() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        initialized(&path, None);
        let conn = database::open(&path, None, false).unwrap();
        // 上一个借用者故意关掉外键、打开约束跳过：模拟被改乱的连接
        conn.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA ignore_check_constraints=ON")
            .unwrap();
        drop(conn);
        let conn = database::open(&path, None, false).unwrap();
        // 复用后约束被统一恢复
        assert_eq!(
            conn.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("PRAGMA ignore_check_constraints", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        // 把日志模式改成 WAL：下一次借用必须被拒绝
        conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
            .unwrap();
        drop(conn);
        assert!(matches!(
            database::open(&path, None, false),
            Err(VeilError::InvalidFormat(_))
        ));
        invalidate(&path);
    }
    /// 验证数据库文件被替换后，池不会复用指向旧文件的文件描述符。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn replacement_database_does_not_reuse_an_old_file_descriptor() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        initialized(&path, None);
        // 借一次建立池后归还，池里留有空闲连接
        drop(database::open(&path, None, false).unwrap());
        // 把旧文件改名并新建同名文件：模拟"删除后重新创建"的替换
        std::fs::rename(&path, root.path().join("old-db")).unwrap();
        let raw = database::open_native(&path, None, true, database::BUSY_TIMEOUT).unwrap();
        raw.execute_batch("CREATE TABLE data(value); INSERT INTO data VALUES(2)")
            .unwrap();
        drop(raw);
        // 因文件身份变化，键不同：借用必须落到新文件，读到的是新数据 2
        let conn = database::open(&path, None, false).unwrap();
        assert_eq!(
            conn.query_row("SELECT value FROM data", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        drop(conn);
        invalidate(&path);
    }
}
