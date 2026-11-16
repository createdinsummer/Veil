//! 所有平台共用的 SQLite / SQLCipher 连接及固定磁盘格式。
//!
//! 所属业务模块：veil-core 的存储底层。本文件是所有平台打开数据库的唯一入口：
//! 容器库 veil.db 使用 SQLCipher（SQLite 的加密扩展）整库加密，应用库 app.db 是
//! 普通 SQLite，两者的连接参数、日志模式与解锁验证都在这里统一固定。
//!
//! 文件内核心清单：
//! - 常量：`CONTAINER_DB`/`APP_DB` 数据库文件名、`BUSY_TIMEOUT` 默认忙等待时长。
//! - `BusyRetryPolicy`/`retry_busy`：调用方定义边界、只对可安全重放操作的繁忙重试。
//! - `commit_batch`：同一写事务内执行一批更新的统一提交入口。
//! - `open`/`open_with_timeout`/`open_native`：打开（或创建）数据库连接。
//! - `verify_readable`：通过实际读表验证密码是否正确解锁。
//! - `rekey`：使用 SQLCipher 原生改密能力修改数据库密码。
//!
//! 依赖能力：
//! - `rusqlite`（bundled-sqlcipher 特性）：SQLCipher 4 + OpenSSL 的捆绑构建，
//!   保证各平台加密格式与派生参数一致。
//! - `database_pool` 模块：连接池的借还与失效管理。
//!
//! 整体业务目标：固定磁盘格式（4096 页、256000 次 PBKDF2-HMAC-SHA512、HMAC-SHA512、
//! 零明文头、delete 日志模式），保证任何平台打开的容器都使用相同参数，并让上层
//! 通过"实际读表"确认密码正确，而不是仅仅打开连接。
pub use crate::database_pool::{invalidate, Connection, PoolScope};
use crate::error::{Result, VeilError};
use rusqlite::{Connection as NativeConnection, OpenFlags, Transaction, TransactionBehavior};
use std::{
    path::Path,
    time::{Duration, Instant},
};

/// 容器库（SQLCipher 整库加密）的固定文件名。
pub const CONTAINER_DB: &str = "veil.db";
/// 应用库（普通 SQLite，只存本机登记/设置/缓存，绝不存密码与文件密钥）的固定文件名。
pub const APP_DB: &str = "app.db";
/// 打开连接时 SQLite 默认的忙等待上限：写锁被占时等待最多 2 秒再报错。
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(2);

/// 调用方定义的 SQLite 临时繁忙重试边界。
///
/// 仅用于可安全重新执行的数据库操作。提交结果未知或带有外部副作用的操作，
/// 必须由调用方先确认结果，不能使用此策略直接重放。
#[derive(Debug, Clone, Copy)]
pub struct BusyRetryPolicy {
    /// 首次尝试失败后，最多再执行多少次。
    pub max_retries: u32,
    /// 不再开始下一次尝试的绝对时间。
    pub deadline: Instant,
    /// 相邻两次尝试之间的等待时间。
    pub retry_interval: Duration,
}

/// 对可安全重放的操作，仅在 SQLite 临时繁忙时按调用方策略重试。
///
/// 功能：包装一个可重复执行的数据库操作，遇到"数据库忙/被锁"这类临时冲突时按
/// 策略退避重试；非繁忙错误、重试次数耗尽或超过截止时间则立刻返回原错误。
///
/// 入参：
/// - `policy`：重试边界——最多重试几次、绝对截止时间、每次间隔多久。
/// - `operation: impl FnMut() -> Result<T>`：可安全重放的数据库操作闭包；
///   失败重试不能重复产生不可撤销的业务效果，已回滚的事务更新可以按业务约束重放。
///
/// 返回值：`Ok(T)` 某次尝试成功；`Err` 原样返回第一次或最后一次的失败原因。
///
/// 注意事项：
/// 异常：传播 operation 的非繁忙错误或最后一次繁忙错误；回调 panic 不在此处捕获。
/// - 禁止直接重放提交结果未知或有不可撤销外部副作用的操作，否则可能重复执行。
/// - 即使 deadline 已过，仍会执行首次尝试；它只限制继续重试，不中断进行中的调用。
/// - 截止时间用单调时钟（Instant）衡量，系统时间跳变不会导致无限重试。
pub fn retry_busy<T>(
    policy: BusyRetryPolicy,
    mut operation: impl FnMut() -> Result<T>,
) -> Result<T> {
    let mut retries = 0;
    loop {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) => {
                // 三种情况直接放弃：错误不是临时繁忙、重试次数用完、已到截止时间
                if !is_busy_error(&error)
                    || retries >= policy.max_retries
                    || Instant::now() >= policy.deadline
                {
                    return Err(error);
                }
                let remaining = policy.deadline.saturating_duration_since(Instant::now());
                // 剩余时间已耗尽则不重试；否则把本次睡眠截短到剩余预算，再在醒来后复查。
                if remaining.is_zero() {
                    return Err(error);
                }
                let delay = policy.retry_interval.min(remaining);
                if !delay.is_zero() {
                    std::thread::sleep(delay);
                }
                retries += 1;
                // 睡醒后再核对一次截止时间，防止睡过头后继续无意义重试
                if Instant::now() >= policy.deadline {
                    return Err(error);
                }
            }
        }
    }
}

