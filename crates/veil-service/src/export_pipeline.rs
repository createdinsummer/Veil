//! 明文导出流水线：单线程分页遍历、批量核对、有界解密和批量发布。
//! 已发布文件在失败或取消后保留；未发布临时明文随资源释放尽力删除，不保证安全擦除或崩溃后清理。
//!
//! 所属模块：服务层的容器到普通文件系统导出流程。
//! 核心数据：Work 是用户选择的源路径与外部目标；Ready 是通过冲突检查的节点；Staged 保存
//! 已解密到同目录临时文件的结果；Shared 负责跨线程停止和首个失败原因；Stamp 检测外部目标变化。
//! 核心阶段：enumerate 分页发现目录树；check 重新读取源并识别覆盖冲突；process 解密为临时明文；
//! publish 校验锁与版本后逐项提交；run 负责锁、无界扫描队列、有界执行/完成队列和线程编排。
//! 依赖能力：容器数据库与范围锁、冲突审核队列、版本比较、并发队列、进度取消、密钥解密、
//! 安全文件发布和目录同步缓存。整体目标是限制内存占用、允许并行解密并清理未发布的明文；
//! 中途失败时已发布目标会保留，因为普通文件系统无法提供整批回滚。
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
/// 一项待导出的业务任务：容器内路径与外部目标路径的对应关系。
#[derive(Clone)]
pub(crate) struct Work {
    /// 容器内源路径；空字符串代表容器根目录。
    source: String,
    /// 用户指定的外部文件或目录目标。
    target: PathBuf,
    /// 冲突检查时目标文件的快照；覆盖前再次读取以防目标已变化。
    expected: Option<Stamp>,
    /// 是否由用户在冲突审核中明确选择覆盖。
    overwrite: bool,
}
/// 外部目标的轻量状态快照，用于检测检查和覆盖之间的变化。
#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    /// 文件字节长度。
    len: u64,
    /// 文件修改时间；文件系统未提供时为空。
    modified: Option<std::time::SystemTime>,
    /// Unix身份标记：设备号、inode、状态变更时间ctime的秒/纳秒；ctime不是文件创建时间。
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}
impl Stamp {
    /// 记录外部目标文件的可观察状态，供确认覆盖前后比较。
    /// 入参：path（&Path）为准备导出的目标文件位置。
    /// 返回值：普通文件返回 Some(Stamp)，含长度、可选修改时间，Unix 另含设备号/inode/ctime；路径缺失返回 None。
    /// 异常：符号链接或非普通文件、其他元数据读取错误返回 VeilError。
    /// 注意事项：用 symlink_metadata 检查最终入口，不跟随最终链接；不是内容哈希或原子覆盖条件，读取后目标仍可改变。
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
/// 已通过冲突检查的任务及其容器源节点元数据。
struct Ready {
    /// 源路径、输出目标、覆盖决定和目标快照。
    work: Work,
    /// 容器节点类型、版本和密钥索引等信息。
    node: Node,
}
/// 解密阶段完成后等待发布的任务。
struct Staged {
    /// 与暂存明文对应的容器源和外部目标。
    ready: Ready,
    /// 同一父目录里的临时明文；目录任务没有临时文件。
    temporary: Option<tempfile::TempPath>,
}
/// 各流水线线程共享的停止信号和首个失败原因。
#[derive(Default)]
struct Shared {
    /// 任一阶段失败或用户取消时置位，通知其他阶段停止。
    stop: AtomicBool,
    /// 首先取得互斥锁的错误；并发顺序由调度决定，可能先保存连锁取消。
    failure: Mutex<Option<VeilError>>,
}
impl Shared {
    /// 判断当前线程是否应停止导出阶段。
    /// 入参：self（&Shared）为全流程共享状态。
    /// 返回值：内部 stop 已置位或当前线程进度取消检查失败时为 true。
    /// 异常：无 Result 错误返回。
    /// 注意事项：依赖当前线程已 attach 正确进度上下文；检查不能抢占正在进行的系统调用。
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire) || progress::check_cancelled().is_err()
    }
    /// 广播导出停止，并保留首先取得失败锁的错误。
    /// 入参：self 为共享状态；error（VeilError）为某一阶段失败。
    /// 返回值：无；仅首次写入 failure，后续错误不覆盖。
    /// 异常：Mutex 中毒通过 into_inner 取回状态；本函数不返回错误。
    /// 注意事项：并发顺序由调度决定，保留项可能为连锁取消；最终 run 还优先返回当前进度取消。
    fn fail(&self, error: VeilError) {
        self.stop.store(true, Ordering::Release);
        let mut failure = self.failure.lock().unwrap_or_else(|e| e.into_inner());
        if failure.is_none() {
            *failure = Some(error);
        }
    }
}
/// 线程异常展开时自动广播停止，避免其他阶段继续等待失效生产者。
struct ThreadGuard<'a>(&'a Shared);
impl Drop for ThreadGuard<'_> {
    /// 在线程异常展开时通知其他导出阶段停止。
    /// 入参：self（&mut ThreadGuard）借用对应 Shared。
    /// 返回值：无；正常退出不置停止标志，panic 展开时置位。
    /// 异常：不捕获或吞掉原 panic。
    /// 注意事项：只发协作停止信号，不记录原始 panic 为 VeilError；作用域线程仍会把 panic 传播给调用者。
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.stop.store(true, Ordering::Release);
        }
    }
}
/// 把扫描、待解密或暂存结果转交下游，并在等待容量时响应停止。
/// 入参：queue（&`Sender<T>`）为对应阶段通道；item（T）按值移交；shared 为停止状态。
/// 返回值：成功入队返回 Ok(())，不表示下游处理完成。
/// 异常：所有发送失败（含关闭、取消）统一返回 VeilError::Cancelled。
/// 注意事项：有界阶段需有消费者并行推进；扫描的无界通道不等容量，但仍需内部互斥锁。发送失败释放工作项时会尝试清理其临时文件。
fn send<T>(queue: &Sender<T>, item: T, shared: &Shared) -> Result<()> {
    queue
        .send_with(item, || shared.stopped())
        .map_err(|_| VeilError::Cancelled)
}
/// 为外部目标寻找创建临时文件及同步目录使用的位置。
/// 入参：path（&Path）为输出路径。
/// 返回值：有非空父目录时返回其借用，否则返回相对路径“.”。
/// 异常：无错误返回。
/// 注意事项：不解析绝对路径、不检查目录存在或是否为符号链接。
fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}
/// 在导出前拒绝已观察到的符号链接祖先或非目录祖先。
/// 入参：path（&Path）为待创建/发布的外部目标。
/// 返回值：所有上级当前为真实目录或尚不存在时返回 Ok(())。
/// 异常：上级类型不符或元数据查询错误返回 VeilError，NotFound 允许后续创建。
/// 注意事项：不检查目标自身，且不能消除检查与创建/替换之间的外部竞态；不是基于目录句柄的隔离沙箱。
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
/// 分页遍历所选容器子树，生成保持相对层级的导出工作项。
/// 入参：manager/password 为容器访问；roots（`Vec<(String, Node, PathBuf)>`）每项为源逻辑路径、初始节点、对应外部目标；queue（`Sender<Work>`）为无界扫描发送端；shared 为停止状态。
/// 返回值：全部根节点及其后代入队，并设置扫描完成进度后返回 Ok(())。
/// 异常：开库、目录分页、队列发送或取消失败返回错误。
/// 注意事项：每级保留一页及递归栈，扫描队列本身无界；不在这里维持数据库读事务，依赖外层目录读租约与后续源复核。
fn enumerate(
    manager: &VeilManager,
    password: &str,
    roots: Vec<(String, Node, PathBuf)>,
    queue: Sender<Work>,
    shared: &Shared,
) -> Result<()> {
    let db = manager.open_db(password)?;
    // 仅保留当前深度的分页游标；无界队列承接发现的工作，不预载整个目录树。
    /// 把一个文件或目录任务入队，再按名称游标递归发现其子项。
    /// 入参：db（&ContainerDb）为遍历连接；source（String）为源逻辑路径，空为根；node（Node）为此层节点；target（PathBuf）为对应输出；queue 为扫描发送端；shared 为停止状态。
    /// 返回值：当前节点及所有可遍历后代投递完返回 Ok(())。
    /// 异常：分页/投递/取消失败返回错误；递归过深可能耗尽调用栈，不在此捕获。
    /// 注意事项：先投父目录再投后代，但并行解密和发布不保证父任务先完成，目录缓存承担必要的父目录创建。每页最多128项，以末项名称推进游标。
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
                // 以本页最后一个名字作为下一页游标，避免重复读取同级条目。
                after = page.last().map(|n| n.name.clone());
                // 每轮保留容器路径和输出路径的相同目录层级，再递归处理当前子节点。
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
    // 多个用户选择项分别作为遍历起点，结果汇入同一个下游队列。
    for (source, node, target) in roots {
        visit(&db, source, node, target, &queue, shared)?;
    }
    progress::scan_finished();
    Ok(())
}
/// 把用户确认的导出冲突转为跳过或待解密任务。
/// 入参：control（&`ConflictQueue<Work>`）为审核控制；output（&`Sender<Ready>`）为解密输入；db 为容器连接；shared 为停止状态。
/// 返回值：本次已取决定全部处理后返回 Ok(())；跳过文件计入完成/跳过进度。
/// 异常：源无法精确解析、缺少 action、SQL 或发送失败返回 VeilError。
/// 注意事项：取走的整批决定不自动回队；即使选择 Skip 也先解析源。跳过目录不取消其已独立扫描的后代；覆盖沿用最初目标 Stamp，不重置用户确认依据。
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
    // 每项决定都重新解析源路径，确保等待用户处理期间容器内对象仍然一致。
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
            // 跳过文件时标记该文件已处理；跳过目录本身不表示其所有子文件已完成。
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
/// 核对源节点和外部目标，决定直接解密或等待用户确认。
/// 入参：manager/password 为容器；guard/directory 为可复用的目录读租约及范围；input（`Receiver<Work>`）接收扫描项；output（`Sender<Ready>`）交给解密线程；control 为冲突队列；interactive（bool）允许等用户选择；shared/policy 为停止及批次策略。
/// 返回值：输入关闭且审核队列清空，监听剩余批次处理后返回 Ok(())。
/// 异常：源缺失/变化、部分目标状态读取、数据库/发送/取消错误传播；非交互下目标冲突直接报错。
/// 注意事项：相同内容也交用户选择；部分比较/上级检查错误变成 ConflictView.error，类型不兼容以 is_directory 表示。控制队列满暂停新取项，目录冲突不自动跳过整棵子树。
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
        // 批次策略控制读取节奏；监听回调会为已解决的审核任务腾出缓冲空间。
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
            // 一次批量解析本批源路径，避免每个文件分别查询容器数据库。
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
                // 先验证目标父路径，再读取目标节点元数据；符号链接不作为可安全覆盖目标。
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
                            // 对比源和目标内容，判断可跳过的相同文件，或需要用户决定的冲突。
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
                // 已占用、不兼容或检查失败的目标都要审核；非交互运行无法询问，因此直接报错。
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

