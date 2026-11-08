//! 所有平台共用的 SQLite / SQLCipher 连接及固定磁盘格式。
pub use crate::database_pool::{invalidate, Connection, PoolScope};
use crate::error::{Result, VeilError};
use rusqlite::{Connection as NativeConnection, OpenFlags, Transaction, TransactionBehavior};
use std::{
    path::Path,
    time::{Duration, Instant},
};

pub const CONTAINER_DB: &str = "veil.db";
pub const APP_DB: &str = "app.db";
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
pub fn retry_busy<T>(
    policy: BusyRetryPolicy,
    mut operation: impl FnMut() -> Result<T>,
) -> Result<T> {
    let mut retries = 0;
    loop {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) => {
                if !is_busy_error(&error)
                    || retries >= policy.max_retries
                    || Instant::now() >= policy.deadline
                {
                    return Err(error);
                }
                let remaining = policy.deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(error);
                }
                let delay = policy.retry_interval.min(remaining);
                if !delay.is_zero() {
                    std::thread::sleep(delay);
                }
                retries += 1;
                if Instant::now() >= policy.deadline {
                    return Err(error);
                }
            }
        }
    }
}

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
pub fn commit_batch<T>(
    conn: &mut NativeConnection,
    apply: impl FnOnce(&Transaction<'_>) -> Result<T>,
    before_commit: impl FnOnce(),
) -> Result<T> {
    let mut phase = "begin_transaction";
    let result = (|| {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        phase = "apply_transaction";
        let output = apply(&tx)?;
        before_commit();
        phase = "commit_transaction";
        tx.commit()
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
        let mut cause = error;
        while let VeilError::ImportBatchFailed { source, .. } = cause {
            cause = source;
        }
        let level = match cause {
            VeilError::Cancelled
            | VeilError::LockConflict { .. }
            | VeilError::VersionConflict(_)
            | VeilError::FileAlreadyExists(_)
            | VeilError::FileNotFound(_) => crate::logging::Level::Info,
            VeilError::Database(error)
                if matches!(
                    error.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
                ) =>
            {
                crate::logging::Level::Warn
            }
            _ => crate::logging::Level::Error,
        };
        crate::logging::failure(level, "database", phase, event, error, &[]);
    }
    result
}

pub fn open(path: &Path, password: Option<&str>, create: bool) -> Result<Connection> {
    open_with_timeout(path, password, create, BUSY_TIMEOUT)
}
pub fn open_with_timeout(
    path: &Path,
    password: Option<&str>,
    create: bool,
    timeout: Duration,
) -> Result<Connection> {
    if let Some(password) = password {
        crate::logging::remember_sensitive(password);
    }
    if !create && (!path.is_file() || path.metadata()?.len() == 0) {
        return Err(VeilError::InvalidFormat(
            "数据库不存在或为空；不支持旧容器格式".into(),
        ));
    }
    crate::database_pool::open(path, password, create, timeout)
}

pub(crate) fn open_native(
    path: &Path,
    password: Option<&str>,
    create: bool,
    timeout: Duration,
) -> Result<NativeConnection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = NativeConnection::open_with_flags(
        path,
        if create {
            flags | OpenFlags::SQLITE_OPEN_CREATE
        } else {
            flags
        },
    )?;
    conn.busy_timeout(timeout)?;
    if let Some(password) = password {
        if password.is_empty() {
            return Err(VeilError::InvalidFormat("密码不能为空".into()));
        }
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
        conn.execute_batch("PRAGMA cipher_page_size=4096; PRAGMA kdf_iter=256000; PRAGMA cipher_use_hmac=ON; PRAGMA cipher_hmac_algorithm=HMAC_SHA512; PRAGMA cipher_kdf_algorithm=PBKDF2_HMAC_SHA512; PRAGMA cipher_plaintext_header_size=0; PRAGMA cipher_memory_security=ON;")?;
    }
    verify_readable(&conn)?;
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA temp_store=MEMORY; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;")?;
    // 使用回滚日志，便于独立快照和原生 rekey；绝不自行删除 SQLite 日志。
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    if mode != "delete" {
        return Err(VeilError::InvalidFormat("数据库日志模式不受支持".into()));
    }
    Ok(conn)
}

pub(crate) fn verify_readable(conn: &NativeConnection) -> Result<()> {
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })
    .map_err(|e| match e.sqlite_error_code() {
        Some(rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt) => {
            VeilError::DecryptionError("密码错误、数据库损坏或格式不受支持".into())
        }
        _ => VeilError::Database(e),
    })?;
    Ok(())
}

pub fn rekey(conn: &NativeConnection, password: &str) -> Result<()> {
    crate::logging::remember_sensitive(password);
    if password.is_empty() {
        return Err(VeilError::InvalidFormat("密码不能为空".into()));
    }
    let length =
        i32::try_from(password.len()).map_err(|_| VeilError::InvalidFormat("密码过长".into()))?;
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

    #[test]
    fn retry_busy_retries_only_transient_database_busy() {
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
                    Err(VeilError::Database(rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                        None,
                    )))
                } else {
                    Ok(())
                }
            },
        );
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
                Err::<(), _>(VeilError::InvalidFormat("不可重试".into()))
            },
        );
        assert!(result.is_err());
        assert_eq!(attempts.get(), 1);
    }

    #[test]
    fn batch_is_atomic_and_only_signals_when_commit_starts() {
        let mut conn = NativeConnection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE entries (id INTEGER PRIMARY KEY)")
            .unwrap();
        let started = Cell::new(false);
        let result = commit_batch(
            &mut conn,
            |tx| {
                tx.execute("INSERT INTO entries VALUES (1)", [])?;
                tx.execute("INSERT INTO entries VALUES (1)", [])?;
                Ok(())
            },
            || started.set(true),
        );
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
        assert!(started.get());
        assert_eq!(
            conn.query_row("SELECT count(*) FROM entries", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
}