/// 判断错误是否属于 SQLite 的临时繁忙类错误（可安全重试的两种底层码）。
/// 入参：error: &VeilError 为本次数据库操作失败原因。
/// 返回值：仅 DatabaseBusy 或 DatabaseLocked 返回 true；其他数据库码及非数据库错误返回 false。
/// 异常：只匹配错误类型，不返回错误。
/// 注意事项：分类不表示业务操作一定可重放，仍须由调用方检查事务和外部副作用。
fn is_busy_error(error: &VeilError) -> bool {
    matches!(
        error,
        VeilError::Database(error)
            if matches!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
            )
    )
}

/// 在同一个写事务中执行一批更新。提交开始后的错误需要由调用方核对实际结果。
///
/// 功能：开启一个 IMMEDIATE 写事务，运行调用方的批量更新，提交前执行钩子，最后
/// 统一提交。apply 返回错误时依赖事务析构回滚；提交已开始后的错误按结果未知处理，
/// 不能承诺所有失败都已回滚，也不能替调用方撤销文件系统等外部副作用。
///
/// 入参：
/// - `conn`：可变借用的事务执行连接，不能同时被其他事务复用。
/// - `apply`：在事务对象上执行批量更新的闭包，承载全部业务 SQL。
/// - `before_commit`：提交开始前执行一次的钩子（如先落盘新密文再改数据库引用）。
///
/// 返回值：`Ok(T)` 事务成功提交并返回 `apply` 的产出；`Err` 事务已回滚，或提交
/// 已开始但结果未知（`CommitUncertain`，此时调用方必须重读数据库确认状态）。
///
/// 注意事项：
/// - 业务日志在事务对象释放后才记录，避免日志 I/O 延长数据库临界区。
/// - 提交开始后的错误绝不能当作"一定没提交"处理。
/// 异常：开启事务或 apply 失败传播原错误；commit 失败包装为 CommitUncertain。
/// before_commit 没有错误返回值，其 panic 也不会在此处捕获；外部落盘结果需由调用方核对。
pub fn commit_batch<T>(
    conn: &mut NativeConnection,
    apply: impl FnOnce(&Transaction<'_>) -> Result<T>,
    before_commit: impl FnOnce(),
) -> Result<T> {
    // phase 记录事务推进到哪一步，用于失败时区分"未开始/已回滚/提交结果未知"
    let mut phase = "begin_transaction";
    let result = (|| {
        // IMMEDIATE 立即取得写锁，避免先读后写之间被其他写事务插入
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        phase = "apply_transaction";
        let output = apply(&tx)?;
        // 提交前钩子：例如把新密文持久化到磁盘后再进入数据库提交
        before_commit();
        phase = "commit_transaction";
        tx.commit()
            // 提交已开始：结果未知，转成专用错误让调用方重读权威状态
            .map_err(|error| VeilError::CommitUncertain(std::io::Error::other(error)))?;
        Ok(output)
    })();
    // 事务已释放后才记录错误，避免日志 I/O 延长数据库临界区。
    if let Err(error) = &result {
        let event = match phase {
            "begin_transaction" => "TRANSACTION_BEGIN_FAILED",
            "apply_transaction" => "TRANSACTION_APPLY_FAILED",
            _ => "TRANSACTION_COMMIT_FAILED",
        };
        // 剥开批量导入的包装层，找到真正导致失败的原因用于定级
        let mut cause = error;
        while let VeilError::ImportBatchFailed { source, .. } = cause {
            cause = source;
        }
        let level = match cause {
            // 可预期的业务冲突只记 Info：取消、锁冲突、版本/文件状态竞争
            VeilError::Cancelled
            | VeilError::LockConflict { .. }
            | VeilError::VersionConflict(_)
            | VeilError::FileAlreadyExists(_)
            | VeilError::FileNotFound(_) => crate::logging::Level::Info,
            // 数据库临时繁忙记 Warn：属于并发下的正常现象
            VeilError::Database(error)
                if matches!(
                    error.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
                ) =>
            {
                crate::logging::Level::Warn
            }
            // 其余错误说明存储层出现问题，按 Error 记录
            _ => crate::logging::Level::Error,
        };
        crate::logging::failure(level, "database", phase, event, error, &[]);
    }
    result
}

/// 打开（或创建）Veil 数据库连接，使用默认 2 秒忙等待超时。
///
/// 功能：统一入口之一。校验目标文件存在与否后交给连接池打开，返回一个可复用的
/// 池化连接；密码正确性会在连接建立时通过实际读表验证。
///
/// 入参：
/// - `path`：数据库文件路径，容器库为 veil.db、应用库为 app.db。
/// - `password`：容器库的解锁密码；应用库传 `None`。
/// - `create`：为 true 时允许创建新文件，false 时要求文件必须已存在且非空。
///
/// 返回值：`Ok(Connection)` 已验证可读的连接；`Err` 文件缺失、密码错误或格式不
/// 受支持等（见 `open_native`）。
///
/// 注意事项：`create=false` 时绝不悄悄创建替代数据库，缺失即报错。
/// 异常：传播 open_with_timeout 的 I/O、格式、解密、数据库或连接池错误。
/// 注意事项：path: &Path 不要求固定文件名；password: `Option<&str>` 的 Some 必须非空，None 表示普通库。
pub fn open(path: &Path, password: Option<&str>, create: bool) -> Result<Connection> {
    open_with_timeout(path, password, create, BUSY_TIMEOUT)
}

/// 与 [`open`] 相同，但由调用方指定忙等待与借连接的等待上限。
///
/// 入参：
/// - `path`/`password`/`create`：同 `open`。
/// - `timeout`：连接池借出连接的最长等待时间；池忙时按 SQLite BUSY 语义返回错误。
/// 返回值：Ok(Connection) 为完成可读性和连接约束检查的池化连接。
/// 异常：现有文件缺失或为空、密码为空、格式不符、解密失败、数据库忙或池借出失败均可能返回 VeilError。
/// 注意事项：timeout: Duration 分别用于相关借出/数据库等待步骤，不是函数总耗时硬上限；
/// create=true 允许创建但不保证目标必须是新文件，业务层仍须负责保护已有容器。
pub fn open_with_timeout(
    path: &Path,
    password: Option<&str>,
    create: bool,
    timeout: Duration,
) -> Result<Connection> {
    // 密码是敏感数据：登记到脱敏上下文，确保日志中不会出现明文密码
    if let Some(password) = password {
        crate::logging::remember_sensitive(password);
    }
    // 不允许把"缺失或空文件"当成可打开的旧容器：明确拒绝，不静默新建
    if !create && (!path.is_file() || path.metadata()?.len() == 0) {
        return Err(VeilError::InvalidFormat(
            "数据库不存在或为空；不支持旧容器格式".into(),
        ));
    }
    crate::database_pool::open(path, password, create, timeout)
}

/// 打开底层原生连接（不进连接池），应用全部固定参数并验证可读。
///
/// 功能：为连接池和测试提供"新连接"的构建步骤：打开文件、设置 SQLCipher 密钥与
/// 固定参数、验证密码、固定日志模式与外键等约束。
///
/// 入参：同 `open_with_timeout`，`timeout` 为 SQLite 忙等待时长。
///
/// 返回值：`Ok(NativeConnection)` 完成全部参数设置且验证可读的原生连接。
///
/// 注意事项：
/// - 密码通过 SQLCipher 原生接口传入，绝不拼进 SQL，也不开启 SQL 跟踪。
/// - 固定的磁盘格式参数与 README 一致，任何平台都不得改变。
/// 异常：打开/设置 PRAGMA 失败传播数据库错误；空密码、超长密码、非 SQLCipher 4 或日志模式不符返回 InvalidFormat；
/// 原生设密失败及无法认证的页面返回 DecryptionError。
/// 注意事项：create=true 可能在参数校验完成前创建文件，失败后不会自动删除；单连接独占使用由调用方负责。
pub(crate) fn open_native(
    path: &Path,
    password: Option<&str>,
    create: bool,
    timeout: Duration,
) -> Result<NativeConnection> {
    // NO_MUTEX：连接自身单线程使用；多线程并发由连接池保证各自独立连接
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = NativeConnection::open_with_flags(
        path,
        if create {
            // 允许创建：首次初始化 app.db / 新建容器 veil.db 时使用
            flags | OpenFlags::SQLITE_OPEN_CREATE
        } else {
            flags
        },
    )?;
    conn.busy_timeout(timeout)?;
    if let Some(password) = password {
        // 空密码没有保护意义，直接拒绝
        if password.is_empty() {
            return Err(VeilError::InvalidFormat("密码不能为空".into()));
        }
        // 只接受固定的 SQLCipher 4 构建，避免其他版本格式混入
        let version: String = conn.query_row("PRAGMA cipher_version", [], |r| r.get(0))?;
        if !version.starts_with("4.") {
            return Err(VeilError::InvalidFormat(
                "仅支持固定 SQLCipher 4 格式".into(),
            ));
        }
        let length = i32::try_from(password.len())
            .map_err(|_| VeilError::InvalidFormat("密码过长".into()))?;
        // 通过原生接口传入密码，不将密码插入 SQL，也不启用 SQL 跟踪。
        let result =
            unsafe { rusqlite::ffi::sqlite3_key(conn.handle(), password.as_ptr().cast(), length) };
        if result != rusqlite::ffi::SQLITE_OK {
            return Err(VeilError::DecryptionError("设置数据库密码失败".into()));
        }
        // 固定磁盘格式：4096 字节页、256000 次 PBKDF2-HMAC-SHA512、HMAC-SHA512 认证、
        // 零明文头；cipher_memory_security 让 SQLCipher 清空内存中的明文页
        conn.execute_batch("PRAGMA cipher_page_size=4096; PRAGMA kdf_iter=256000; PRAGMA cipher_use_hmac=ON; PRAGMA cipher_hmac_algorithm=HMAC_SHA512; PRAGMA cipher_kdf_algorithm=PBKDF2_HMAC_SHA512; PRAGMA cipher_plaintext_header_size=0; PRAGMA cipher_memory_security=ON;")?;
    }
    // 实际读表验证解锁：只设置密码不报错不等于密码正确
    verify_readable(&conn)?;
    // 通用约束：外键检查、内存临时存储、完整持久化、安全删除残留页
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA temp_store=MEMORY; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;")?;
    // 使用回滚日志，便于独立快照和原生 rekey；绝不自行删除 SQLite 日志。
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    if mode != "delete" {
        return Err(VeilError::InvalidFormat("数据库日志模式不受支持".into()));
    }
    Ok(conn)
}

/// 通过实际读表确认数据库可用且密码正确。
///
/// 功能：执行一条最小查询，把"不是数据库/数据库损坏"类错误翻译成解密错误——
/// 它们几乎都意味着密码错误或文件损坏，而不是 SQL 执行问题。
/// 入参：conn: &NativeConnection 为已经设置必要解锁参数的连接。
/// 返回值：成功查询 sqlite_master 返回 Ok(())，不返回表数量。
/// 异常：NotADatabase/DatabaseCorrupt 转为 DecryptionError，其他错误按 Database 传播。
/// 注意事项：这里只证明本次系统表查询可读，不扫描所有业务行或全部加密页，也不检查结构版本。
pub(crate) fn verify_readable(conn: &NativeConnection) -> Result<()> {
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })
    .map_err(|e| match e.sqlite_error_code() {
        // SQLCipher 用错误密码读到的页无法通过认证，底层报"不是数据库/损坏"
        Some(rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt) => {
            VeilError::DecryptionError("密码错误、数据库损坏或格式不受支持".into())
        }
        _ => VeilError::Database(e),
    })?;
    Ok(())
}

