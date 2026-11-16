//! CLI 的流式传输适配：沿用同一队列流水线，由调用方负责终端交互与进度展示。
//!
//! 所属业务模块：veil-service 的「命令与传输协议封装」。它把 session 层维护的容器会话
//! （一次解锁期间持有的容器目录、容器 ID 与密码）与导入/导出流水线（多线程逐文件
//! 加密/解密）组合起来，形成 CLI 可直接调用的流式导入/导出入口。
//!
//! 文件内核心清单：
//! - `TransferResult`：一次流式传输结束后的统计结果（总数、完成、跳过、目标与密文名）；
//! - `CancelOnUnwind`：回退守卫，回调发生 panic 时自动请求取消传输；
//! - `drive`：传输主循环——后台线程干活，主线程轮询冲突队列并回调调用方逐条决策；
//! - `ContainerHandle::stream_import` / `stream_export`：面向 CLI 的导入/导出业务入口。
//!
//! 依赖能力：veil-core 的 Progress 与日志上下文，以及 service 的 ConflictQueue。冲突队列满会拒绝推送，
//! 上游流水线先按剩余容量暂停检查阶段形成背压，调用线程领取展示批次并回写选择，不是队列自身阻塞入队。
//!
//! 整体业务目标：一次调用完成「扫描 → 流式处理 → 冲突暂停等待用户决策 → 汇总结果」，
//! 用户通过 decide 回调决定跳过/覆盖/中止，通过 report 回调刷新终端进度展示。
use crate::{
    commands::{ContainerHandle, ImportConflict, ImportDecision},
    error::VeilError,
    session::{ServiceError, SessionImportAction, SessionImportResolution},
    transfer_conflicts::ConflictQueue,
};
use std::{path::Path, sync::Arc, time::Duration};
use veil_core::progress::{Progress, Snapshot};

