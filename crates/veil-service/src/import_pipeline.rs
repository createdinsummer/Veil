//! 导入流水线：无界收集待处理文件，有界执行，复用现有完成队列及提交事务。
//!
//! 所属业务模块：veil-service 的外部文件导入管线（把"把外部文件/目录
//! 递归加密导入容器"这个大操作拆成可并行推进的多个阶段）。
//!
//! 文件内核心结构与函数清单：
//! - Work：一条待导入文件的工作单元（请求 + 重试计数 + 错误缓存）
//! - Shared：各线程共享的管线状态（停止信号、扫描完成标记、在途计数、首个失败）
//! - ImportDirectoryLocks：目标目录的共享写租约集合
//! - enumerate：遍历线程，递归扫源文件生成 Work（无界队列，不阻塞扫描）
//! - check：核对线程，批量查询目标路径状态，冲突的送用户选择，无冲突的转执行
//! - run_mapped：管线总编排（遍历/核对/多个执行线程/批量提交线程）
//!
//! 依赖能力：
//! - veil_core::queue：有界/无界队列（削峰：收集阶段不阻塞、执行阶段限流）
//! - veil_ops：VeilManager 的批量写入与批量提交
//! - transfer_conflicts：同名冲突的用户决策队列
//! - batch_policy：用户设置驱动的批大小与间隔
//!
//! 整体业务目标：扫描与核对可随时被取消；加密执行限流为有限个并发
//! 工作线程；全部提交合并进少量短写事务，避免长时间占库。
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
/// 导入冲突队列的别名：队列项是 Work（等待用户决定覆盖/跳过的文件）。
pub(crate) type ImportConflictQueue = ConflictQueue<Work>;

/// 核对线程单批查询的目标路径数量上限。
const CHECK_BATCH: usize = 128;
/// 加密执行队列（ready）的容量上限：控制同时等待执行的工作单元数量。
const EXECUTE_LIMIT: usize = 32;
/// 每个执行线程单批加密处理的文件数量。
const PROCESS_BATCH: usize = 8;
/// 待核对（waiting）队列的容量上限。
const WAIT_LIMIT: usize = 128;
/// 完成提交队列的容量上限。
const COMPLETED_LIMIT: usize = 8;
/// 数据库繁忙时的固定退避序列（毫秒）：逐次翻倍，最多重试 4 次。
const DATABASE_BUSY_RETRY_DELAYS: [Duration; 4] = [
    Duration::from_millis(100),
    Duration::from_millis(200),
    Duration::from_millis(400),
    Duration::from_millis(800),
];

/// 识别可在导入检查阶段等待重试的 SQLite 竞争错误。
/// 入参：error（&VeilError）为一次操作失败。
/// 返回值：仅 Database 包装的 DatabaseBusy/DatabaseLocked 返回 true。
/// 异常：无错误返回。
/// 注意事项：不代表任意业务操作可安全重放，也不识别被其他错误变体包裹的忙错误。
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

