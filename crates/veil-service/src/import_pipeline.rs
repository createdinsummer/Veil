//! 导入流水线：无界收集待处理文件，有界执行，复用现有完成队列及提交事务。
use crate::{
    batch_policy::{BatchPolicy, ListenControl, load_policy},
    comparison::{ComparisonChecks, ComparisonState, FileInput, compare_files},
    error::{Result, VeilError},
    session::{ContainerView, ServiceError, SessionImportAction},
    storage::{container, invalid},
    veil_ops::{AddFileSpec, ImportBatchContext, LockedDirectory, VeilManager},
};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use veil_core::{
    progress,
    queue::{BoundedQueue, Receiver, SendError, Sender, UnboundedQueue},
    version::VersionSnapshot,
};

pub use crate::transfer_conflicts::{ConflictBatch, ConflictView};
use crate::transfer_conflicts::{ConflictQueue, Waiting};
pub(crate) type ImportConflictQueue = ConflictQueue<Work>;

const CHECK_BATCH: usize = 128;
const EXECUTE_LIMIT: usize = 32;
const PROCESS_BATCH: usize = 8;
const WAIT_LIMIT: usize = 128;
const COMPLETED_LIMIT: usize = 8;
const DATABASE_BUSY_RETRY_DELAYS: [Duration; 4] = [
    Duration::from_millis(100),
    Duration::from_millis(200),
    Duration::from_millis(400),
    Duration::from_millis(800),
];

fn is_database_busy(error: &VeilError) -> bool {
    matches!(
        error,
        VeilError::Database(error)
            if matches!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
            )
    )
}

fn retry_database_busy<T>(mut operation: impl FnMut() -> Result<T>) -> Result<T> {
    use veil_core::logging::{self, Field, Level};
    let mut attempt = 0;
    let started = std::time::Instant::now();
    loop {
        match operation() {
            Err(error)
                if is_database_busy(&error) && attempt < DATABASE_BUSY_RETRY_DELAYS.len() =>
            {
                if attempt == 0 {
                    logging::failure(
                        Level::Warn,
                        "service",
                        "import_retry",
                        "IMPORT_RETRY_STARTED",
                        &error,
                        &[
                            ("attempts", Field::Count(1)),
                            (
                                "backoff_ms",
                                Field::Duration(DATABASE_BUSY_RETRY_DELAYS[attempt]),
                            ),
                        ],
                    );
                }
                std::thread::sleep(DATABASE_BUSY_RETRY_DELAYS[attempt]);
                attempt += 1;
            }
            result => {
                if attempt > 0 {
                    let context = [
                        ("retries", Field::Count(attempt as u64)),
                        ("elapsed_ms", Field::Duration(started.elapsed())),
                    ];
                    match &result {
                        Ok(_) => logging::event(
                            Level::Warn,
                            "service",
                            "import_retry",
                            "IMPORT_RETRY_RECOVERED",
                            "数据库忙重试后恢复",
                            &context,
                        ),
                        Err(error) => logging::failure(
                            Level::Error,
                            "service",
                            "import_retry",
                            "IMPORT_RETRY_EXHAUSTED",
                            error,
                            &context,
                        ),
                    }
                }
                return result;
            }
        }
    }
}

struct ImportDirectoryLocks {
    guard: Option<Arc<crate::storage::locks::LeaseGuard>>,
    directories: Vec<LockedDirectory>,
}
impl ImportDirectoryLocks {
    fn acquire(
        manager: &VeilManager,
        password: &str,
        sources: &[String],
        directory: &str,
        progress: &progress::Progress,
        mapped_target: Option<&str>,
    ) -> Result<Self> {
        let mut paths = Vec::new();
        for source in sources {
            let source = Path::new(source);
            if !source.is_dir() {
                continue;
            }
            paths.push(target_prefix(source, directory, mapped_target)?);
        }
        paths.sort();
        paths.dedup();
        if paths.is_empty() {
            return Ok(Self {
                guard: None,
                directories: Vec::new(),
            });
        }
        let db = manager.open_db(password)?;
        let nodes = container::resolve_batch(&db.conn, &paths)?;
        // 文件与目录类型冲突仍交给冲突队列，不能为了加锁提前替换目标。
        paths.retain(|path| {
            nodes
                .get(path)
                .is_some_and(|(node, _, _)| node.kind == "directory")
        });
        drop(db);
        if paths.is_empty() {
            return Ok(Self {
                guard: None,
                directories: Vec::new(),
            });
        }
        let (guard, directories) =
            manager.lock_import_directories(&paths, password, progress.cancellation_flag())?;
        Ok(Self {
            guard: Some(Arc::new(guard)),
            directories,
        })
    }
    fn index(&self, path: &str) -> Option<usize> {
        self.directories
            .iter()
            .enumerate()
            .filter(|(_, directory)| directory.covers(path))
            .max_by_key(|(_, directory)| directory.path_len())
            .map(|(index, _)| index)
    }
    fn scope(
        &self,
        index: Option<usize>,
    ) -> Option<(&crate::storage::locks::LeaseGuard, &LockedDirectory)> {
        self.guard
            .as_deref()
            .zip(index.and_then(|index| self.directories.get(index)))
    }
}

