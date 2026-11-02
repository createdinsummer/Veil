//! service 独占业务表访问；连接与磁盘加密格式由 core 统一提供。
pub mod app;
pub mod container;
pub mod locks;
use crate::error::{Result, VeilError};
use rusqlite::{Connection, TransactionBehavior};
use serde::{Serialize, de::DeserializeOwned};

pub(crate) fn now() -> i64 {
    let time = chrono::Utc::now().timestamp_millis();
    #[cfg(feature = "fault-injection")]
    return time + TEST_CLOCK_OFFSET.load(std::sync::atomic::Ordering::SeqCst);
    #[cfg(not(feature = "fault-injection"))]
    time
}
pub(crate) fn id() -> String {
    veil_core::metadata::generate_version_id()
}
pub(crate) fn json<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| VeilError::SerializationError(e.to_string()))
}
pub(crate) fn parse<T: DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value)
        .map_err(|e| VeilError::InvalidFormat(format!("数据库 JSON 无效: {e}")))
}
pub(crate) fn invalid(message: &str) -> VeilError {
    VeilError::InvalidFormat(message.into())
}

/// 版本检查与建表只允许在本入口的事务内进行，不为业务模块零散补表。
pub(crate) fn migrate(
    conn: &mut Connection,
    application_id: i64,
    schema: &str,
    create: bool,
) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let actual: i64 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    if version == 1 && actual == application_id {
        verify_schema(conn, application_id, schema)?;
        return Ok(());
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let actual: i64 = tx.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    if version == 0 && create {
        let tables: i64 = tx.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table'",
            [],
            |r| r.get(0),
        )?;
        if tables != 0 || actual != 0 {
            return Err(invalid("拒绝初始化已有的未知数据库"));
        }
        tx.execute_batch(schema)?;
        tx.pragma_update(None, "application_id", application_id)?;
        tx.pragma_update(None, "user_version", 1)?;
    } else if version != 1 || actual != application_id {
        return Err(invalid("数据库结构版本或应用标识不受支持"));
    }
    tx.commit()?;
    Ok(())
}

/// 结构版本相同也不能接受缺表、改表或未知触发器；只缓存固定结构，不缓存业务树。
fn verify_schema(conn: &Connection, application_id: i64, schema: &str) -> Result<()> {
    use std::sync::OnceLock;
    static APP: OnceLock<String> = OnceLock::new();
    static CONTAINER: OnceLock<String> = OnceLock::new();
    fn fingerprint(conn: &Connection) -> Result<String> {
        let mut query = conn.prepare("SELECT type,name,tbl_name,sql FROM sqlite_master WHERE name NOT GLOB 'sqlite_*' OR type='trigger' ORDER BY type,name")?;
        let definitions = query
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?
                        .map(|sql| sql.replace("\r\n", "\n")),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(blake3::hash(json(&definitions)?.as_bytes())
            .to_hex()
            .to_string())
    }
    let cached = match application_id {
        0x56415050 => &APP,
        0x56444956 => &CONTAINER,
        _ => return Err(invalid("未知数据库类型")),
    };
    if cached.get().is_none() {
        let template = Connection::open_in_memory()?;
        template.execute_batch(schema)?;
        let _ = cached.set(fingerprint(&template)?);
    }
    if cached.get() != Some(&fingerprint(conn)?) {
        return Err(invalid("数据库结构与声明的版本不一致"));
    }
    Ok(())
}

/// 对尚不存在的目标也解析最近的已存在祖先，统一 /var 与 /private/var 等别名。
pub(crate) fn canonical_path(path: &std::path::Path) -> Result<std::path::PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    loop {
        match std::fs::canonicalize(ancestor) {
            Ok(mut resolved) => {
                for name in suffix.iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                suffix.push(
                    ancestor
                        .file_name()
                        .ok_or_else(|| invalid("路径无效"))?
                        .to_os_string(),
                );
                ancestor = ancestor.parent().ok_or_else(|| invalid("路径无效"))?;
            }
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(feature = "fault-injection")]
static TEST_CLOCK_OFFSET: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
#[cfg(feature = "fault-injection")]
#[doc(hidden)]
pub fn set_test_clock_offset(millis: i64) {
    TEST_CLOCK_OFFSET.store(millis, std::sync::atomic::Ordering::SeqCst);
}
