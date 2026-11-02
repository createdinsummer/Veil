//! 所有平台共用的 SQLite / SQLCipher 连接及固定磁盘格式。
use crate::error::{Result, VeilError};
use rusqlite::{Connection, OpenFlags};
use std::{path::Path, time::Duration};

pub const CONTAINER_DB: &str = "veil.db";
pub const APP_DB: &str = "app.db";
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(2);

pub fn open(path: &Path, password: Option<&str>, create: bool) -> Result<Connection> {
    open_with_timeout(path, password, create, BUSY_TIMEOUT)
}
pub fn open_with_timeout(
    path: &Path,
    password: Option<&str>,
    create: bool,
    timeout: Duration,
) -> Result<Connection> {
    if !create && (!path.is_file() || path.metadata()?.len() == 0) {
        return Err(VeilError::InvalidFormat(
            "数据库不存在或为空；不支持旧容器格式".into(),
        ));
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(
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
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })
    .map_err(|e| match e.sqlite_error_code() {
        Some(rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt) => {
            VeilError::DecryptionError("密码错误、数据库损坏或格式不受支持".into())
        }
        _ => VeilError::Database(e),
    })?;
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA temp_store=MEMORY; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;")?;
    // 使用回滚日志，便于独立快照和原生 rekey；绝不自行删除 SQLite 日志。
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    if mode != "delete" {
        return Err(VeilError::InvalidFormat("数据库日志模式不受支持".into()));
    }
    Ok(conn)
}

pub fn rekey(conn: &Connection, password: &str) -> Result<()> {
    if password.is_empty() {
        return Err(VeilError::InvalidFormat("密码不能为空".into()));
    }
    let length =
        i32::try_from(password.len()).map_err(|_| VeilError::InvalidFormat("密码过长".into()))?;
    let result =
        unsafe { rusqlite::ffi::sqlite3_rekey(conn.handle(), password.as_ptr().cast(), length) };
    if result != rusqlite::ffi::SQLITE_OK {
        return Err(VeilError::EncryptionError(
            "数据库改密失败，请重新验证密码".into(),
        ));
    }
    Ok(())
}