pub(crate) struct Work {
    spec: AddFileSpec,
    retries: u8,
    error: Option<String>,
}
#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    scan_complete: AtomicBool,
    inflight: AtomicUsize,
    failure: Mutex<Option<(String, VeilError)>>,
}
impl Shared {
    fn active(&self) -> bool {
        !self.stop.load(Ordering::Acquire) && progress::check_cancelled().is_ok()
    }
    fn fail(&self, target: String, error: VeilError) {
        self.stop.store(true, Ordering::Release);
        let mut failure = self.failure.lock().unwrap_or_else(|e| e.into_inner());
        if failure.is_none() {
            *failure = Some((target, error));
        }
    }
}
struct ThreadGuard<'a>(&'a Shared);
impl Drop for ThreadGuard<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.stop.store(true, Ordering::Release);
        }
    }
}
fn send<T>(sender: &Sender<T>, item: T, shared: &Shared) -> Result<()> {
    sender
        .send_with(item, || !shared.active())
        .map_err(|error| match error {
            SendError::Cancelled(_) => VeilError::Cancelled,
            _ => invalid("导入队列已关闭"),
        })
}

fn target_prefix(source: &Path, directory: &str, mapped: Option<&str>) -> Result<String> {
    if let Some(target) = mapped {
        return Ok(target.to_owned());
    }
    let name = source
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| invalid("文件名无效"))?;
    Ok(if directory.is_empty() {
        name.to_owned()
    } else {
        format!("{directory}/{name}")
    })
}

fn enumerate(
    sources: &[String],
    directory: &str,
    sender: &Sender<Work>,
    shared: &Shared,
    mapped_target: Option<&str>,
) -> Result<()> {
    for source in sources {
        let root = Path::new(source);
        let prefix = target_prefix(root, directory, mapped_target)?;
        if root.is_file() {
            emit(root, &prefix, sender, shared)?;
        } else if root.is_dir() {
            // 限制遍历器同时打开的目录数量，不一次收集完整目录树。
            for entry in walkdir::WalkDir::new(root).follow_links(false).max_open(8) {
                if !shared.active() {
                    return Err(VeilError::Cancelled);
                }
                let entry = entry.map_err(|error| invalid(&format!("无法遍历目录：{error}")))?;
                if !entry.file_type().is_file() {
                    continue;
                }
                let relative = entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|_| invalid("来源路径无效"))?;
                let relative = relative
                    .to_str()
                    .ok_or_else(|| invalid("文件名无效"))?
                    .replace('\\', "/");
                emit(
                    entry.path(),
                    &if prefix.is_empty() {
                        relative
                    } else {
                        format!("{prefix}/{relative}")
                    },
                    sender,
                    shared,
                )?;
            }
        } else {
            return Err(invalid(&format!("来源不是文件或目录：{}", root.display())));
        }
    }
    progress::scan_finished();
    Ok(())
}
fn emit(source: &Path, target: &str, sender: &Sender<Work>, shared: &Shared) -> Result<()> {
    let target = crate::veil_ops::normalize_veil_dir(target)?;
    // 队列既限制条目数量，也限制单条路径描述的大小。
    if source.as_os_str().len() + target.len() > 64 * 1024 {
        return Err(invalid("导入路径过长"));
    }
    let size = source.metadata()?.len();
    progress::discovered_file(size);
    send(
        sender,
        Work {
            spec: AddFileSpec::new(source, target).without_overwrite(),
            retries: 0,
            error: None,
        },
        shared,
    )
}