/// 一次流式导入/导出的最终统计结果，供调用方（CLI/桌面端）展示。
pub struct TransferResult {
    /// 本次传输计划处理的文件总数（扫描完成后才稳定）。
    pub total_files: u64,
    /// 已处理文件数，包含跳过项；非跳过完成数量可在需要时用 completed_files - skipped_files 解释。
    pub completed_files: u64,
    /// 被跳过的文件数：内容相同或用户选择跳过。
    pub skipped_files: u64,
    /// 单文件导入的映射目标，跳过也可有值；目录导入或导出为空，结束时还会检查源是否仍为文件。
    pub target: Option<String>,
    /// 单文件导入成功后该文件的密文文件名（如 xxxx.enc）；导出时为空。
    pub encrypted_name: Option<String>,
}
/// 调用线程的异常展开守卫，元组借用当前传输 Progress。
/// 回调等路径 panic 时尽力请求取消，帮助工作线程到检查点停止；不保证取消一定仍可被受理。
struct CancelOnUnwind<'a>(&'a Progress);
impl Drop for CancelOnUnwind<'_> {
    /// 功能：传输控制线程异常退出时请求取消，通知后台工作停止。
    /// 入参：self 保存当前传输 Progress 的借用。
    /// 返回值：无；只有当前线程正在 panic 展开时才请求取消。
    /// 异常：不返回取消是否被接受的结果。
    /// 注意事项：正常离开不取消；请求取消不能强行终止正在进行的阻塞 I/O。
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.request_cancel();
        }
    }
}
/// 功能：在同步命令调用中协调后台传输、用户冲突选择和进度报告，等真实工作退出后返回结果。
/// 入参：operation: &'static str 为 import/export 操作标识；progress: &Progress 为共享进度；control: &`Arc<ConflictQueue<W>>` 为冲突调度状态；
/// work: FnOnce()->`Result<R,ServiceError>` + Send 为后台一次性工作；decide: FnMut(&ImportConflict)->`Result<ImportDecision,VeilError>` 在调用线程逐项作 Skip/Overwrite/Error 决策；report: FnMut(Snapshot) 在调用线程接收状态副本。
/// 返回值：Ok(R) 为后台业务结果；用户决定失败时等待正常 join 后优先返回该原因。
/// 异常：工作线程 panic 映射为 WorkError 并优先于决策失败；work 的业务错误原样返回；decide/report 的 panic 不在此捕获，线程创建失败也可能 panic。
/// 注意事项：scoped thread 不超出函数作用域，决策或报告异常展开时守卫请求取消但仍须等工作结束；取消不能强停阻塞 I/O。
/// 轮询轮间睡 50 毫秒，回调和调度还可能耗时，非严格响应上限；进度标记 cancelled 只依据是否已受理取消，不证明所有历史输出已撤回。
fn drive<W: Send, R: Send>(
    operation: &'static str,
    progress: &Progress,
    control: &Arc<ConflictQueue<W>>,
    work: impl FnOnce() -> Result<R, ServiceError> + Send,
    mut decide: impl FnMut(&ImportConflict) -> Result<ImportDecision, VeilError>,
    mut report: impl FnMut(Snapshot),
) -> Result<R, ServiceError> {
    // 在共享进度对象上登记本次传输的阶段名（import/export），并立即上报初始快照。
    let _scope = progress.begin(operation);
    report(progress.snapshot());
    // 捕获当前线程的日志上下文，由工作线程 attach 恢复，保证日志字段不丢失。
    let logging_context = veil_core::logging::capture_context();
    let result = std::thread::scope(|scope| {
        // 守卫位于调用线程，负责 decide/report 等本线程异常展开时请求取消；后台 panic 由 join 另行转换。
        let _cancel_on_unwind = CancelOnUnwind(progress);
        // 真正的导入/导出工作交给工作线程。
        let worker = scope.spawn(|| {
            let _logging = logging_context.attach();
            let _attachment = progress.attach();
            work()
        });
        // 决策错误暂存处：用户决策失败后不再处理新冲突，但先等工作线程收尾退出。
        let mut decision_error = None;
        // 主循环：工作线程结束前，反复上报进度并取走冲突批次交给用户决策。
        while !worker.is_finished() {
            report(progress.snapshot());
            // poll 取得或复用展示批次；不等新冲突到来，但仍须短暂取得冲突状态互斥锁。
            if let Some(batch) = control.poll(progress.snapshot().total_files) {
                let choices = (|| {
                    // 存放本批次中每一条冲突的最终决策，容量与冲突数一致。
                    let mut resolutions = Vec::with_capacity(batch.conflicts.len());
                    // 逐条询问用户：每条冲突对应一个目标路径的占用情况。
                    for conflict in batch.conflicts {
                        let decision = decide(&ImportConflict {
                            target: conflict.target.clone(),
                            same_content: conflict.same_content,
                            is_directory: conflict.is_directory,
                            error: conflict.error,
                        })?;
                        // 把命令层的决策映射成会话层可识别的动作；
                        // Error 决策即用户选择中止，直接以「文件已存在」错误终止本批次。
                        let action = match decision {
                            ImportDecision::Skip => SessionImportAction::Skip,
                            ImportDecision::Overwrite => SessionImportAction::Overwrite,
                            ImportDecision::Error => {
                                return Err(VeilError::FileAlreadyExists(conflict.target));
                            }
                        };
                        resolutions.push(SessionImportResolution {
                            target: conflict.target,
                            action,
                        });
                    }
                    // 把整批决策回写队列，工作线程据此继续或中止对应文件。
                    control.resolve(&batch.id, resolutions)
                })();
                // 决策过程出错（回调返回 Err 或用户选择中止）：记录错误、请求取消，
                // 跳出轮询但继续等 worker.join 收尾，避免留下游离线程。
                if let Err(error) = choices {
                    decision_error = Some(error);
                    progress.request_cancel();
                    break;
                }
            }
            // 轮询间隔 50ms：既及时响应新冲突，又避免空转占用 CPU。
            std::thread::sleep(Duration::from_millis(50));
        }
        // 等待工作线程结束并取回结果；panic 被统一映射为传输线程异常退出。
        let result = worker
            .join()
            .map_err(|_| ServiceError::Core(VeilError::WorkError("文件传输线程异常退出".into())))?;
        // join 正常取得结果后，决策错误优先于后台业务返回值；若 join 已因 panic 失败，上面的 ? 会先返回线程异常。
        if let Some(error) = decision_error {
            return Err(ServiceError::Core(error));
        }
        result
    });
    // 收尾：按是否请求取消把进度标记为 cancelled 或 success/failure，并上报最终快照。
    if progress.snapshot().cancel_requested {
        progress.finish_cancelled();
    } else {
        progress.finish(result.is_ok());
    }
    report(progress.snapshot());
    result
}
impl ContainerHandle {
    /// 功能：将一个普通文件或目录流式导入本句柄指向的容器，并把目标占用交给调用方逐条决策。
    /// 入参：self: &ContainerHandle 为已定位容器入口；source: &Path 为存在且可表示为 UTF-8 的文件/目录；destination: `Option<&str>` 为容器内目标，
    /// 文件末尾分隔符表示放入目录，目录空字符串拒绝，其他按目标映射与规范化规则处理；password: &str 为本次口令；
    /// decide: FnMut(&ImportConflict)->`Result<ImportDecision,VeilError>` 为冲突回调；report: FnMut(Snapshot) 为同步进度回调。
    /// 返回值：Ok(TransferResult)，含总量、已处理量及跳过量；单文件可附 target，确有非跳过完成项时再回查 encrypted_name。空目录的特定“无文件可添加”结果被转为成功。
    /// 异常：来源/目标、密码、流水线、冲突拒绝、取消、线程异常及完成后回查数据库失败均返回 ServiceError；回调 panic 原样传播。
    /// 注意事项：后台先 open_db 认证，再执行带文件/目录租约的导入；本方法同步等待真实收尾，不更改句柄保存的会话密码。
    /// 已提交文件在后续失败时可能保留；返回前仍会依据 source.is_file 选择单文件展示字段，调用方应保持源类型稳定。
    pub fn stream_import(
        &self,
        source: &Path,
        destination: Option<&str>,
        password: &str,
        decide: impl FnMut(&ImportConflict) -> Result<ImportDecision, VeilError>,
        report: impl FnMut(Snapshot),
    ) -> Result<TransferResult, ServiceError> {
        use crate::import::ImportError;
        // 来源必须先存在，否则没有可导入的内容。
        if !source.exists() {
            return Err(ImportError::SourceNotFound(source.to_owned()).into());
        }
        // name 是来源自身的文件名/目录名，将作为容器内的最终名称。
        let name = source
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| ImportError::InvalidDestination(source.display().to_string()))?;
        // 按来源类型推导容器内目标路径：
        // 文件 + 以 '/'/'\\' 结尾的目的地 = 把文件名拼进该目录；
        // 目录导入时 destination 是目录的新名字（不允许空串）。
        let target = if source.is_file() {
            match destination {
                Some(path) if path.ends_with(['/', '\\']) => {
                    let path = path.trim_end_matches(['/', '\\']);
                    if path.is_empty() {
                        name.to_owned()
                    } else {
                        format!("{path}/{name}")
                    }
                }
                Some(path) => path.to_owned(),
                None => name.to_owned(),
            }
        } else if source.is_dir() {
            match destination {
                Some("") => return Err(ImportError::InvalidDestination(String::new()).into()),
                Some(path) => path.trim_end_matches(['/', '\\']).to_owned(),
                None => name.to_owned(),
            }
        } else {
            return Err(ImportError::NotFile(source.to_owned()).into());
        };
        // 规范化为容器内路径（去首尾斜杠、拒绝非法片段）；目录导入到根目录时允许空串。
        let target = if target.is_empty() && source.is_dir() {
            target
        } else {
            crate::veil_ops::normalize_veil_dir(&target)
                .map_err(|_| ImportError::InvalidDestination(target.clone()))?
        };
        // 导入流水线以字符串路径接收来源（CLI 参数风格）。
        let source_text = source
            .to_str()
            .ok_or_else(|| ImportError::InvalidDestination(source.display().to_string()))?
            .to_owned();
        // 进度对象与冲突队列：驱动线程和工作线程共享。
        let progress = Progress::default();
        let control = Arc::new(ConflictQueue::default());
        // 把取消标志挂到文件租约选项上，用户取消时等待租约的线程一并退出。
        let mut options = crate::storage::locks::default_options();
        options.cancellation = Some(progress.cancellation_flag());
        let manager = self.manager().with_lock_options(options);
        let result = drive(
            "import",
            &progress,
            &control,
            || {
                // 提前认证，将错误密码直接交给 CLI 错误码映射，不包装成单文件失败。
                drop(manager.open_db(password)?);
                // 真正的导入流水线：扫描来源、加密写盘、冲突入队。
                let result = crate::import_pipeline::run_mapped(
                    &manager,
                    password,
                    vec![source_text],
                    String::new(),
                    &progress,
                    &control,
                    Some(target.clone()),
                );
                // 空文件夹视为成功（无文件可导），避免把用户误操作当成失败。
                match result {
                    Err(ServiceError::InvalidInput("选择的文件夹没有可添加的文件")) => {
                        Ok(())
                    }
                    result => result,
                }
            },
            decide,
            report,
        );
        result?;
        let snapshot = progress.snapshot();
        // 单文件且确实完成了导入时，回查数据库取新密文文件名（随机 .enc 名）用于展示。
        let encrypted_name =
            if source.is_file() && snapshot.completed_files > snapshot.skipped_files {
                manager
                    .open_db(password)?
                    .resolve(&target)?
                    .and_then(|node| node.cipher_name)
            } else {
                None
            };
        Ok(TransferResult {
            total_files: snapshot.total_files,
            completed_files: snapshot.completed_files,
            skipped_files: snapshot.skipped_files,
            // 目录导入不返回单个目标路径。
            target: source.is_file().then_some(target),
            encrypted_name,
        })
    }
    /// 功能：把容器内选择路径按导出流水线解密到外部位置，串行回调处理覆盖冲突并报告进度。
    /// 入参：self: &ContainerHandle 为容器入口；name: &str 为容器内文件/目录路径，根选择语义由流水线解释；destination: &Path 为外部目标；
    /// password: &str 为本次解锁口令；decide 为逐冲突 Skip/Overwrite/Error 决策回调；report 为 Snapshot 展示回调，均在调用线程执行。
    /// 返回值：Ok(TransferResult)，总数、已处理数和跳过数来自最终进度，target 与 encrypted_name 固定为 None。
    /// 异常：密码/源路径、范围租约、目标/认证/发布、冲突拒绝、取消或工作线程异常通过 ServiceError 返回；回调 panic 不统一转换。
    /// 注意事项：同步等待所有实际工作收尾；需要明确选择 Overwrite 才继续覆盖，并且发布时还要核对目标状态。
    /// 失败或取消不撤销已发布文件；未发布临时明文按流水线资源清理策略处理，不能据返回错误推断外部目录毫无变化。
    pub fn stream_export(
        &self,
        name: &str,
        destination: &Path,
        password: &str,
        decide: impl FnMut(&ImportConflict) -> Result<ImportDecision, VeilError>,
        report: impl FnMut(Snapshot),
    ) -> Result<TransferResult, ServiceError> {
        // 与导入对称：进度对象、冲突队列、带取消标志的租约选项。
        let progress = Progress::default();
        let control = Arc::new(ConflictQueue::default());
        let mut options = crate::storage::locks::default_options();
        options.cancellation = Some(progress.cancellation_flag());
        let manager = self.manager().with_lock_options(options);
        drive(
            "export",
            &progress,
            &control,
            || {
                // 提前认证密码；随后执行真正的导出流水线。
                drop(manager.open_db(password)?);
                crate::export_pipeline::run(
                    &manager,
                    password,
                    vec![name.to_owned()],
                    destination,
                    &progress,
                    &control,
                    true,
                )?;
                Ok(())
            },
            decide,
            report,
        )?;
        // 导出只关心数量统计，目标路径与密文名固定为空。
        let snapshot = progress.snapshot();
        Ok(TransferResult {
            total_files: snapshot.total_files,
            completed_files: snapshot.completed_files,
            skipped_files: snapshot.skipped_files,
            target: None,
            encrypted_name: None,
        })
    }
}