/// 使用 SQLCipher 原生改密能力修改数据库密码。
///
/// 功能：普通改密的底层实现——只换数据库的密码保护，文件数据密钥与 .enc 密文
/// 保持不变，不重写任何文件内容。
///
/// 入参：
/// - `conn`：已经用旧密码解锁的连接，改密在其上原地执行。
/// - `password`：新密码，不能为空。
///
/// 返回值：`Ok(())` 改密成功；`Err` 密码为空/过长或底层改密失败。
///
/// 注意事项：
/// - 改密前先使该路径的连接池失效，防止其他持有旧密钥的连接继续复用。
/// - 改密失败或提交状态不确定时，调用方必须重新验证数据库状态后再给结果。
/// 异常：空或超长新口令返回 InvalidFormat；sqlite3_rekey 非 SQLITE_OK 返回 EncryptionError。
/// 注意事项：conn: &NativeConnection 必须已正确解锁并由调用方协调写入，pool 失效不会强制关闭其他在用连接；
/// password: &str 的字节长度须可用 i32 表达，此函数不负责调用方原始字符串或其副本的清零。
pub fn rekey(conn: &NativeConnection, password: &str) -> Result<()> {
    crate::logging::remember_sensitive(password);
    if password.is_empty() {
        return Err(VeilError::InvalidFormat("密码不能为空".into()));
    }
    let length =
        i32::try_from(password.len()).map_err(|_| VeilError::InvalidFormat("密码过长".into()))?;
    // 先废弃旧密码对应的连接池：已借出的连接归还后也不会再复用
    if let Some(path) = conn.path() {
        invalidate(Path::new(path));
    }
    let result =
        unsafe { rusqlite::ffi::sqlite3_rekey(conn.handle(), password.as_ptr().cast(), length) };
    if result != rusqlite::ffi::SQLITE_OK {
        return Err(VeilError::EncryptionError(
            "数据库改密失败，请重新验证密码".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// 验证重试只在 SQLite 临时繁忙（BUSY）时发生，其他错误不重放。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    /// 异常：测试准备或断言失败会 panic；场景中预期的错误由测试体捕获或核对，不作为生产错误返回。
    fn retry_busy_retries_only_transient_database_busy() {
        // attempts 记录操作被执行了几次，跨闭包调用共享计数
        let attempts = Cell::new(0);
        let result = retry_busy(
            BusyRetryPolicy {
                max_retries: 2,
                deadline: Instant::now() + Duration::from_secs(1),
                retry_interval: Duration::ZERO,
            },
            || {
                attempts.set(attempts.get() + 1);
                if attempts.get() < 3 {
                    // 前两次模拟 SQLITE_BUSY，第三次才成功
                    Err(VeilError::Database(rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                        None,
                    )))
                } else {
                    Ok(())
                }
            },
        );
        // 两次繁忙后第三次成功：结果 Ok 且恰好执行 3 次
        assert!(result.is_ok());
        assert_eq!(attempts.get(), 3);

        let attempts = Cell::new(0);
        let result = retry_busy(
            BusyRetryPolicy {
                max_retries: 2,
                deadline: Instant::now() + Duration::from_secs(1),
                retry_interval: Duration::ZERO,
            },
            || {
                attempts.set(attempts.get() + 1);
                // 非繁忙错误：无论重试多少次都无意义
                Err::<(), _>(VeilError::InvalidFormat("不可重试".into()))
            },
        );
        // 非繁忙错误立即返回，操作只执行 1 次
        assert!(result.is_err());
        assert_eq!(attempts.get(), 1);
    }

    /// 验证事务原子性：apply 失败整体回滚，且 before_commit 钩子只在提交前触发。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn batch_is_atomic_and_only_signals_when_commit_starts() {
        let mut conn = NativeConnection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE entries (id INTEGER PRIMARY KEY)")
            .unwrap();
        let started = Cell::new(false);
        let result = commit_batch(
            &mut conn,
            |tx| {
                tx.execute("INSERT INTO entries VALUES (1)", [])?;
                // 主键冲突：第二次插入必然失败，验证整体回滚
                tx.execute("INSERT INTO entries VALUES (1)", [])?;
                Ok(())
            },
            || started.set(true),
        );
        // apply 阶段失败：钩子不应执行，两条插入都应被回滚
        assert!(result.is_err());
        assert!(!started.get());
        assert_eq!(
            conn.query_row("SELECT count(*) FROM entries", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        commit_batch(
            &mut conn,
            |tx| {
                tx.execute("INSERT INTO entries VALUES (1), (2)", [])?;
                Ok(())
            },
            || started.set(true),
        )
        .unwrap();
        // 成功后钩子已执行，两条记录都提交
        assert!(started.get());
        assert_eq!(
            conn.query_row("SELECT count(*) FROM entries", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
}