fn dispatch(work: Work, ready: &Sender<Work>, shared: &Shared) -> Result<()> {
    shared.inflight.fetch_add(1, Ordering::AcqRel);
    if let Err(error) = send(ready, work, shared) {
        shared.inflight.fetch_sub(1, Ordering::AcqRel);
        return Err(error);
    }
    Ok(())
}
fn pump(control: &ImportConflictQueue, ready: &Sender<Work>, shared: &Shared) -> Result<()> {
    for mut waiting in control.take_resolved() {
        match waiting.action.unwrap() {
            SessionImportAction::Skip => progress::file_done(true),
            SessionImportAction::Overwrite => {
                // 基础版本在核对线程记录，用户不能替换这份版本依据。
                waiting.work.spec.reject_existing = waiting.work.spec.base_version.is_none();
                dispatch(waiting.work, ready, shared)?;
            }
        }
    }
    Ok(())
}

fn check(
    manager: &VeilManager,
    password: &str,
    scan: Receiver<Work>,
    ready: Sender<Work>,
    control: &ImportConflictQueue,
    shared: &Shared,
    directory_locks: &ImportDirectoryLocks,
    policy: BatchPolicy,
) -> Result<()> {
    let (db, identity) = retry_database_busy(|| {
        let db = manager.open_db(password)?;
        let identity = db.info()?.0;
        Ok((db, identity))
    })?;
    scan.listen_batches(
        policy,
        |_| 1,
        |state| {
            if !shared.active() {
                return Err(VeilError::Cancelled);
            }
            pump(control, &ready, shared)?;
            let capacity = (WAIT_LIMIT - control.len()).min(CHECK_BATCH);
            let input_finished = shared.scan_complete.load(Ordering::Acquire) && scan.len() == 0;
            let mut next = ListenControl::new(capacity, state.closed);
            next.flush = input_finished;
            next.finish = input_finished
                && shared.inflight.load(Ordering::Acquire) == 0
                && control.len() == 0;
            Ok(next)
        },
        |batch| {
            let paths = batch
                .iter()
                .map(|w| w.spec.target.clone())
                .collect::<Vec<_>>();
            let mut nodes = retry_database_busy(|| {
                db.conn.execute_batch("BEGIN DEFERRED")?;
                match container::resolve_batch(&db.conn, &paths) {
                    Ok(nodes) => {
                        db.conn.execute_batch("ROLLBACK")?;
                        Ok(nodes)
                    }
                    Err(error) => {
                        if let Err(rollback) = db.conn.execute_batch("ROLLBACK") {
                            veil_core::logging::error_with_code(
                                "database",
                                "import_rollback",
                                "ROLLBACK_FAILED",
                                &rollback,
                            );
                        }
                        Err(error)
                    }
                }
            })?;
            for mut work in batch {
                if !shared.active() {
                    return Err(VeilError::Cancelled);
                }
                if work.retries == 0 {
                    progress::file_checked();
                }
                let target = work.spec.target.clone();
                let (node, hash, exact) = nodes
                    .remove(&target)
                    .ok_or_else(|| invalid("批量路径核对缺少结果"))?;
                let exists = exact || node.kind != "directory";
                if !exists && work.error.is_none() {
                    dispatch(work, &ready, shared)?;
                    continue;
                }
                let is_directory = exists && (!exact || node.kind != "file");
                let mut same_content = false;
                let mut error = work.error.take();
                if exists && !is_directory {
                    let version = VersionSnapshot {
                        veil_id: identity.clone(),
                        file_id: node.id.clone(),
                        version_history: node.versions()?.to_vec(),
                    };
                    work.spec.base_version = Some(version.clone());
                    let source_size = match work.spec.source.metadata() {
                        Ok(metadata) => Some(metadata.len()),
                        Err(failure) => {
                            veil_core::logging::error_chain(
                                "service",
                                "import_inspection",
                                &failure,
                            );
                            error = Some(failure.to_string());
                            None
                        }
                    };
                    if error.is_none() && node.size == source_size {
                        let input = match hash.as_deref() {
                            Some(hash) => FileInput::ContainerMetadata {
                                version: &version,
                                hash: Some(hash),
                            },
                            None => match directory_locks.scope(directory_locks.index(&target)) {
                                Some((guard, directory)) => {
                                    FileInput::ContainerCiphertextInDirectory {
                                        manager,
                                        password,
                                        path: &target,
                                        guard,
                                        directory,
                                    }
                                }
                                None => FileInput::ContainerCiphertext {
                                    manager,
                                    password,
                                    path: &target,
                                },
                            },
                        };
                        match compare_files(
                            input,
                            FileInput::External {
                                path: &work.spec.source,
                                version: None,
                            },
                            ComparisonChecks {
                                hash: true,
                                ..Default::default()
                            },
                        ) {
                            Ok(report) => same_content = report.hash == Some(ComparisonState::Same),
                            Err(VeilError::Cancelled) => return Err(VeilError::Cancelled),
                            Err(failure) => {
                                veil_core::logging::error_chain(
                                    "service",
                                    "import_comparison",
                                    &failure,
                                );
                                error = Some(failure.to_string());
                            }
                        }
                    }
                } else {
                    work.spec.base_version = None;
                }
                control.push(Waiting {
                    view: ConflictView {
                        target,
                        same_content,
                        is_directory,
                        error,
                    },
                    work,
                    action: None,
                })?;
            }
            Ok(())
        },
    )
}

