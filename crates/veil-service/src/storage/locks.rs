//! 文件级租约，申请、验证、交接均在短写事务内完成。
use super::{container::ContainerDb, id, invalid, json, now, parse};
use crate::{
    error::{Result, VeilError},
    execution::{ExecutionEndpoint, ExecutionOwner, ExecutionServer, ExecutionService, Liveness},
};
use rusqlite::{Connection, TransactionBehavior, params};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
pub use veil_core::locking::LockOptions;
use zeroize::Zeroizing;

pub const LEASE: Duration = Duration::from_secs(30);
pub const RENEW_INTERVAL: Duration = Duration::from_secs(5);
pub const BACKOFF: Duration = Duration::from_millis(20);
pub const DEFAULT_WAIT: Duration = Duration::from_secs(30);
const PROBE_BATCH_BUDGET: Duration = Duration::from_millis(250);
pub fn default_options() -> LockOptions {
    LockOptions {
        timeout: Some(DEFAULT_WAIT),
        cancellation: None,
    }
}
static SERVER: OnceLock<ExecutionServer> = OnceLock::new();
fn endpoint(execution: &ExecutionService) -> Option<ExecutionEndpoint> {
    if SERVER.get().is_none() {
        let server = execution.listen().ok()?;
        let _ = SERVER.set(server);
    }
    SERVER.get().map(|server| server.endpoint().clone())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockMode {
    Read,
    Write,
}
impl LockMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}
#[derive(Clone)]
pub struct LockManager {
    directory: PathBuf,
    password: Arc<Zeroizing<String>>,
    options: LockOptions,
}
#[derive(Clone)]
pub struct Credential {
    pub lock_id: String,
    pub resource_id: String,
    token: String,
    owner: ExecutionOwner,
    issued: Instant,
    wall_issued: i64,
    valid_until: Instant,
}
impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("lock_id", &self.lock_id)
            .field("resource_id", &self.resource_id)
            .finish_non_exhaustive()
    }
}
impl Credential {
    fn local_valid(&self) -> bool {
        let elapsed = self.issued.elapsed().as_millis() as i64;
        Instant::now() < self.valid_until && (now() - self.wall_issued - elapsed).abs() < 1000
    }
}
impl LockManager {
    pub fn new(directory: impl AsRef<Path>, password: &str) -> Self {
        Self {
            directory: directory.as_ref().into(),
            password: Arc::new(Zeroizing::new(password.into())),
            options: default_options(),
        }
    }
    pub fn with_options(mut self, options: LockOptions) -> Self {
        self.options = options;
        self
    }
    pub fn acquire(&self, resources: &[String], mode: LockMode) -> Result<LeaseGuard> {
        let execution = ExecutionService::global()?;
        let owner = execution.runtime().current_context().owner().clone();
        self.acquire_for(resources, mode, owner, endpoint(&execution))
    }
    pub fn acquire_for(
        &self,
        resources: &[String],
        mode: LockMode,
        owner: ExecutionOwner,
        endpoint: Option<ExecutionEndpoint>,
    ) -> Result<LeaseGuard> {
        let mut resources = resources.to_vec();
        resources.sort();
        resources.dedup();
        if resources.iter().any(|r| r.is_empty()) {
            return Err(invalid("文件锁资源不能为空"));
        }
        let start = Instant::now();
        loop {
            self.check_wait(start)?;
            let timeout = self
                .options
                .timeout
                .map(|d| d.saturating_sub(start.elapsed()))
                .unwrap_or(Duration::from_millis(100))
                .min(Duration::from_millis(100));
            match self.try_acquire(&resources, mode, &owner, endpoint.as_ref(), timeout, start) {
                Ok(credentials) => return LeaseGuard::start(self.clone(), credentials),
                Err(VeilError::Database(ref e)) if busy(e) => {}
                Err(VeilError::WorkError(ref e)) if e == "文件正忙" => {}
                Err(e) => return Err(e),
            }
            std::thread::sleep(BACKOFF);
        }
    }
    fn check_wait(&self, start: Instant) -> Result<()> {
        if self
            .options
            .cancellation
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Acquire))
        {
            return Err(VeilError::WorkError("锁等待已取消".into()));
        }
        if self.options.timeout.is_some_and(|d| start.elapsed() >= d) {
            return Err(VeilError::WorkError("锁等待超时".into()));
        }
        Ok(())
    }
    fn try_acquire(
        &self,
        resources: &[String],
        mode: LockMode,
        owner: &ExecutionOwner,
        endpoint: Option<&ExecutionEndpoint>,
        timeout: Duration,
        start: Instant,
    ) -> Result<Vec<Credential>> {
        let mut db =
            ContainerDb::open_with_timeout(&self.directory, &self.password, None, timeout)?;
        let deadline = self.options.timeout.map(|d| start + d);
        let dead = dead_locks_until(
            &db.conn,
            Some(resources),
            deadline,
            self.options.cancellation.as_deref(),
        )?;
        self.check_wait(start)?;
        let tx = db
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        reap(&tx, &dead)?;
        self.check_wait(start)?;
        for resource in resources {
            self.check_wait(start)?;
            let conflicts:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM operation_locks WHERE resource_type='file' AND resource_id=?1 AND (?2='write' OR mode='write'))",params![resource,mode.as_str()],|r|r.get(0))?;
            if conflicts {
                return Err(VeilError::WorkError("文件正忙".into()));
            }
        }
        let time = now();
        let issued = Instant::now();
        let mut credentials = Vec::new();
        let token = id();
        for resource in resources {
            let credential = Credential {
                lock_id: id(),
                resource_id: resource.clone(),
                token: token.clone(),
                owner: owner.clone(),
                issued,
                wall_issued: time,
                valid_until: issued + LEASE,
            };
            tx.execute(
                "INSERT INTO operation_locks VALUES (?1,'file',?2,?3,?4,?5,?6,?7)",
                params![
                    credential.lock_id,
                    resource,
                    mode.as_str(),
                    json(owner)?,
                    endpoint.map(json).transpose()?,
                    credential.token,
                    time + LEASE.as_millis() as i64
                ],
            )?;
            credentials.push(credential);
        }
        self.check_wait(start)?;
        tx.commit()?;
        Ok(credentials)
    }
    pub fn renew(&self, credentials: &mut [Credential]) -> Result<()> {
        let mut db = ContainerDb::open(&self.directory, &self.password, None)?;
        let tx = db
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for c in credentials.iter() {
            validate(&tx, c, None)?;
        }
        let time = now();
        let instant = Instant::now();
        for c in credentials.iter() {
            tx.execute(
                "UPDATE operation_locks SET expires_at=?1 WHERE lock_id=?2 AND owner_token=?3",
                params![time + LEASE.as_millis() as i64, c.lock_id, c.token],
            )?;
        }
        tx.commit()?;
        for c in credentials {
            c.issued = instant;
            c.wall_issued = time;
            c.valid_until = instant + LEASE;
        }
        Ok(())
    }
    pub fn release(&self, credentials: &[Credential]) -> Result<()> {
        let mut db = ContainerDb::open(&self.directory, &self.password, None)?;
        let tx = db
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for c in credentials {
            validate(&tx, c, None)?;
        }
        for c in credentials {
            tx.execute(
                "DELETE FROM operation_locks WHERE lock_id=?1 AND owner_token=?2",
                params![c.lock_id, c.token],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn handoff(
        &self,
        credentials: &[Credential],
        owner: ExecutionOwner,
        endpoint: Option<&ExecutionEndpoint>,
    ) -> Result<Vec<Credential>> {
        let mut db = ContainerDb::open(&self.directory, &self.password, None)?;
        let tx = db
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for c in credentials {
            validate(&tx, c, None)?;
        }
        let mut groups = std::collections::HashMap::new();
        let mut identities = std::collections::HashSet::new();
        for c in credentials {
            if !identities.insert(&c.lock_id) {
                return Err(invalid("交接锁重复"));
            }
            *groups.entry(c.token.as_str()).or_insert(0i64) += 1;
        }
        for (token, count) in groups {
            let total: i64 = tx.query_row(
                "SELECT count(*) FROM operation_locks WHERE owner_token=?1",
                [token],
                |r| r.get(0),
            )?;
            if total != count {
                return Err(invalid("关联锁必须整体交接"));
            }
        }
        let time = now();
        let instant = Instant::now();
        let token = id();
        let mut replacement = Vec::new();
        for c in credentials {
            let mut next = c.clone();
            next.token = token.clone();
            next.owner = owner.clone();
            next.issued = instant;
            next.wall_issued = time;
            next.valid_until = instant + LEASE;
            tx.execute("UPDATE operation_locks SET owner_identity=?1,owner_endpoint=?2,owner_token=?3,expires_at=?4 WHERE lock_id=?5 AND owner_token=?6",params![json(&owner)?,endpoint.map(json).transpose()?,next.token,time+LEASE.as_millis() as i64,c.lock_id,c.token])?;
            replacement.push(next);
        }
        tx.commit()?;
        Ok(replacement)
    }
}
pub(crate) fn validate(conn: &Connection, c: &Credential, mode: Option<LockMode>) -> Result<()> {
    if !c.local_valid() {
        return Err(invalid("文件锁租约已失效，请重新申请"));
    }
    let valid:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM operation_locks WHERE lock_id=?1 AND resource_id=?2 AND owner_token=?3 AND owner_identity=?4 AND expires_at>?5 AND (?6 IS NULL OR mode=?6))",params![c.lock_id,c.resource_id,c.token,json(&c.owner)?,now(),mode.map(LockMode::as_str)],|r|r.get(0))?;
    if !valid {
        return Err(invalid("文件锁凭证已失效"));
    }
    Ok(())
}
pub(crate) fn release_in(conn: &Connection, c: &Credential) -> Result<()> {
    validate(conn, c, None)?;
    conn.execute(
        "DELETE FROM operation_locks WHERE lock_id=?1 AND owner_token=?2",
        params![c.lock_id, c.token],
    )?;
    Ok(())
}
pub(crate) fn dead_locks(conn: &Connection) -> Result<Vec<(String, String)>> {
    dead_locks_until(conn, None, None, None)
}
fn dead_locks_until(
    conn: &Connection,
    resources: Option<&[String]>,
    deadline: Option<Instant>,
    cancellation: Option<&AtomicBool>,
) -> Result<Vec<(String, String)>> {
    let execution = ExecutionService::global()?;
    let start = Instant::now();
    let budget_deadline = start + PROBE_BATCH_BUDGET;
    let deadline = deadline
        .map(|d| d.min(budget_deadline))
        .unwrap_or(budget_deadline);
    let mut stmt=conn.prepare("SELECT lock_id,owner_token,owner_identity,owner_endpoint FROM operation_locks WHERE expires_at>?1 AND (?2 IS NULL OR resource_id IN (SELECT value FROM json_each(?2)))")?;
    let records = stmt
        .query_map(params![now(), resources.map(json).transpose()?], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut dead = Vec::new();
    for (identity, token, owner, endpoint) in records {
        if Instant::now() >= deadline || cancellation.is_some_and(|c| c.load(Ordering::Acquire)) {
            break;
        }
        let owner: ExecutionOwner = parse(&owner)?;
        let endpoint: Option<ExecutionEndpoint> = endpoint.map(|e| parse(&e)).transpose()?;
        let timeout =
            Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now()));
        if execution.check_owner(&owner, endpoint.as_ref(), timeout) == Liveness::Dead {
            dead.push((identity, token));
        }
    }
    Ok(dead)
}
pub(crate) fn reap(conn: &Connection, dead: &[(String, String)]) -> Result<()> {
    conn.execute("DELETE FROM operation_locks WHERE expires_at<=?1", [now()])?;
    for (identity, token) in dead {
        conn.execute(
            "DELETE FROM operation_locks WHERE lock_id=?1 AND owner_token=?2",
            params![identity, token],
        )?;
    }
    Ok(())
}
fn busy(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

/// 守卫应随实际工作移动到阻塞闭包中；等待句柄被丢弃不会释放闭包内的租约。
pub struct LeaseGuard {
    manager: LockManager,
    credentials: Arc<std::sync::Mutex<Vec<Credential>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl LeaseGuard {
    fn start(manager: LockManager, credentials: Vec<Credential>) -> Result<Self> {
        let credentials = Arc::new(std::sync::Mutex::new(credentials));
        let stop = Arc::new(AtomicBool::new(false));
        let shared = credentials.clone();
        let stopped = stop.clone();
        let renewal = manager.clone();
        let worker = ExecutionService::global()?
            .spawn_thread("文件租约续租", move |_| {
                let mut last = Instant::now();
                while !stopped.load(Ordering::Acquire) {
                    std::thread::park_timeout(Duration::from_millis(100));
                    if last.elapsed() >= RENEW_INTERVAL {
                        if let Ok(mut c) = shared.lock() {
                            if renewal.renew(&mut c).is_err() {
                                break;
                            }
                        } else {
                            break;
                        }
                        last = Instant::now();
                    }
                }
            })?
            .handle;
        Ok(Self {
            manager,
            credentials,
            stop,
            worker: Some(worker),
        })
    }
    pub fn credentials(&self) -> Result<Vec<Credential>> {
        self.credentials
            .lock()
            .map(|c| c.clone())
            .map_err(|_| invalid("租约状态不可用"))
    }
    pub fn release(mut self) -> Result<()> {
        self.stop_worker();
        self.manager.release(&self.credentials()?)
    }
    pub fn handoff(
        mut self,
        owner: ExecutionOwner,
        endpoint: Option<&ExecutionEndpoint>,
    ) -> Result<Self> {
        self.stop_worker();
        let next = self
            .manager
            .handoff(&self.credentials()?, owner, endpoint)?;
        if let Ok(mut c) = self.credentials.lock() {
            c.clear();
        }
        Self::start(self.manager.clone(), next)
    }
    fn stop_worker(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}
impl Drop for LeaseGuard {
    fn drop(&mut self) {
        self.stop_worker();
        if let Ok(c) = self.credentials.lock() {
            if !c.is_empty() {
                let _ = self.manager.release(&c);
            }
        }
    }
}