/// 并行把已审核文件解密到目标同目录的临时明文，交给发布阶段。
/// 入参：manager 定位密文；keys（&`HashMap<String, Zeroizing<[u8; 32]>>`）为 key_id 到32字节数据密钥；directories 为本任务输出目录缓存；input（`Receiver<Ready>`）为待处理项；output（`Sender<Staged>`）为有界暂存结果；shared 为停止状态。
/// 返回值：输入关闭且本线程处理完后返回 Ok(())；目录任务不产生临时文件。
/// 异常：目录/密钥/密文/认证/同步/队列或取消失败返回错误；文件节点缺 size 的内部不变量破坏会 unwrap panic。
/// 注意事项：临时文件包含明文，释放仅尽力删除而非安全擦除；可能提前创建输出父目录，失败不回滚这些目录。下游限消息数，不限制明文总字节。
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
            // 每个消费者逐个解密；有界队列限制待发布项数，另有各工作线程和发布批次持有的临时文件。
            for ready in batch {
                check_parents(&ready.work.target)?;
                directories.ensure_filesystem(parent(&ready.work.target))?;
                let temporary = if ready.node.kind == "file" {
                    progress::begin_file(&ready.work.source);
                    let mut temporary =
                        tempfile::NamedTempFile::new_in(parent(&ready.work.target))?;
                    // 由文件节点的 key_id 选择这份文件的专属密钥，不使用容器级密钥替代。
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

/// 复核源与租约后，把一批暂存明文逐项发布到最终外部位置。
/// 入参：db（&mut ContainerDb）为发布线程连接；guard（&LeaseGuard）为覆盖所有来源的读租约；directories 为目录缓存；batch（`Vec<Staged>`）每项为文件暂存或目录任务。
/// 返回值：本批发布后租约再次验证通过返回 Ok(())。
/// 异常：取消、租约/源节点变化、目标 Stamp 改变、类型不符、目录创建或文件发布失败返回错误。
/// 注意事项：源节点和租约在短读事务内核对，文件发布在事务外。覆盖用 persist，非覆盖用共享发布函数；Stamp 检查和替换不原子。发布后目录同步失败只告警，最终验租约失败也不撤销已发布目标。
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
        // 在同一短事务内批量读取源节点，确认待导出明文仍对应当前元数据版本。
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
    // 逐项提交外部目标；错误不回滚已发布文件，文件进度仅在发布成功后增加。
    for staged in batch {
        progress::check_cancelled()?;
        let target = &staged.ready.work.target;
        check_parents(target)?;
        if let Some(temporary) = staged.temporary {
            // 覆盖前检测可观察的目标变化；检查与 persist 分开执行，外部仍可能在两者间替换目标。
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
                // tempfile 的 persist_noclobber 依赖 renamex_np(RENAME_EXCL)，
                // 在 exFAT（fskit）、SMB 等文件系统上返回 ENOTSUP。统一走共享发布
                // 函数：其内部遇到 ENOTSUP 时回退为「排他创建 + 复制」，同样不会
                // 覆盖导出目标；发布失败时临时明文仍由 TempPath 负责清理。
                crate::fsutil::publish_staged_file(temporary.as_ref(), target).map_err(
                    |error| {
                        if progress::is_cancelled_io(&error) {
                            VeilError::Cancelled
                        } else {
                            VeilError::Io(error)
                        }
                    },
                )?;
            }
            // 文件已经发布，目录同步在个别文件系统上可能不受支持，按尽力而为处理。
            if let Err(error) = crate::fsutil::sync_directory(parent(target)) {
                veil_core::logging::failure(
                    veil_core::logging::Level::Warn,
                    "service",
                    "export_publish",
                    "PUBLISHED_DIRECTORY_SYNC_FAILED",
                    &error,
                    &[("destination", veil_core::logging::Field::Path(target))],
                );
            }
            progress::file_done(false);
        } else {
            // 目录目标遵循不覆盖规则：遇到普通文件或符号链接占位时停止，不替换它们。
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

/// 协调容器到外部文件系统的分阶段导出，保留已发布成果并收敛各线程。
/// 入参：manager/password 为容器；names（`Vec<String>`）非空，每项容器路径，空字符串可代表根；destination（&Path）为非空输出位置；progress（&Progress）提供任务进度；control（&`Arc<ConflictQueue<Work>>`）供外部处理冲突；interactive（bool）决定是否等待审核。
/// 返回值：所有阶段成功结束返回 Ok(())，不额外返回文件清单。
/// 异常：输入/源/输出范围无效、读租约、密码/密钥/数据库/认证/I/O/阶段失败返回 VeilError；最终取消优先于保存错误，子线程 panic 可传播。
/// 注意事项：先去重选择并去除已选祖先下的重复项，申请覆盖全部来源的公共目录读租约。单文件把 destination 当精确文件名，多项拼顶层名称。扫描无界，解密/完成有界；已发布文件和已建目录不整体回滚，未发布临时文件只尽力清理。调用线程须绑定进度上下文。
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
    // 规范化并去重所选容器路径，再处理目录与子项重复选择的情况。
    let mut names = names
        .into_iter()
        .map(|n| normalize_veil_dir(&n))
        .collect::<Result<Vec<_>>>()?;
    names.sort();
    names.dedup();
    let all = names.clone();
    // 同时选择目录和其子项时只保留目录，递归扫描会自然包含子项。
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
    // 为所有选择计算公共目录范围；目录节点锁定自己，文件节点锁定其父目录。
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
    // 将候选路径逐级提升到共同祖先，让一把读锁覆盖全部导出源。
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
    // 在关闭初始数据库句柄后取得读租约，保护后续核对、解密和发布全过程。
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
    // 将相对输出位置锚定到当前目录，随后统一做组件清理和系统路径规范化。
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
    // 目标末尾尚不存在的路径组件，先倒序收集；已存在前缀 canonicalize 后再逆序拼回。
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
    // 单文件精确导出可直接使用目标路径；多项导出保留每个顶层节点名称。
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
    // 最终顶层输出路径集合：每个用户根选择加入一次，拒绝不同来源映射到同一个顶层目标。
    let mut targets = std::collections::HashSet::new();
    if roots
        .iter()
        .any(|(_, _, target)| !targets.insert(target.clone()))
    {
        return Err(invalid("选择的项目包含同名根目录或同名文件，请分别导出"));
    }
    // 本次导出解密密钥表：key_id -> 密钥副本；各解密线程借用同一表，结束释放时逐份零化。
    let keys = db
        .keys()?
        .iter()
        .map(|k| Ok((k.key_id.clone(), k.material()?)))
        .collect::<Result<HashMap<_, _>>>()?;
    drop(db);
    // 扫描仅发送路径/节点任务描述，不受容量限制；解密与发布并行，临时明文存于磁盘。
    let policy = load_policy()?;
    let (pending_tx, pending_rx) = UnboundedQueue::new().split();
    // 最多排队32项 Ready（文件或目录）；此外各工作线程可各自持有一项。
    let (ready_tx, ready_rx) = BoundedQueue::new(32).split();
    // 最多排队8项 Staged；每项持暂存路径而非完整明文字节，发布监听器还可能留有一批待处理项。
    let (done_tx, done_rx) = BoundedQueue::new(8).split();
    let mut publisher_db = manager.open_db(password)?;
    let directories = crate::directory_cache::DirectoryCache::default();
    let shared = Shared::default();
    let workers = progress::worker_count(32);
    progress::stage("processing", Some(0));
    progress::set_parallel_workers(workers);
    let logging_context = veil_core::logging::capture_context();
    // 当前线程持有发布端；作用域线程分别负责扫描、冲突检查和并行解密，并共享停止信号。
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