/// 对可安全重放的导入查询操作作有限数据库忙重试。
/// 入参：operation（FnMut() -> `Result<T>`）为调用方提供的完整重试单元。
/// 返回值：首次成功或后续重试成功返回 T；最终错误原样返回。
/// 异常：非数据库忙立即返回；忙错误最多额外重试4次，休眠100/200/400/800ms。闭包 panic 不捕获。
/// 注意事项：最多调用5次；不在休眠期间检查取消，无总时限，单次数据库调用也可能等待。调用方负责事务回滚和重放安全。
fn retry_database_busy<T>(mut operation: impl FnMut() -> Result<T>) -> Result<T> {
    use veil_core::logging::{self, Field, Level};
    // attempt：已重试次数；started：重试起点，用于统计总耗时。
    let mut attempt = 0;
    let started = std::time::Instant::now();
    loop {
        match operation() {
            Err(error)
                if is_database_busy(&error) && attempt < DATABASE_BUSY_RETRY_DELAYS.len() =>
            {
                // 数据库忙且还有重试额度：首次记录告警，随后按退避序列休眠。
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
                    // 发生过重试：按最终结果记录恢复或耗尽日志。
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

/// 导入目标目录的共享写租约集合。
///
/// 业务定位：导入启动前为"源目录映射到的容器内目录"批量申请目录租约，
/// 之后所有写入该目录的文件都复用这把锁，避免逐文件申请造成开销与
/// 等待；锁覆盖判断按最长前缀匹配。
struct ImportDirectoryLocks {
    /// 目录租约守卫（无目录可锁时为 None）。
    guard: Option<Arc<crate::storage::locks::LeaseGuard>>,
    /// 已持锁目录的 (路径, 节点 ID) 列表，按路径长度升序。
    directories: Vec<LockedDirectory>,
}
impl ImportDirectoryLocks {
    /// 为已存在的导入目标目录准备可复用的共享写租约。
    /// 入参：manager/password 为目标容器与密码；sources（&[String]）每项为本地源路径；directory（&str）为容器导入根；progress（&Progress）提供取消标志；mapped_target（`Option<&str>`）为单来源显式映射。
    /// 返回值：返回 ImportDirectoryLocks；无目录源或没有已存在目标目录时 guard 为 None、directories 为空。
    /// 异常：前缀无效、开库/批量解析/目录租约申请失败返回 VeilError。
    /// 注意事项：文件源不产生目录锁；已有文件及缺失目录先过滤掉。过滤与加锁之间仍可变化，底层再次检查；不承诺为所有新增路径预先持有目录锁。
    fn acquire(
        manager: &VeilManager,
        password: &str,
        sources: &[String],
        directory: &str,
        progress: &progress::Progress,
        mapped_target: Option<&str>,
    ) -> Result<Self> {
        // paths：每个源目录对应的容器内目标前缀路径。
        let mut paths = Vec::new();
        for source in sources {
            let source = Path::new(source);
            // 只有目录源需要目录锁；文件源直接跳过。
            if !source.is_dir() {
                continue;
            }
            paths.push(target_prefix(source, directory, mapped_target)?);
        }
        // 排序去重：多个源映射到同一目录时只锁一次。
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
        // 只保留"容器内已是目录"的路径；文件/缺失路径不入锁集合。
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
        // 对刚才确认存在的目录批量申请共享写租约；两次检查间的变化仍由底层重新处理。
        let (guard, directories) =
            manager.lock_import_directories(&paths, password, progress.cancellation_flag())?;
        Ok(Self {
            guard: Some(Arc::new(guard)),
            directories,
        })
    }
    /// 为一条导入目标选择覆盖最具体的现有目录租约。
    /// 入参：self 为目录锁集合；path（&str）为规范化后的容器目标逻辑路径。
    /// 返回值：返回覆盖路径中 path_len 最大者的下标；无人覆盖为 None。
    /// 异常：无错误返回。
    /// 注意事项：遍历每个 LockedDirectory 比较路径覆盖；只选择现有描述，不重新验租约或查询目录树。
    fn index(&self, path: &str) -> Option<usize> {
        self.directories
            .iter()
            .enumerate()
            .filter(|(_, directory)| directory.covers(path))
            .max_by_key(|(_, directory)| directory.path_len())
            .map(|(index, _)| index)
    }
    /// 按分组下标借用目录租约与范围描述。
    /// 入参：self 为目录锁集合；index（`Option<usize>`）为 index 计算出的可选下标。
    /// 返回值：守卫和下标均有效时返回 Some((&LeaseGuard, &LockedDirectory))，否则 None。
    /// 异常：无错误返回，越界按 None 处理。
    /// 注意事项：返回借用只用于当前批处理；实际加密/提交仍须校验凭证。
    fn scope(
        &self,
        index: Option<usize>,
    ) -> Option<(&crate::storage::locks::LeaseGuard, &LockedDirectory)> {
        self.guard
            .as_deref()
            .zip(index.and_then(|index| self.directories.get(index)))
    }
}

/// 一条待导入文件的工作单元。
///
/// 业务定位：从遍历线程发出、流经核对/冲突/执行各阶段的最小任务对象；
/// 每次被退回重试都携带上一次的错误信息。
pub(crate) struct Work {
    /// 该文件的导入请求（源路径、目标路径、版本约束等）。
    spec: AddFileSpec,
    /// u8失败反馈次数；超过2次附带错误提示，仍可再次选择，当前代码不做溢出保护。
    retries: u8,
    /// 上次失败的错误文本（None 表示尚未失败过）。
    error: Option<String>,
}
/// 管线各线程共享的运行状态。
///
/// 业务定位：用原子/互斥量在多线程间传递"该停了""扫完了""还有几个
/// 在途""第一个错误是谁"，任一环节失败都会让全管线尽快收敛。
#[derive(Default)]
struct Shared {
    /// 协作停止信号：各线程在下个检查点停止，不会强制中断已开始的读写/系统调用。
    stop: AtomicBool,
    /// 遍历线程是否已完成来源扫描。
    scan_complete: AtomicBool,
    /// 在途工作单元计数（已转执行、尚未完成）。
    inflight: AtomicUsize,
    /// 首个取得状态锁的失败（目标路径或阶段名, 错误）；只记录一次，并发顺序由调度决定。
    failure: Mutex<Option<(String, VeilError)>>,
}
impl Shared {
    /// 判断当前线程是否还应继续推动导入。
    /// 入参：self 为 Shared，包含管线停止标志。
    /// 返回值：stop 未置位且当前线程 progress::check_cancelled 成功时为 true。
    /// 异常：不返回错误，取消检查错误折为 false。
    /// 注意事项：依赖线程已 attach 正确进度上下文；只是检查点，不能中断正在阻塞的系统调用。
    fn active(&self) -> bool {
        !self.stop.load(Ordering::Acquire) && progress::check_cancelled().is_ok()
    }
    /// 保存首先取得失败状态锁的业务错误，并通知各阶段停止。
    /// 入参：self 为共享状态；target（String）为文件路径或阶段名；error（VeilError）为原始失败。
    /// 返回值：无；首个写入的（目标、错误）保留，后续不覆盖。
    /// 异常：锁中毒通过 into_inner 恢复状态，不返回错误。
    /// 注意事项：并发失败的保留顺序取决于调度，不保证业务时间上最早；先置停止标志，再获取失败锁。
    fn fail(&self, target: String, error: VeilError) {
        self.stop.store(true, Ordering::Release);
        let mut failure = self.failure.lock().unwrap_or_else(|e| e.into_inner());
        if failure.is_none() {
            *failure = Some((target, error));
        }
    }
}
/// 线程守卫：线程 panic 时置停止信号，避免管线在无人推进时死等。
struct ThreadGuard<'a>(&'a Shared);
impl Drop for ThreadGuard<'_> {
    /// 功能：工作线程 panic 时标记导入管线停止，避免其他阶段继续等待已失去的生产者。
    /// 入参：self 为借用当前 Shared 管线状态的 ThreadGuard。
    /// 返回值：无；异常展开时把共享 stop 原子标志置为 true。
    /// 异常：不返回错误，不捕获或吞掉原 panic。
    /// 注意事项：Release 写入配合其他线程的原子读取；正常线程结束不触发停止。
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.stop.store(true, Ordering::Release);
        }
    }
}
/// 向下游转交一条工作项，队列满时等待并响应管线停止。
/// 入参：sender（&`Sender<T>`）为相应阶段通道；item（T）按值转移；shared 提供停止/线程进度取消检查。
/// 返回值：入队成功返回 Ok(())，不表示下游已完成。
/// 异常：投递取消为 Cancelled，关闭等其他发送失败为导入队列已关闭错误。
/// 注意事项：有界队列会产生等待，无界扫描队列不等容量；内部 Mutex 仍可能竞争，不能当作无锁操作。
fn send<T>(sender: &Sender<T>, item: T, shared: &Shared) -> Result<()> {
    sender
        .send_with(item, || !shared.active())
        .map_err(|error| match error {
            SendError::Cancelled(_) => VeilError::Cancelled,
            _ => invalid("导入队列已关闭"),
        })
}