fn process_work_group(
    manager: &VeilManager,
    password: &str,
    works: Vec<Work>,
    directory_scope: Option<(&crate::storage::locks::LeaseGuard, &LockedDirectory)>,
    gate: &Mutex<()>,
    context: &ImportBatchContext,
    feedback: &Sender<Work>,
    shared: &Shared,
) {
    let specs = works
        .iter()
        .map(|work| work.spec.clone())
        .collect::<Vec<_>>();
    match manager.add_files_for_import(&specs, password, directory_scope, gate, context) {
        Ok(_) | Err(VeilError::Cancelled) => {}
        Err(error) if works.len() == 1 => {
            retry_work(works.into_iter().next().unwrap(), error, feedback, shared)
        }
        Err(error) => {
            veil_core::logging::failure(
                veil_core::logging::Level::Warn,
                "service",
                "import_batch",
                "BATCH_FALLBACK",
                &error,
                &[(
                    "batch_files",
                    veil_core::logging::Field::Count(works.len() as u64),
                )],
            );
            // 仅异常批次逐项定位；正常批次共用连接、密钥、准备事务和目录锁。
            for work in works {
                if !shared.active() {
                    break;
                }
                if let Err(error) = manager.add_files_for_import(
                    std::slice::from_ref(&work.spec),
                    password,
                    directory_scope,
                    gate,
                    context,
                ) {
                    retry_work(work, error, feedback, shared);
                }
            }
        }
    }
}

fn retry_work(mut work: Work, error: VeilError, feedback: &Sender<Work>, shared: &Shared) {
    if matches!(error, VeilError::Cancelled) {
        return;
    }
    // 版本冲突会重新核对后交给用户选择，是正常并发结果。
    if !matches!(
        &error,
        VeilError::FileAlreadyExists(_) | VeilError::VersionConflict(_)
    ) {
        veil_core::logging::failure(
            veil_core::logging::Level::Warn,
            "service",
            "import_worker",
            "IMPORT_FILE_RECHECK",
            &error,
            &[(
                "retries",
                veil_core::logging::Field::Count(work.retries as u64),
            )],
        );
    }
    work.retries += 1;
    work.spec.reject_existing = true;
    work.spec.base_version = None;
    if work.retries > 2
        || !matches!(
            error,
            VeilError::FileAlreadyExists(_) | VeilError::VersionConflict(_)
        )
    {
        work.error = Some(error.to_string());
    }
    let _ = send(feedback, work, shared);
}

pub(crate) fn run(
    manager: &VeilManager,
    password: &str,
    sources: Vec<String>,
    directory: String,
    progress_state: &progress::Progress,
    control: &Arc<ImportConflictQueue>,
) -> std::result::Result<ContainerView, ServiceError> {
    run_mapped(
        manager,
        password,
        sources,
        directory,
        progress_state,
        control,
        None,
    )?;
    Ok(super::session::container_view(
        &manager.veil_dir,
        &manager.read_meta(password)?,
    ))
}

