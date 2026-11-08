//! 明文导出流水线：单线程分页遍历、批量核对、有界解密和批量发布。
//! 已发布文件在失败或取消后保留；未发布的临时明文随队列资源释放而删除。
use crate::{
    batch_policy::{BatchPolicy, ListenControl, load_policy},
    comparison::{ComparisonChecks, ComparisonState, FileInput, compare_files},
    error::{Result, VeilError},
    session::SessionImportAction,
    storage::{
        container::{self, Node},
        invalid,
        locks::{self, LeaseGuard, LockMode},
    },
    transfer_conflicts::{ConflictQueue, ConflictView, Waiting},
    veil_ops::{LockedDirectory, VeilManager, normalize_veil_dir},
};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use veil_core::{
    file_ops, progress,
    queue::{BoundedQueue, Receiver, Sender, UnboundedQueue},
    version::VersionSnapshot,
};
use zeroize::Zeroizing;

const BATCH: usize = 128;
#[derive(Clone)]
pub(crate) struct Work {
    source: String,
    target: PathBuf,
    expected: Option<Stamp>,
    overwrite: bool,
}
#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}
impl Stamp {
    fn read(path: &Path) -> Result<Option<Self>> {
        let m = match fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if !m.is_file() {
            return Err(invalid("目标不是普通文件"));
        }
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Some(Self {
            len: m.len(),
            modified: m.modified().ok(),
            #[cfg(unix)]
            identity: (m.dev(), m.ino(), m.ctime(), m.ctime_nsec()),
        }))
    }
}
struct Ready {
    work: Work,
    node: Node,
}
struct Staged {
    ready: Ready,
    temporary: Option<tempfile::TempPath>,
}
#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    failure: Mutex<Option<VeilError>>,
}
impl Shared {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire) || progress::check_cancelled().is_err()
    }
    fn fail(&self, error: VeilError) {
        self.stop.store(true, Ordering::Release);
        let mut failure = self.failure.lock().unwrap_or_else(|e| e.into_inner());
        if failure.is_none() {
            *failure = Some(error);
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
fn send<T>(queue: &Sender<T>, item: T, shared: &Shared) -> Result<()> {
    queue
        .send_with(item, || shared.stopped())
        .map_err(|_| VeilError::Cancelled)
}
fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}
/// 禁止沿符号链接发布明文，避免导出落入选定目录之外。
fn check_parents(path: &Path) -> Result<()> {
    for ancestor in path.ancestors().skip(1) {
        match fs::symlink_metadata(ancestor) {
            Ok(m) if !m.is_dir() || m.file_type().is_symlink() => {
                return Err(invalid("导出路径的上级不是普通目录"));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn enumerate(
    manager: &VeilManager,
    password: &str,
    roots: Vec<(String, Node, PathBuf)>,
    queue: Sender<Work>,
    shared: &Shared,
) -> Result<()> {
    let db = manager.open_db(password)?;
    // 仅保留当前深度的分页游标；无界队列承接发现的工作，不预载整个目录树。
    fn visit(
        db: &container::ContainerDb,
        source: String,
        node: Node,
        target: PathBuf,
        queue: &Sender<Work>,
        shared: &Shared,
    ) -> Result<()> {
        if shared.stopped() {
            return Err(VeilError::Cancelled);
        }
        if node.kind == "file" {
            progress::discovered_file(node.size.unwrap_or(0));
        }
        send(
            queue,
            Work {
                source: source.clone(),
                target: target.clone(),
                expected: None,
                overwrite: false,
            },
            shared,
        )?;
        if node.kind == "directory" {
            let mut after = None;
            loop {
                let page = db.children_after(&node.id, after.as_deref(), BATCH as u32)?;
                if page.is_empty() {
                    break;
                }
                after = page.last().map(|n| n.name.clone());
                for child in page {
                    let path = if source.is_empty() {
                        child.name.clone()
                    } else {
                        format!("{source}/{}", child.name)
                    };
                    let output = target.join(&child.name);
                    visit(db, path, child, output, queue, shared)?;
                }
            }
        }
        Ok(())
    }
    for (source, node, target) in roots {
        visit(&db, source, node, target, &queue, shared)?;
    }
    progress::scan_finished();
    Ok(())
}
fn resolved(
    control: &ConflictQueue<Work>,
    output: &Sender<Ready>,
    db: &container::ContainerDb,
    shared: &Shared,
) -> Result<()> {
    let tasks = control.take_resolved();
    let paths = tasks
        .iter()
        .map(|t| t.work.source.clone())
        .collect::<Vec<_>>();
    let nodes = container::resolve_batch(&db.conn, &paths)?;
    for task in tasks {
        let mut work = task.work;
        let node = if work.source.is_empty() {
            db.root()?
        } else {
            nodes
                .get(&work.source)
                .filter(|(_, _, exact)| *exact)
                .ok_or_else(|| invalid("导出源已变化"))?
                .0
                .clone()
        };
        match task.action {
            Some(SessionImportAction::Skip) => {
                if node.kind == "file" {
                    progress::file_done(true);
                }
            }
            Some(SessionImportAction::Overwrite) => {
                work.overwrite = true;
                send(output, Ready { work, node }, shared)?;
            }
            None => return Err(invalid("缺少导出处理选择")),
        }
    }
    Ok(())
}
fn check(
    manager: &VeilManager,
    password: &str,
    guard: &LeaseGuard,
    directory: &LockedDirectory,
    input: Receiver<Work>,
    output: Sender<Ready>,
    control: &ConflictQueue<Work>,
    interactive: bool,
    shared: &Shared,
    policy: BatchPolicy,
) -> Result<()> {
    let db = manager.open_db(password)?;
    let identity = db.info()?.0;
    input.listen_batches(
        policy,
        |_| 1,
        |state| {
            if shared.stopped() {
                return Err(VeilError::Cancelled);
            }
            resolved(control, &output, &db, shared)?;
            let mut next = ListenControl::new((128 - control.len()).min(BATCH), state.closed);
            next.finish = state.closed && control.len() == 0;
            Ok(next)
        },
        |batch| {
            let paths = batch.iter().map(|w| w.source.clone()).collect::<Vec<_>>();
            let nodes = container::resolve_batch(&db.conn, &paths)?;
            for mut work in batch {
                let (node, hash) = if work.source.is_empty() {
                    (db.root()?, None)
                } else {
                    let (n, h, exact) = nodes
                        .get(&work.source)
                        .ok_or_else(|| invalid("导出源不存在"))?;
                    if !exact {
                        return Err(invalid("导出源已变化"));
                    }
                    (n.clone(), h.clone())
                };
                if node.kind == "file" {
                    progress::file_checked();
                }
                let check = check_parents(&work.target).and_then(|_| {
                    fs::symlink_metadata(&work.target)
                        .map(Some)
                        .or_else(|e| {
                            if e.kind() == std::io::ErrorKind::NotFound {
                                Ok(None)
                            } else {
                                Err(e)
                            }
                        })
                        .map_err(Into::into)
                });
                let mut view = None;
                match check {
                    Ok(None) => {}
                    Ok(Some(m))
                        if node.kind == "directory"
                            && m.is_dir()
                            && !m.file_type().is_symlink() => {}
                    Ok(Some(m)) => {
                        let incompatible =
                            node.kind == "directory" || !m.is_file() || m.file_type().is_symlink();
                        let mut same = false;
                        let mut error = None;
                        if !incompatible {
                            work.expected = Stamp::read(&work.target)?;
                            let version = VersionSnapshot {
                                veil_id: identity.clone(),
                                file_id: node.id.clone(),
                                version_history: node.versions()?.to_vec(),
                            };
                            let source = if let Some(hash) = hash.as_deref() {
                                FileInput::ContainerMetadata {
                                    version: &version,
                                    hash: Some(hash),
                                }
                            } else {
                                FileInput::ContainerCiphertextInDirectory {
                                    manager,
                                    password,
                                    path: &work.source,
                                    guard,
                                    directory,
                                }
                            };
                            match compare_files(
                                source,
                                FileInput::External {
                                    path: &work.target,
                                    version: None,
                                },
                                ComparisonChecks {
                                    hash: true,
                                    ..Default::default()
                                },
                            ) {
                                Ok(report) => same = report.hash == Some(ComparisonState::Same),
                                Err(e) => {
                                    veil_core::logging::error_chain(
                                        "service",
                                        "export_comparison",
                                        &e,
                                    );
                                    error = Some(e.to_string());
                                }
                            }
                            if Stamp::read(&work.target)? != work.expected {
                                error = Some("目标在检查期间已变化，请重新导出".into());
                            }
                        }
                        view = Some(ConflictView {
                            target: work.target.to_string_lossy().into_owned(),
                            same_content: same,
                            is_directory: incompatible,
                            error,
                        });
                    }
                    Err(e) => {
                        veil_core::logging::error_chain("service", "export_inspection", &e);
                        view = Some(ConflictView {
                            target: work.target.to_string_lossy().into_owned(),
                            same_content: false,
                            is_directory: true,
                            error: Some(e.to_string()),
                        })
                    }
                }
                if let Some(view) = view {
                    if !interactive {
                        return Err(invalid(&format!("导出目标已存在或不可写：{}", view.target)));
                    }
                    control.push(Waiting {
                        work,
                        view,
                        action: None,
                    })?;
                } else {
                    send(&output, Ready { work, node }, shared)?;
                }
            }
            Ok(())
        },
    )
}

fn process(
    manager: &VeilManager,
    keys: &HashMap<String, Zeroizing<[u8; 32]>>,
    directories: &crate::directory_cache::DirectoryCache,
    input: Receiver<Ready>,
    output: Sender<Staged>,
    shared: &Shared,
) -> Result<()> {
    input.listen_batches(
        BatchPolicy {
            count: 1,
            interval: Duration::ZERO,
        },
        |_| 1,
        |state| {
            if shared.stopped() {
                return Err(VeilError::Cancelled);
            }
            Ok(ListenControl::new(1, state.closed))
        },
        |batch| {
            for ready in batch {
                check_parents(&ready.work.target)?;
                directories.ensure_filesystem(parent(&ready.work.target))?;
                let temporary = if ready.node.kind == "file" {
                    progress::begin_file(&ready.work.source);
                    let mut temporary =
                        tempfile::NamedTempFile::new_in(parent(&ready.work.target))?;
                    let key = keys
                        .get(
                            ready
                                .node
                                .key_id
                                .as_deref()
                                .ok_or_else(|| invalid("缺少文件密钥"))?,
                        )
                        .ok_or_else(|| invalid("文件密钥不存在"))?;
                    // 直接写入待发布文件，不再由解密函数创建第二层临时文件。
                    file_ops::decrypt_file_to_writer(
                        &manager.veil_dir.join(ready.node.cipher()?),
                        temporary.as_file_mut(),
                        key,
                        &ready.node.nonce()?,
                        ready.node.size.unwrap(),
                    )?;
                    crate::fsutil::sync_file(temporary.as_file())?;
                    Some(temporary.into_temp_path())
                } else {
                    None
                };
                send(&output, Staged { ready, temporary }, shared)?;
            }
            Ok(())
        },
    )
}

fn publish(
    db: &mut container::ContainerDb,
    guard: &LeaseGuard,
    directories: &crate::directory_cache::DirectoryCache,
    batch: Vec<Staged>,
) -> Result<()> {
    progress::check_cancelled()?;
    // 在事务之前取凭据，避免与续约线程形成相反的加锁顺序。
    let credentials = guard.credentials()?;
    {
        // 读租约保护整个导出过程；数据库事务只用于取得发布前的一致元数据快照。
        // 不能将明文发布或目录同步置于事务内，否则会长时间阻塞写事务提交。
        let tx = db.conn.transaction()?;
        locks::validate_many(
            &tx,
            &credentials
                .iter()
                .map(|c| (c, Some(LockMode::Read)))
                .collect::<Vec<_>>(),
        )?;
        let nodes = container::nodes_batch(
            &tx,
            &batch
                .iter()
                .map(|s| s.ready.node.id.clone())
                .collect::<Vec<_>>(),
        )?;
        for staged in &batch {
            if nodes.get(&staged.ready.node.id) != Some(&staged.ready.node) {
                return Err(VeilError::VersionConflict("导出源在发布前已变化".into()));
            }
        }
        tx.commit()?;
    }
    // 文件系统发布逐项完成；错误不会回滚已发布文件，进度仅在发布成功后增加。
    for staged in batch {
        progress::check_cancelled()?;
        let target = &staged.ready.work.target;
        check_parents(target)?;
        if let Some(temporary) = staged.temporary {
            if staged.ready.work.overwrite {
                if Stamp::read(target)? != staged.ready.work.expected {
                    return Err(VeilError::VersionConflict(
                        "导出目标在确认后已变化，请重新导出".into(),
                    ));
                }
                temporary
                    .persist(target)
                    .map_err(|e| VeilError::Io(e.error))?;
            } else {
                temporary
                    .persist_noclobber(target)
                    .map_err(|e| VeilError::Io(e.error))?;
            }
            crate::fsutil::sync_directory(parent(target))?;
            progress::file_done(false);
        } else {
            match fs::symlink_metadata(target) {
                Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {}
                Ok(_) => return Err(invalid("导出目录被其他文件占用")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    directories.ensure_filesystem(target)?
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    let credentials = guard.credentials()?;
    locks::validate_many(
        &db.conn,
        &credentials
            .iter()
            .map(|c| (c, Some(LockMode::Read)))
            .collect::<Vec<_>>(),
    )?;
    Ok(())
}

pub(crate) fn run(
    manager: &VeilManager,
    password: &str,
    names: Vec<String>,
    destination: &Path,
    progress: &progress::Progress,
    control: &Arc<ConflictQueue<Work>>,
    interactive: bool,
) -> Result<()> {
    if names.is_empty() || destination.as_os_str().is_empty() {
        return Err(invalid("请选择导出文件和导出位置"));
    }
    let db = manager.open_db(password)?;
    let mut names = names
        .into_iter()
        .map(|n| normalize_veil_dir(&n))
        .collect::<Result<Vec<_>>>()?;
    names.sort();
    names.dedup();
    let all = names.clone();
    names.retain(|n| {
        !all.iter().any(|p| {
            p != n && (p.is_empty() || n.strip_prefix(p).is_some_and(|s| s.starts_with('/')))
        })
    });
    let roots = names
        .iter()
        .map(|n| {
            db.resolve(n)?
                .map(|node| (n.clone(), node))
                .ok_or_else(|| VeilError::FileNotFound(n.clone()))
        })
        .collect::<Result<Vec<_>>>()?;
    let exact = roots.len() == 1 && roots[0].1.kind == "file";
    // 复用覆盖本次选择的最小公共目录读锁，不为每个文件另行加锁。
    let scopes = roots
        .iter()
        .map(|(name, node)| {
            if node.kind == "directory" {
                name.as_str()
            } else {
                name.rsplit_once('/').map_or("", |(p, _)| p)
            }
        })
        .collect::<Vec<_>>();
    let mut directory_path = scopes[0].to_owned();
    while !scopes.iter().all(|p| {
        *p == directory_path
            || directory_path.is_empty()
            || p.strip_prefix(&directory_path)
                .is_some_and(|s| s.starts_with('/'))
    }) {
        directory_path = directory_path
            .rsplit_once('/')
            .map_or("", |(p, _)| p)
            .to_owned();
    }
    let directory_node = db
        .resolve(&directory_path)?
        .ok_or_else(|| invalid("导出目录不存在"))?;
    drop(db);
    let guard = manager
        .lock_manager(password)
        .acquire_directory(&directory_node.id, LockMode::Read)?;
    let directory = LockedDirectory::for_export(directory_path, directory_node.id);
    let db = manager.open_db(password)?;
    for (name, node) in &roots {
        if db.resolve(name)?.as_ref() != Some(node) {
            return Err(invalid("取得目录锁前导出源已变化"));
        }
    }
    let destination = if destination.is_absolute() {
        destination.to_owned()
    } else {
        std::env::current_dir()?.join(destination)
    };
    // 不允许明文导出到容器内部（包括包含 .. 的路径）。
    let mut normalized = PathBuf::new();
    for c in destination.components() {
        match c {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            c => normalized.push(c.as_os_str()),
        }
    }
    // 解析系统目录别名（如 macOS 的 /var、/tmp）；之后的检查针对实际路径。
    let mut cursor = normalized.as_path();
    let mut missing = Vec::new();
    while !cursor.exists() {
        missing.push(
            cursor
                .file_name()
                .ok_or_else(|| invalid("导出位置无效"))?
                .to_owned(),
        );
        cursor = cursor.parent().ok_or_else(|| invalid("导出位置无效"))?;
    }
    let mut destination = if cursor.is_file() {
        fs::canonicalize(parent(cursor))?
            .join(cursor.file_name().ok_or_else(|| invalid("导出位置无效"))?)
    } else {
        // 目标本身为符号链接时，交给冲突队列处理，不能绕过确认。
        if fs::symlink_metadata(cursor)?.file_type().is_symlink() && missing.is_empty() {
            fs::canonicalize(parent(cursor))?
                .join(cursor.file_name().ok_or_else(|| invalid("导出位置无效"))?)
        } else {
            fs::canonicalize(cursor)?
        }
    };
    for component in missing.into_iter().rev() {
        destination.push(component);
    }
    check_parents(&destination)?;
    if destination.starts_with(fs::canonicalize(&manager.veil_dir)?) {
        return Err(invalid("不能将明文导出到容器内部"));
    }
    let roots = roots
        .into_iter()
        .map(|(name, node)| {
            let target = if exact || name.is_empty() {
                destination.clone()
            } else {
                destination.join(&node.name)
            };
            (name, node, target)
        })
        .collect::<Vec<_>>();
    let mut targets = std::collections::HashSet::new();
    if roots
        .iter()
        .any(|(_, _, target)| !targets.insert(target.clone()))
    {
        return Err(invalid("选择的项目包含同名根目录或同名文件，请分别导出"));
    }
    let keys = db
        .keys()?
        .iter()
        .map(|k| Ok((k.key_id.clone(), k.material()?)))
        .collect::<Result<HashMap<_, _>>>()?;
    drop(db);
    let policy = load_policy()?;
    let (pending_tx, pending_rx) = UnboundedQueue::new().split();
    let (ready_tx, ready_rx) = BoundedQueue::new(32).split();
    let (done_tx, done_rx) = BoundedQueue::new(8).split();
    let mut publisher_db = manager.open_db(password)?;
    let directories = crate::directory_cache::DirectoryCache::default();
    let shared = Shared::default();
    let workers = progress::worker_count(32);
    progress::stage("processing", Some(0));
    progress::set_parallel_workers(workers);
    let logging_context = veil_core::logging::capture_context();
    std::thread::scope(|scope| {
        let logging_context = &logging_context;
        let shared = &shared;
        scope.spawn(move || {
            let _logging = logging_context.attach();
            let _attachment = progress.attach();
            let _thread_guard = ThreadGuard(shared);
            if let Err(e) = enumerate(manager, password, roots, pending_tx, shared) {
                shared.fail(e);
            }
        });
        let guard = &guard;
        let directory = &directory;
        scope.spawn(move || {
            let _logging = logging_context.attach();
            let _attachment = progress.attach();
            let _thread_guard = ThreadGuard(shared);
            if let Err(e) = check(
                manager,
                password,
                guard,
                directory,
                pending_rx,
                ready_tx,
                control,
                interactive,
                shared,
                policy,
            ) {
                shared.fail(e);
            }
        });
        for _ in 0..workers {
            let input = ready_rx.clone();
            let output = done_tx.clone();
            let keys = &keys;
            let directories = &directories;
            scope.spawn(move || {
                let _logging = logging_context.attach();
                let _attachment = progress.attach();
                let _thread_guard = ThreadGuard(shared);
                if let Err(e) = process(manager, keys, directories, input, output, shared) {
                    shared.fail(e);
                }
            });
        }
        drop(ready_rx);
        drop(done_tx);
        let result = done_rx.listen_batches(
            policy,
            |_| 1,
            |state| {
                if shared.stopped() {
                    return Err(VeilError::Cancelled);
                }
                Ok(ListenControl::new(policy.count, state.closed))
            },
            |batch| publish(&mut publisher_db, guard, &directories, batch),
        );
        if let Err(error) = result {
            shared.fail(error);
        }
    });
    progress::set_waiting_files(0);
    if progress::check_cancelled().is_err() {
        return Err(VeilError::Cancelled);
    }
    if let Some(error) = shared
        .failure
        .into_inner()
        .unwrap_or_else(|e| e.into_inner())
    {
        return Err(error);
    }
    Ok(())
}