/// 计算一个来源在容器内的映射前缀。
/// 入参：source（&Path）为本地来源；directory（&str）为导入根，空表示容器根；mapped（`Option<&str>`）为显式完整映射。
/// 返回值：mapped 有值直接复制；否则取来源最后一级名称并拼到 directory 下。
/// 异常：无可用 UTF-8 文件名时返回参数错误。
/// 注意事项：此函数只拼接，不做目标合法性归一；mapped 与来源数量约束由 run_mapped 检查。
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

/// 扫描本地来源，把每个普通文件变成待核对任务。
/// 入参：sources（&[String]）每项一个来源文件/目录；directory 为默认导入根；sender（&`Sender<Work>`）为无界扫描发送端；shared 为停止状态；mapped_target 为单来源可选映射。
/// 返回值：所有来源扫描并投递成功后标记进度扫描完成，返回 Ok(())。
/// 异常：来源无效、文件名/遍历/元数据/路径/投递失败或取消返回 VeilError。
/// 注意事项：只投文件、不保留空目录；目录遍历不跟随后代符号链接，但显式来源 is_file/is_dir 会跟随链接。无界积压仍消耗内存，扫描完成不代表导入提交完成。
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
/// 把扫描出的本地文件登记为一个禁止覆盖的导入任务。
/// 入参：source（&Path）为源文件；target（&str）为目标逻辑路径；sender 为扫描队列；shared 提供停止检查。
/// 返回值：目标规范化、记录发现进度并入队成功为 Ok(())。
/// 异常：目标非法、源与目标路径描述总长超过64KiB、读取源元数据或投递失败返回错误。
/// 注意事项：每条消息只存 AddFileSpec、重试计数和错误，不包含文件正文；进度在发送前增加，发送失败时该发现数不回退。
fn emit(source: &Path, target: &str, sender: &Sender<Work>, shared: &Shared) -> Result<()> {
    let target = crate::veil_ops::normalize_veil_dir(target)?;
    // 扫描队列本身不限制条目数；这里仅限制一条来源与目标路径描述的总大小。
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

/// 把已核对或已确认覆盖的文件送到执行队列，并维护在途计数。
/// 入参：work（Work）为一条任务；ready（&`Sender<Work>`）为有界执行队列；shared 为管线状态。
/// 返回值：发送成功为 Ok(())；失败撤销本次加一。
/// 异常：队列关闭或管线取消错误传播。
/// 注意事项：inflight 只跟踪执行队列/加密处理，完成待提交项由另一队列管理；它为零不等于全部事务提交完。
fn dispatch(work: Work, ready: &Sender<Work>, shared: &Shared) -> Result<()> {
    shared.inflight.fetch_add(1, Ordering::AcqRel);
    if let Err(error) = send(ready, work, shared) {
        shared.inflight.fetch_sub(1, Ordering::AcqRel);
        return Err(error);
    }
    Ok(())
}
/// 消费用户已决定的冲突，把任务转为跳过或重新执行。
/// 入参：control（&ImportConflictQueue）为决策控制队列；ready 为执行发送端；shared 为运行状态。
/// 返回值：本次所有已取决策处理成功为 Ok(())，跳过只更新进度。
/// 异常：重新投递失败返回错误；内部已决策项缺 action 会因 unwrap panic。
/// 注意事项：take_resolved 转移整批所有权；中途失败不会自动放回后续项。覆盖依核对时 base_version 决定是否仍禁止占用，用户不能提交自定义基础版本。
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

/// 批量核对容器目标占用，派发空闲项并把冲突交给用户选择。
/// 入参：manager/password 为目标容器；scan（`Receiver<Work>`）接收扫描及失败反馈；ready（`Sender<Work>`）发往加密线程；control 为冲突队列；shared 为运行状态；directory_locks 为可复用范围；policy 为批次数量/间隔。
/// 返回值：扫描耗尽、冲突处理完且执行在途数归零后正常结束。
/// 异常：取消、开库/路径批量查询、版本解析、比较取消或队列失败返回错误；普通比较失败改为冲突项 error 提示。
/// 注意事项：每批用读事务查目标；用户等待时可继续派发其他文件，冲突队列满则暂停取新项。结束不表示完成队列已提交，提交由外层协调。
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
            // 目标路径 ->（最近解析节点、可选明文哈希、是否精确命中）；缺失路径仍返回最近祖先供判断阻断。
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
                // 精确命中就是占用；未精确命中但祖先是文件，表示路径被该文件阻断，也需用户处理。
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

/// 准备同一目录锁分组的加密结果并送交完成队列，异常时逐项定位。
/// 入参：manager/password 为容器；works（`Vec<Work>`）每项为待处理文件；directory_scope 为可选复用目录租约；gate（&`Mutex<()>`）协调准备与提交；context 为完成发送端/缓存/所有者；feedback 为重核队列；shared 为停止状态。
/// 返回值：无；成功结果由 add_files_for_import 入完成队列，失败项交 retry_work。
/// 异常：不返回 Result；取消停止本组，其他整批失败先记录日志再按单项重试。底层 panic 不捕获。
/// 注意事项：这里准备加密与提交材料，不等于完成文件元数据事务；逐项回退也不是整批回滚。
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

/// 把失败文件重新放回核对队列，供重新判断目标和用户决定。
/// 入参：work 为原任务；error 为准备失败原因；feedback 为无界扫描反馈发送端；shared 为运行状态。
/// 返回值：无；取消直接结束本次处理，其他错误重置覆盖限制和基础版本后重新入队。
/// 异常：发送错误在此被忽略；不返回错误，管线是否继续依赖 shared 状态。
/// 注意事项：FileAlreadyExists/VersionConflict 前两次可直接重核，超过两次附带错误文本；其他错误立即附带文本。次数不是停止上限，用户仍可再次选择覆盖。
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

/// 执行常规多来源导入并读取完成后的容器展示视图。
/// 入参：manager/password 为目标；sources（`Vec<String>`）每项本地文件/目录；directory 为导入根；progress_state 为任务进度；control（&`Arc<ImportConflictQueue>`）接收外部冲突决策。
/// 返回值：流水线成功且重新读取元数据成功后返回 ContainerView。
/// 异常：输入、取消、文件失败或最后读取视图失败返回 ServiceError。
/// 注意事项：最后读取失败也可能发生在导入已完成之后；调用线程应已绑定进度上下文，控制队列需由外部轮询并决策。
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

/// 协调扫描、冲突核对、并行加密和主线程分批提交，支持单来源目标映射。
/// 入参：manager/password 为目标；sources（`Vec<String>`）非空且每项为来源路径；directory 为默认导入根（空为根）；progress_state 为进度与取消；control 为共享冲突控制；mapped_target（`Option<String>`）指定精确前缀，有值时仅允许一个来源。
/// 返回值：所有线程收敛、无记录失败/取消且至少发现一个文件时返回 Ok(())，最后进入 finalizing。
/// 异常：输入为空/映射多来源、无可添加文件、设置/锁/执行上下文/处理失败返回 ServiceError；取消与文件失败携带已完成及跳过数；作用域子线程 panic 可向调用者传播。
/// 注意事项：扫描无界、执行和完成队列有界；提交分批完成，不提供整个导入回滚。父线程的 progress 函数依赖正确 attach；已提交文件在取消/后续失败后保留。
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
        // 每项 QueuedImport 可包含多份已加密文件及待提交引用；容量8限制消息数，不等于最多8个文件。
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
        // 全任务共享目录缓存：容器节点只在对应目录创建事务提交后缓存，各工作线程复用。
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
                    // 与工作线程准备阶段共用 gate，防止准备/提交对同一批临时资源交叉操作；等待期间不能抢占此锁。
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
    /// 验证批量路径查找能分别标识精确命中、缺失路径和被文件节点阻断的子路径。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