pub(crate) fn run_mapped(
    manager: &VeilManager,
    password: &str,
    sources: Vec<String>,
    directory: String,
    progress_state: &progress::Progress,
    control: &Arc<ImportConflictQueue>,
    mapped_target: Option<String>,
) -> std::result::Result<(), ServiceError> {
    if mapped_target.is_some() && sources.len() != 1 {
        return Err(ServiceError::InvalidInput("指定目标路径时只支持一个来源"));
    }
    if sources.is_empty() {
        return Err(ServiceError::InvalidInput("请选择文件或文件夹"));
    }
    let directory = if directory.is_empty() {
        directory
    } else {
        crate::veil_ops::normalize_veil_dir(&directory)?
    };
    let policy = load_policy()?;
    let commit_count = policy.count;
    let worker_count = progress::worker_count(usize::MAX);
    let shared = Shared::default();
    let gate = Mutex::new(());
    let owner = crate::execution::ExecutionService::global()
        .map_err(|source| ServiceError::Io {
            operation: "无法初始化导入执行上下文",
            source,
        })?
        .runtime()
        .current_context()
        .owner()
        .clone();
    progress::start(Some(0));
    progress::set_parallel_workers(worker_count);
    let directory_locks = match ImportDirectoryLocks::acquire(
        manager,
        password,
        &sources,
        &directory,
        progress_state,
        mapped_target.as_deref(),
    ) {
        Ok(locks) => locks,
        Err(_) if progress::check_cancelled().is_err() => {
            return Err(ServiceError::ImportCancelled {
                completed: 0,
                skipped: 0,
            });
        }
        Err(error) => return Err(error.into()),
    };
    let logging_context = veil_core::logging::capture_context();
    std::thread::scope(|scope| {
        let logging_context = &logging_context;
        // 遍历线程持续整理文件清单，不因核对或用户选择暂停；仅投递任务描述。
        let (scan_tx, scan_rx) = UnboundedQueue::new().split();
        // 核对完成后交给处理线程的执行队列仍有容量上限。
        let (ready_tx, ready_rx) = BoundedQueue::new(EXECUTE_LIMIT).split();
        // 重试投回同一待检查队列，仍由队列驱动核对回调。
        let feedback_tx = scan_tx.clone();
        let (completed_tx, completed_rx) = BoundedQueue::new(COMPLETED_LIMIT).split();
        let shared_ref = &shared;
        let directory_locks = &directory_locks;
        scope.spawn(move || {
            let _logging = logging_context.attach();
            let _guard = ThreadGuard(shared_ref);
            let _attachment = progress_state.attach();
            match enumerate(
                &sources,
                &directory,
                &scan_tx,
                shared_ref,
                mapped_target.as_deref(),
            ) {
                Ok(()) => shared_ref.scan_complete.store(true, Ordering::Release),
                Err(error) if !matches!(error, VeilError::Cancelled) => {
                    shared_ref.fail("遍历来源".into(), error)
                }
                Err(_) => {}
            }
        });
        scope.spawn(move || {
            let _logging = logging_context.attach();
            let _guard = ThreadGuard(shared_ref);
            let _attachment = progress_state.attach();
            if let Err(error) = check(
                manager,
                password,
                scan_rx,
                ready_tx,
                control,
                shared_ref,
                directory_locks,
                policy,
            ) {
                if !matches!(error, VeilError::Cancelled) {
                    shared_ref.fail("核对文件".into(), error);
                }
            }
        });
        let directories = Arc::new(crate::directory_cache::DirectoryCache::default());
        for _ in 0..worker_count {
            let ready_rx = ready_rx.clone();
            let feedback_tx = feedback_tx.clone();
            let context = ImportBatchContext {
                directories: directories.clone(),
                sender: completed_tx.clone(),
                directory_guard: directory_locks.guard.clone(),
                owner: owner.clone(),
            };
            let gate = &gate;
            scope.spawn(move || {
                let _logging = logging_context.attach();
                let _guard = ThreadGuard(shared_ref);
                let _attachment = progress_state.attach();
                let execute_policy = BatchPolicy {
                    count: PROCESS_BATCH.min(commit_count),
                    interval: policy.interval,
                };
                let _ = ready_rx.listen_batches(
                    execute_policy,
                    |_| 1,
                    |state| {
                        if !shared_ref.active() {
                            return Err(VeilError::Cancelled);
                        }
                        Ok(ListenControl::new(execute_policy.count, state.closed))
                    },
                    |works| {
                        let count = works.len();
                        // 每组只使用覆盖其目标路径的目录锁，避免再次申请文件租约。
                        let mut groups =
                            std::collections::BTreeMap::<Option<usize>, Vec<Work>>::new();
                        for work in works {
                            groups
                                .entry(directory_locks.index(&work.spec.target))
                                .or_default()
                                .push(work);
                        }
                        for (index, works) in groups {
                            process_work_group(
                                manager,
                                password,
                                works,
                                directory_locks.scope(index),
                                gate,
                                &context,
                                &feedback_tx,
                                shared_ref,
                            );
                        }
                        shared_ref.inflight.fetch_sub(count, Ordering::AcqRel);
                        Ok(())
                    },
                );
            });
        }
        drop(ready_rx);
        drop(completed_tx);
        drop(feedback_tx);
        // 沿用现有完成队列和批量提交机制，不在工作线程中独立提交。
        let result = completed_rx.listen_batches(
            policy,
            |item: &crate::veil_ops::QueuedImport| item.file_count(),
            |state| {
                let mut next = ListenControl::new(commit_count, state.closed);
                next.max_items = commit_count
                    .saturating_sub(state.pending_weight)
                    .div_ceil(PROCESS_BATCH.min(commit_count));
                next.flush |= !shared.active();
                Ok::<_, VeilError>(next)
            },
            |batch| {
                if !shared.active() {
                    VeilManager::cancel_import_batch(batch);
                } else {
                    let target = batch[0].target().to_owned();
                    let _gate = gate.lock().unwrap_or_else(|e| e.into_inner());
                    if let Err(error) = manager.commit_import_batch(batch, password, &shared.stop) {
                        shared.fail(target, error);
                    }
                }
                Ok(())
            },
        );
        if let Err(error) = result {
            shared.fail("提交文件".into(), error);
        }
    });
    let snapshot = progress_state.snapshot();
    drop(directory_locks);
    if let Some((name, source)) = shared
        .failure
        .into_inner()
        .unwrap_or_else(|e| e.into_inner())
    {
        return Err(ServiceError::ImportFileFailed {
            name,
            completed: snapshot.completed_files,
            skipped: snapshot.skipped_files,
            source,
        });
    }
    if snapshot.cancel_requested {
        return Err(ServiceError::ImportCancelled {
            completed: snapshot.completed_files,
            skipped: snapshot.skipped_files,
        });
    }
    if snapshot.total_files == 0 {
        return Err(ServiceError::InvalidInput("选择的文件夹没有可添加的文件"));
    }
    progress::finalizing();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn batch_lookup_covers_exact_missing_and_blocked_paths() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::tempdir().unwrap();
        let manager = VeilManager::new(root.path().join("container"));
        manager.init_veil("test", "password").unwrap();
        let source = root.path().join("source");
        std::fs::write(&source, b"content").unwrap();
        manager
            .add_files(&[AddFileSpec::new(&source, "folder/file")], "password")
            .unwrap();
        let db = manager.open_db("password").unwrap();
        let paths: Vec<_> = [
            "folder/file",
            "folder/missing",
            "folder",
            "folder/file/child",
            "absent",
            "quote'file",
        ]
        .iter()
        .map(|path| path.to_string())
        .collect();
        let rows = container::resolve_batch(&db.conn, &paths).unwrap();
        assert_eq!(rows.len(), paths.len());
        assert!(rows["folder/file"].2);
        assert_eq!(
            rows["folder/file"].1.as_deref(),
            Some(blake3::hash(b"content").to_hex().as_str())
        );
        assert!(!rows["folder/missing"].2);
        assert_eq!(rows["folder/missing"].0.kind, "directory");
        assert!(rows["folder"].2);
        assert!(!rows["folder/file/child"].2);
        assert_eq!(rows["folder/file/child"].0.kind, "file");
        assert!(!rows["quote'file"].2);
    }
}
