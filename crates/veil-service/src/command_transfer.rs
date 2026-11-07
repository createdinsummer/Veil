//! CLI 的流式传输适配：沿用同一队列流水线，由调用方负责终端交互与进度展示。
use crate::{
    commands::{ContainerHandle, ImportConflict, ImportDecision},
    error::VeilError,
    session::{ServiceError, SessionImportAction, SessionImportResolution},
    transfer_conflicts::ConflictQueue,
};
use std::{path::Path, sync::Arc, time::Duration};
use veil_core::progress::{Progress, Snapshot};

pub struct TransferResult {
    pub total_files: u64,
    pub completed_files: u64,
    pub skipped_files: u64,
    pub target: Option<String>,
    pub encrypted_name: Option<String>,
}
struct CancelOnUnwind<'a>(&'a Progress);
impl Drop for CancelOnUnwind<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.request_cancel();
        }
    }
}
fn drive<W: Send, R: Send>(
    operation: &'static str,
    progress: &Progress,
    control: &Arc<ConflictQueue<W>>,
    work: impl FnOnce() -> Result<R, ServiceError> + Send,
    mut decide: impl FnMut(&ImportConflict) -> Result<ImportDecision, VeilError>,
    mut report: impl FnMut(Snapshot),
) -> Result<R, ServiceError> {
    let _scope = progress.begin(operation);
    report(progress.snapshot());
    let result = std::thread::scope(|scope| {
        let _cancel_on_unwind = CancelOnUnwind(progress);
        let worker = scope.spawn(|| {
            let _attachment = progress.attach();
            work()
        });
        let mut decision_error = None;
        while !worker.is_finished() {
            report(progress.snapshot());
            if let Some(batch) = control.poll(progress.snapshot().total_files) {
                let choices = (|| {
                    let mut resolutions = Vec::with_capacity(batch.conflicts.len());
                    for conflict in batch.conflicts {
                        let decision = decide(&ImportConflict {
                            target: conflict.target.clone(),
                            same_content: conflict.same_content,
                            is_directory: conflict.is_directory,
                            error: conflict.error,
                        })?;
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
                    control.resolve(&batch.id, resolutions)
                })();
                if let Err(error) = choices {
                    decision_error = Some(error);
                    progress.request_cancel();
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let result = worker
            .join()
            .map_err(|_| ServiceError::Core(VeilError::WorkError("文件传输线程异常退出".into())))?;
        if let Some(error) = decision_error {
            return Err(ServiceError::Core(error));
        }
        result
    });
    if progress.snapshot().cancel_requested {
        progress.finish_cancelled();
    } else {
        progress.finish(result.is_ok());
    }
    report(progress.snapshot());
    result
}
impl ContainerHandle {
    pub fn stream_import(
        &self,
        source: &Path,
        destination: Option<&str>,
        password: &str,
        decide: impl FnMut(&ImportConflict) -> Result<ImportDecision, VeilError>,
        report: impl FnMut(Snapshot),
    ) -> Result<TransferResult, ServiceError> {
        use crate::import::ImportError;
        if !source.exists() {
            return Err(ImportError::SourceNotFound(source.to_owned()).into());
        }
        let name = source
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| ImportError::InvalidDestination(source.display().to_string()))?;
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
        let target = if target.is_empty() && source.is_dir() {
            target
        } else {
            crate::veil_ops::normalize_veil_dir(&target)
                .map_err(|_| ImportError::InvalidDestination(target.clone()))?
        };
        let source_text = source
            .to_str()
            .ok_or_else(|| ImportError::InvalidDestination(source.display().to_string()))?
            .to_owned();
        let progress = Progress::default();
        let control = Arc::new(ConflictQueue::default());
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
                let result = crate::import_pipeline::run_mapped(
                    &manager,
                    password,
                    vec![source_text],
                    String::new(),
                    &progress,
                    &control,
                    Some(target.clone()),
                );
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
            target: source.is_file().then_some(target),
            encrypted_name,
        })
    }
    pub fn stream_export(
        &self,
        name: &str,
        destination: &Path,
        password: &str,
        decide: impl FnMut(&ImportConflict) -> Result<ImportDecision, VeilError>,
        report: impl FnMut(Snapshot),
    ) -> Result<TransferResult, ServiceError> {
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
