//! SQLCipher 连接池；密码仅在连接池生命周期内保留，借还连接不重复派生密钥。
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

const MAX_CONNECTIONS: u32 = 16;
const MAX_DATABASES: usize = 32;
type Registry = HashMap<PoolKey, (Arc<Slot>, Instant)>;
fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct PoolKey {
    path: PathBuf,
    authentication: [u8; 32],
    #[cfg(unix)]
    identity: Option<(u64, u64)>,
    #[cfg(not(unix))]
    identity: Option<std::time::SystemTime>,
}
fn canonical(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    loop {
        match std::fs::canonicalize(ancestor) {
            Ok(mut path) => {
                for part in suffix.iter().rev() {
                    path.push(part);
                }
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
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
    fn new(path: &Path, password: Option<&str>) -> Result<Self> {
        static SECRET: OnceLock<Zeroizing<[u8; 32]>> = OnceLock::new();
        let secret = SECRET.get_or_init(|| {
            let mut key = Zeroizing::new([0; 32]);
            getrandom::getrandom(&mut *key).expect("初始化连接池随机密钥");
            key
        });
        let mut input = blake3::Hasher::new_keyed(secret);
        input.update(&[u8::from(password.is_some())]);
        if let Some(password) = password {
            input.update(password.as_bytes());
        }
        let path = canonical(path)?;
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
enum PoolFailure {
    Decryption(String),
    Other(String),
}
fn busy(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}
struct Manager {
    path: PathBuf,
    password: Option<Zeroizing<String>>,
    invalid: Arc<AtomicBool>,
    seed: Mutex<Option<NativeConnection>>,
    failure: Arc<Mutex<Option<PoolFailure>>>,
    reported_failure: AtomicBool,
}
impl ManageConnection for Manager {
    type Connection = NativeConnection;
    type Error = VeilError;
    fn connect(&self) -> Result<NativeConnection> {
        let _secrets = crate::logging::sensitive_scope(
            &self
                .password
                .as_deref()
                .map(|value| value.as_str())
                .into_iter()
                .collect::<Vec<_>>(),
        );
        if self.invalid.load(Ordering::Acquire) {
            return Err(VeilError::DatabasePool("连接池已关闭".into()));
        }
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
                VeilError::DecryptionError(message) => {
                    Some(PoolFailure::Decryption(message.clone()))
                }
                VeilError::Database(error) if busy(error) => None,
                _ => Some(PoolFailure::Other(error.to_string())),
            };
            if let Some(failure) = failure {
                *self.failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(failure);
                self.invalid.store(true, Ordering::Release);
            }
        } else {
            self.reported_failure.store(false, Ordering::Relaxed);
        }
        result
    }
    fn is_valid(&self, conn: &mut NativeConnection) -> Result<()> {
        if self.has_broken(conn) {
            Err(VeilError::DatabasePool("连接不可复用".into()))
        } else {
            Ok(())
        }
    }
    fn has_broken(&self, conn: &mut NativeConnection) -> bool {
        self.invalid.load(Ordering::Acquire) || !conn.is_autocommit()
    }
}
struct Slot {
    failure: Arc<Mutex<Option<PoolFailure>>>,
    pool: Mutex<Option<Pool<Manager>>>,
    invalid: Arc<AtomicBool>,
}
impl Slot {
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
    inner: PooledConnection<Manager>,
}
impl Deref for Connection {
    type Target = NativeConnection;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
impl DerefMut for Connection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        // 手动 BEGIN 的错误路径不能将未结束事务交给下一位借用者。
        if !self.inner.is_autocommit() {
            if let Err(error) = self.inner.execute_batch("ROLLBACK") {
                crate::logging::error_with_code("database", "rollback", "ROLLBACK_FAILED", &error);
            }
        }
    }
}

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
        if slots
            .get(&key)
            .is_some_and(|(s, _)| s.invalid.load(Ordering::Acquire))
        {
            evicted = slots.remove(&key);
        }
        if !slots.contains_key(&key) && slots.len() >= MAX_DATABASES {
            if let Some(oldest) = slots
                .iter()
                // 活跃池不能淘汰，否则并行任务会反复重建同一数据库的连接池。
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
                .min_by_key(|(_, (_, time))| *time)
                .map(|(k, _)| k.clone())
            {
                evicted = slots.remove(&oldest);
            }
        }
        let (slot, time) = slots
            .entry(key.clone())
            .or_insert_with(|| (Arc::new(Slot::new()), Instant::now()));
        *time = Instant::now();
        (slot.clone(), evicted)
    };
    drop(evicted);
    let pool = {
        let mut pool = slot.pool.lock().unwrap_or_else(|e| e.into_inner());
        if pool.is_none() {
            // 首个连接同步校验密码，错误直接返回，不让连接池后台无限重试错误密码。
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
                    .min_idle(Some(0))
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
                Some(PoolFailure::Decryption(message)) => {
                    VeilError::DecryptionError(message.clone())
                }
                Some(PoolFailure::Other(message)) => VeilError::DatabasePool(message.clone()),
                // 连接池资源争用与 SQLite 忙沿用相同的重试语义；不能让目录锁短轮询误报失败。
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
        if matches!(error, VeilError::DecryptionError(_)) {
            invalidate(path);
        }
        return Err(error);
    }
    let mode: String = connection.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    if mode != "delete" {
        return Err(VeilError::InvalidFormat("数据库日志模式不受支持".into()));
    }
    connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON; PRAGMA writable_schema=OFF; PRAGMA ignore_check_constraints=OFF; PRAGMA cipher_memory_security=ON;")?;
    // 首次创建文件后补齐文件身份，使下一次借用直接命中同一个连接池。
    if key.identity.is_none() {
        let current = PoolKey::new(path, password)?;
        if current != key {
            let mut slots = registry().lock().unwrap_or_else(|e| e.into_inner());
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
pub struct PoolScope {
    path: PathBuf,
}
impl PoolScope {
    pub fn new(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        Self {
            path: canonical(path).unwrap_or_else(|_| path.to_owned()),
        }
    }
}
impl Drop for PoolScope {
    fn drop(&mut self) {
        invalidate(&self.path);
    }
}

/// 改密或关闭容器时废弃其连接池；已借出的连接归还后也不会再次使用。
pub fn invalidate(path: &Path) {
    let Ok(path) = canonical(path) else {
        return;
    };
    let removed = {
        let mut slots = registry().lock().unwrap_or_else(|e| e.into_inner());
        let keys = slots
            .keys()
            .filter(|key| key.path == path)
            .cloned()
            .collect::<Vec<_>>();
        keys.into_iter()
            .filter_map(|key| slots.remove(&key).map(|(slot, _)| slot))
            .collect::<Vec<_>>()
    };
    for slot in removed {
        slot.invalid.store(true, Ordering::Release);
        let pool = slot.pool.lock().unwrap_or_else(|e| e.into_inner()).take();
        drop(pool);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn initialized(path: &Path, password: Option<&str>) {
        let conn = database::open(path, password, true).unwrap();
        conn.execute_batch("CREATE TABLE data(value INTEGER); INSERT INTO data VALUES(1)")
            .unwrap();
    }
    #[test]
    fn returning_a_connection_preserves_it_and_rolls_back_unfinished_transactions() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        initialized(&path, None);
        let conn = database::open(&path, None, false).unwrap();
        conn.execute_batch("CREATE TEMP TABLE reused(value); BEGIN; INSERT INTO data VALUES(2)")
            .unwrap();
        let identity = unsafe { conn.handle() } as usize;
        drop(conn);
        let reused = database::open(&path, None, false).unwrap();
        assert_eq!(unsafe { reused.handle() } as usize, identity);
        assert!(reused.is_autocommit());
        assert_eq!(
            reused
                .query_row("SELECT count(*) FROM data", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(reused.execute("INSERT INTO reused VALUES(1)", []).is_ok());
        drop(reused);
        invalidate(&path);
    }
    #[test]
    fn concurrent_borrowers_get_distinct_connections() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        initialized(&path, None);
        let barrier = std::sync::Barrier::new(4);
        let handles = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let path = &path;
                let barrier = &barrier;
                let handles = &handles;
                scope.spawn(move || {
                    let conn = database::open(path, None, false).unwrap();
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
        assert_eq!(handles.len(), 4);
        invalidate(&path);
    }
    #[test]
    fn invalidation_closes_idle_connections() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        initialized(&path, None);
        let conn = database::open(&path, None, false).unwrap();
        conn.execute_batch("CREATE TEMP TABLE old_connection(value)")
            .unwrap();
        drop(conn);
        invalidate(&path);
        let conn = database::open(&path, None, false).unwrap();
        assert!(conn.prepare("SELECT * FROM old_connection").is_err());
        drop(conn);
        invalidate(&path);
    }
    #[test]
    fn cached_connection_rejects_external_rekey_and_accepts_new_password() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        initialized(&path, Some("old"));
        drop(database::open(&path, Some("old"), false).unwrap());
        let external =
            database::open_native(&path, Some("old"), false, database::BUSY_TIMEOUT).unwrap();
        assert_eq!(
            unsafe { rusqlite::ffi::sqlite3_rekey(external.handle(), b"new".as_ptr().cast(), 3) },
            rusqlite::ffi::SQLITE_OK
        );
        drop(external);
        assert!(matches!(
            database::open(&path, Some("old"), false),
            Err(VeilError::DecryptionError(_))
        ));
        let conn = database::open(&path, Some("new"), false).unwrap();
        assert_eq!(
            conn.query_row("SELECT value FROM data", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        database::rekey(&conn, "next").unwrap();
        drop(conn);
        assert!(database::open(&path, Some("new"), false).is_err());
        drop(database::open(&path, Some("next"), false).unwrap());
        invalidate(&path);
    }
    #[test]
    fn a_blocked_database_does_not_delay_other_databases() {
        let root = tempfile::tempdir().unwrap();
        let mut held = Vec::new();
        let mut pools = Vec::new();
        let mut paths = Vec::new();
        for index in 0..8 {
            let path = root.path().join(format!("blocked-{index}"));
            initialized(&path, None);
            let conn = database::open(&path, None, false).unwrap();
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
    #[test]
    fn pool_contention_preserves_sqlite_busy_retry_semantics() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        initialized(&path, None);
        let held = (0..MAX_CONNECTIONS)
            .map(|_| database::open(&path, None, false).unwrap())
            .collect::<Vec<_>>();
        assert!(
            matches!(database::open_with_timeout(&path,None,false,Duration::from_millis(5)),Err(VeilError::Database(error)) if busy(&error))
        );
        drop(held);
        assert!(database::open(&path, None, false).is_ok());
        invalidate(&path);
    }
    #[test]
    fn reused_connections_restore_constraints_and_reject_unsupported_journal_mode() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        initialized(&path, None);
        let conn = database::open(&path, None, false).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA ignore_check_constraints=ON")
            .unwrap();
        drop(conn);
        let conn = database::open(&path, None, false).unwrap();
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
        conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))
            .unwrap();
        drop(conn);
        assert!(matches!(
            database::open(&path, None, false),
            Err(VeilError::InvalidFormat(_))
        ));
        invalidate(&path);
    }
    #[test]
    fn replacement_database_does_not_reuse_an_old_file_descriptor() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("db");
        initialized(&path, None);
        drop(database::open(&path, None, false).unwrap());
        std::fs::rename(&path, root.path().join("old-db")).unwrap();
        let raw = database::open_native(&path, None, true, database::BUSY_TIMEOUT).unwrap();
        raw.execute_batch("CREATE TABLE data(value); INSERT INTO data VALUES(2)")
            .unwrap();
        drop(raw);
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
