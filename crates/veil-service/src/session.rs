//! 面向调用方的容器会话请求入口。

use crate::veil_ops::VeilManager;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
use veil_core::metadata::{MetaData, generate_veil_id};
use zeroize::Zeroizing;

/// 接收来自命令行、桌面端或移动端的业务请求。
#[derive(Clone)]
pub struct VeilService(
    Arc<Mutex<Option<Session>>>,
    veil_core::progress::Progress,
    Arc<Mutex<()>>,
    Arc<RwLock<Option<PathBuf>>>,
    Arc<Mutex<Option<Arc<crate::import_pipeline::ImportConflictQueue>>>>,
    Arc<Mutex<Option<Arc<crate::transfer_conflicts::ConflictQueue<crate::export_pipeline::Work>>>>>,
    Arc<Mutex<Option<Vec<std::fs::File>>>>,
    Arc<crate::deletion_queue::PhysicalDeletionQueue>,
    Arc<GarbageCollectionQueue>,
);

impl Default for VeilService {
    fn default() -> Self {
        let session = Arc::new(Mutex::new(None));
        Self(
            Arc::clone(&session),
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            Arc::new(GarbageCollectionQueue::new(session)),
        )
    }
}

impl Drop for VeilService {
    fn drop(&mut self) {
        if Arc::strong_count(&self.0) == 1 {
            if let Ok(session) = self.0.lock() {
                if let Some(session) = session.as_ref() {
                    veil_core::database::invalidate(
                        &session.path.join(veil_core::database::CONTAINER_DB),
                    );
                }
            }
        }
    }
}

struct StreamingImportRegistration<'a>(
    &'a Mutex<Option<Arc<crate::import_pipeline::ImportConflictQueue>>>,
);
impl Drop for StreamingImportRegistration<'_> {
    fn drop(&mut self) {
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) = None;
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionImportAction {
    Skip,
    Overwrite,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionImportResolution {
    pub target: String,
    pub action: SessionImportAction,
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct ImportKey {
    container: PathBuf,
    source_paths: Vec<PathBuf>,
    destination: String,
}

fn import_registry() -> Arc<Mutex<HashSet<ImportKey>>> {
    static REGISTRY: OnceLock<Arc<Mutex<HashSet<ImportKey>>>> = OnceLock::new();
    Arc::clone(REGISTRY.get_or_init(|| Arc::new(Mutex::new(HashSet::new()))))
}

struct ImportGuard {
    active: Arc<Mutex<HashSet<ImportKey>>>,
    key: ImportKey,
}
impl Drop for ImportGuard {
    fn drop(&mut self) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        active.remove(&self.key);
    }
}

/// 容器会话请求；密码只用于本次操作，不应写入日志。
pub enum ServiceRequest<'a> {
    CheckExecution {
        owner: &'a crate::execution::ExecutionOwner,
        endpoint: Option<&'a crate::execution::ExecutionEndpoint>,
        timeout: std::time::Duration,
    },
    CreateContainer {
        parent: String,
        name: String,
        password: String,
    },
    OpenContainer {
        path: String,
        password: String,
    },
    RefreshContainer,
    LockContainer,
    ListSpaces,
    ListRecents,
    GetUserSetting {
        key: &'a str,
    },
    SetUserSetting {
        key: &'a str,
        value: &'a serde_json::Value,
    },
    RenameSpace {
        id: &'a str,
        name: &'a str,
    },
    SetDefaultSpace {
        id: &'a str,
    },
    DeleteSpace {
        id: String,
    },
    DeleteSpaceRecord {
        id: String,
    },
    DeleteContainer {
        id: String,
    },
    UnregisterContainer {
        id: &'a str,
    },
    MoveContainer {
        id: &'a str,
        target_space: &'a str,
        password: &'a str,
    },
    CreateSpace {
        name: String,
        path: Option<PathBuf>,
    },
    CreateInSpace {
        space: String,
        name: String,
        password: String,
    },
    ImportSessionFiles {
        sources: Vec<String>,
        directory: String,
    },
    StreamSessionImport {
        sources: Vec<String>,
        directory: String,
    },
    PollSessionImportConflicts,
    ResolveSessionImportConflicts {
        id: String,
        resolutions: Vec<SessionImportResolution>,
    },
    PreviewSessionFile {
        name: String,
    },
    ExportSessionFile {
        name: String,
        destination: String,
    },
    StreamSessionExport {
        names: Vec<String>,
        destination: String,
    },
    PollSessionExportConflicts,
    ResolveSessionExportConflicts {
        id: String,
        resolutions: Vec<SessionImportResolution>,
    },
    ExportSessionPackage {
        destination: String,
    },
    ImportPackageInSpace {
        path: String,
        space: String,
        password: String,
    },
    RemoveSessionPath {
        name: String,
    },
    OperateContainer {
        directory: &'a Path,
        veil_id: &'a str,
        operation: crate::operations::ContainerOperation<'a>,
    },
    PrepareContainer {
        selector: &'a str,
    },
    OperatePrepared {
        container: &'a crate::commands::ContainerHandle,
        operation: crate::operations::ContainerOperation<'a>,
    },
    PrepareImport {
        selector: &'a str,
        source: &'a Path,
        destination: Option<&'a str>,
    },
    CommitImport {
        plan: crate::commands::ImportPlan,
        password: &'a str,
    },
    PrepareContainerPack {
        selector: &'a str,
        output: Option<&'a str>,
    },
    CommitContainerPack {
        plan: crate::commands::PackPlan,
        password: &'a str,
    },
    ResolveContainer {
        selector: &'a str,
    },
    ReadConfigSummary,
    RegisterContainer {
        directory: &'a Path,
        password: &'a str,
    },
    ConfigPath,
    GuessMime {
        path: &'a str,
    },
    EffectiveHintLevel {
        override_value: Option<&'a str>,
    },
    ShouldShowHint {
        kind: crate::hints::HintKind,
        override_value: Option<&'a str>,
    },
    SetHintLevel {
        level: &'a str,
    },
    PrepareLink {
        target: &'a str,
        output: Option<&'a Path>,
    },
    CommitLink {
        plan: crate::link::LinkPlan,
    },
    CollectImport {
        source: &'a Path,
        destination: Option<&'a str>,
    },
    PreparePack {
        name: &'a str,
        output: Option<&'a str>,
    },
    ValidateNewPassword {
        password: &'a str,
    },
    PrepareCreate {
        options: crate::workflow::CreateOptions<'a>,
    },
    CommitCreate {
        plan: crate::workflow::CreatePlan,
        password: &'a str,
    },
    PrepareUnpack {
        options: crate::workflow::UnpackOptions<'a>,
    },
    CommitUnpack {
        plan: crate::workflow::UnpackPlan,
        password: &'a str,
    },
}

impl ServiceRequest<'_> {
    /// 只返回变体名称，禁止格式化请求参数。
    fn log_operation(&self) -> &'static str {
        match self {
            Self::CheckExecution { .. } => "CheckExecution",
            Self::CreateContainer { .. } => "CreateContainer",
            Self::OpenContainer { .. } => "OpenContainer",
            Self::RefreshContainer => "RefreshContainer",
            Self::LockContainer => "LockContainer",
            Self::ListSpaces => "ListSpaces",
            Self::ListRecents => "ListRecents",
            Self::GetUserSetting { .. } => "GetUserSetting",
            Self::SetUserSetting { .. } => "SetUserSetting",
            Self::RenameSpace { .. } => "RenameSpace",
            Self::SetDefaultSpace { .. } => "SetDefaultSpace",
            Self::DeleteSpace { .. } => "DeleteSpace",
            Self::DeleteSpaceRecord { .. } => "DeleteSpaceRecord",
            Self::DeleteContainer { .. } => "DeleteContainer",
            Self::UnregisterContainer { .. } => "UnregisterContainer",
            Self::MoveContainer { .. } => "MoveContainer",
            Self::CreateSpace { .. } => "CreateSpace",
            Self::CreateInSpace { .. } => "CreateInSpace",
            Self::ImportSessionFiles { .. } => "ImportSessionFiles",
            Self::StreamSessionImport { .. } => "StreamSessionImport",
            Self::PollSessionImportConflicts => "PollSessionImportConflicts",
            Self::ResolveSessionImportConflicts { .. } => "ResolveSessionImportConflicts",
            Self::PreviewSessionFile { .. } => "PreviewSessionFile",
            Self::ExportSessionFile { .. } => "ExportSessionFile",
            Self::StreamSessionExport { .. } => "StreamSessionExport",
            Self::PollSessionExportConflicts => "PollSessionExportConflicts",
            Self::ResolveSessionExportConflicts { .. } => "ResolveSessionExportConflicts",
            Self::ExportSessionPackage { .. } => "ExportSessionPackage",
            Self::ImportPackageInSpace { .. } => "ImportPackageInSpace",
            Self::RemoveSessionPath { .. } => "RemoveSessionPath",
            Self::OperateContainer { .. } => "OperateContainer",
            Self::PrepareContainer { .. } => "PrepareContainer",
            Self::OperatePrepared { .. } => "OperatePrepared",
            Self::PrepareImport { .. } => "PrepareImport",
            Self::CommitImport { .. } => "CommitImport",
            Self::PrepareContainerPack { .. } => "PrepareContainerPack",
            Self::CommitContainerPack { .. } => "CommitContainerPack",
            Self::ResolveContainer { .. } => "ResolveContainer",
            Self::ReadConfigSummary => "ReadConfigSummary",
            Self::RegisterContainer { .. } => "RegisterContainer",
            Self::ConfigPath => "ConfigPath",
            Self::GuessMime { .. } => "GuessMime",
            Self::EffectiveHintLevel { .. } => "EffectiveHintLevel",
            Self::ShouldShowHint { .. } => "ShouldShowHint",
            Self::SetHintLevel { .. } => "SetHintLevel",
            Self::PrepareLink { .. } => "PrepareLink",
            Self::CommitLink { .. } => "CommitLink",
            Self::CollectImport { .. } => "CollectImport",
            Self::PreparePack { .. } => "PreparePack",
            Self::ValidateNewPassword { .. } => "ValidateNewPassword",
            Self::PrepareCreate { .. } => "PrepareCreate",
            Self::CommitCreate { .. } => "CommitCreate",
            Self::PrepareUnpack { .. } => "PrepareUnpack",
            Self::CommitUnpack { .. } => "CommitUnpack",
        }
    }
}

/// 业务请求的结果。
pub enum ServiceResponse {
    ExecutionStatus(crate::execution::Liveness),
    Container(ContainerView),
    Locked,
    Spaces(Vec<crate::spaces::SpaceView>),
    Recents(Vec<crate::storage::app::RecentContainer>),
    Setting(serde_json::Value),
    Preview(Vec<u8>),
    PreviewUnstable(Vec<u8>),
    ExportUnstable,
    Done,
    Operation(crate::operations::ContainerOperationResult),
    ContainerPrepared(crate::commands::ContainerHandle),
    ImportPrepared(crate::commands::ImportPlan),
    Imported(crate::commands::ImportResult),
    ContainerPackPrepared(crate::commands::PackPlan),
    ContainerPacked(crate::commands::PackResult),
    Resolved(crate::config::ResolvedVeil),
    ConfigSummary(crate::config::ConfigSummary),
    EffectiveHintLevel(crate::config::HintLevel),
    ShowHint(bool),
    HintLevelSet(crate::config::HintLevel),
    ConfigPath(PathBuf),
    Mime(Option<String>),
    CreatePrepared(crate::workflow::CreatePlan),
    Created(crate::workflow::CreatedContainer),
    UnpackPrepared(crate::workflow::UnpackPlan),
    Unpacked(crate::workflow::UnpackedContainer),
    LinkPrepared(crate::link::LinkPlan),
    LinkCreated(PathBuf),
    ImportFiles(Vec<crate::veil_ops::AddFileSpec>),
    ImportConflicts(Option<crate::import_pipeline::ConflictBatch>),
    PackPrepared(PathBuf),
    PasswordValid,
}

/// 服务层错误；调用方可以按类型决定如何展示或处理。
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("{0}")]
    InvalidInput(&'static str),
    #[error("容器状态不可用")]
    StateUnavailable,
    #[error("相同来源路径和目标目录的导入任务正在进行，请勿重复提交")]
    DuplicateImport,
    #[error("请先打开容器")]
    NotOpen,
    #[error("{operation}：{source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Core(#[from] veil_core::error::VeilError),
    #[error("{0}")]
    Workflow(#[from] crate::workflow::WorkflowError),
    #[error("{0}")]
    Import(#[from] crate::import::ImportError),
    #[error(
        "导入失败：{name}；已完成 {completed} 个文件（其中跳过 {skipped} 个）：{source}。处理问题后可重新选择相同文件继续导入"
    )]
    ImportFileFailed {
        name: String,
        completed: u64,
        skipped: u64,
        #[source]
        source: veil_core::error::VeilError,
    },
    #[error("导入已中止，已完成 {completed} 个文件（其中跳过 {skipped} 个）")]
    ImportCancelled { completed: u64, skipped: u64 },
    #[error("容器创建未完成：{source}。目录保留在 {path}")]
    CreateFailed {
        path: PathBuf,
        #[source]
        source: veil_core::error::VeilError,
    },
    #[error("服务响应类型不符合请求")]
    UnexpectedResponse,
}

impl ServiceError {
    fn log_level(&self, operation: &str) -> Option<veil_core::logging::Level> {
        use veil_core::logging::Level;
        match self {
            Self::InvalidInput(_)
            | Self::DuplicateImport
            | Self::NotOpen
            | Self::ImportCancelled { .. }
            | Self::Import(
                crate::import::ImportError::Cancelled
                | crate::import::ImportError::InvalidDestination(_)
                | crate::import::ImportError::NotFile(_),
            ) => None,
            Self::Workflow(error) => match error {
                crate::workflow::WorkflowError::Core(error) => core_log_level(error),
                crate::workflow::WorkflowError::Io(_)
                | crate::workflow::WorkflowError::CreateFailed { .. }
                | crate::workflow::WorkflowError::LinkRegistrationFailed { .. } => {
                    Some(Level::Error)
                }
                _ => None,
            },
            Self::Core(veil_core::error::VeilError::DecryptionError(_))
                if operation == "OpenContainer" =>
            {
                Some(Level::Warn)
            }
            Self::Core(error) | Self::ImportFileFailed { source: error, .. } => {
                core_log_level(error)
            }
            Self::Import(crate::import::ImportError::SourceNotFound(_)) => Some(Level::Warn),
            _ => Some(Level::Error),
        }
    }

    fn log_code(&self) -> &'static str {
        match self {
            Self::InvalidInput(..) => "INVALID_INPUT",
            Self::StateUnavailable => "STATE_UNAVAILABLE",
            Self::DuplicateImport => "DUPLICATE_IMPORT",
            Self::NotOpen => "NOT_OPEN",
            Self::Io { .. } => "IO",
            Self::Core(..) => "CORE",
            Self::Workflow(error) => error.log_code(),
            Self::Import(error) => error.log_code(),
            Self::ImportFileFailed { .. } => "IMPORT_FILE_FAILED",
            Self::ImportCancelled { .. } => "IMPORT_CANCELLED",
            Self::CreateFailed { .. } => "CREATE_FAILED",
            Self::UnexpectedResponse => "UNEXPECTED_RESPONSE",
        }
    }
}

fn core_log_level(error: &veil_core::error::VeilError) -> Option<veil_core::logging::Level> {
    use veil_core::{error::VeilError, logging::Level};
    match error {
        VeilError::Cancelled => None,
        VeilError::ImportBatchFailed { source, .. } => core_log_level(source),
        VeilError::FileAlreadyExists(_)
        | VeilError::VersionConflict(_)
        | VeilError::LockConflict { .. }
        | VeilError::FileNotFound(_)
        | VeilError::VeilNotFound(_)
        | VeilError::VeilNameAmbiguous(_)
        | VeilError::VolumeUnavailable(_) => Some(Level::Warn),
        _ => Some(Level::Error),
    }
}

#[derive(Clone)]
struct Session {
    path: PathBuf,
    veil_id: String,
    password: Zeroizing<String>,
}

const GARBAGE_QUEUE_CAPACITY: usize = 64;
const GARBAGE_BATCH_COUNT: usize = 16;
const GARBAGE_BATCH_INTERVAL: Duration = Duration::from_secs(30);

/// 只保存清理信号；密码仅由消费者从当前已解锁会话中短暂读取。
struct GarbageCollectionQueue {
    sender: veil_core::queue::Sender<()>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl GarbageCollectionQueue {
    fn new(session: Arc<Mutex<Option<Session>>>) -> Self {
        let (sender, receiver) = veil_core::queue::BoundedQueue::new(GARBAGE_QUEUE_CAPACITY).split();
        let recurring = sender.clone();
        let worker = std::thread::spawn(move || {
            let _ = receiver.listen_batches(
                veil_core::queue::BatchPolicy {
                    count: GARBAGE_BATCH_COUNT,
                    interval: GARBAGE_BATCH_INTERVAL,
                },
                |_| 1,
                |state| {
                    Ok::<_, ()>(veil_core::queue::ListenControl::new(
                        GARBAGE_BATCH_COUNT,
                        state.closed,
                    ))
                },
                |_| {
                    let session = session.lock().ok().and_then(|session| session.clone());
                    if let Some(session) = session {
                        let manager = VeilManager::for_veil(session.path, session.veil_id);
                        let _ = manager.cleanup(session.password.as_str());
                        // 会话保持解锁时，周期性发送下一轮低优先级扫描。
                        let _ = recurring.try_send(());
                    }
                    Ok(())
                },
            );
        });
        Self {
            sender,
            worker: Mutex::new(Some(worker)),
        }
    }

    fn schedule(&self) {
        let _ = self.sender.try_send(());
    }
}

impl Drop for GarbageCollectionQueue {
    fn drop(&mut self) {
        self.sender.close();
        if let Ok(worker) = self.worker.get_mut() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
}

fn import_one_file(
    manager: &VeilManager,
    file: &crate::veil_ops::AddFileSpec,
    password: &str,
    directory_guard: Option<(
        &crate::storage::locks::LeaseGuard,
        &crate::veil_ops::LockedDirectory,
    )>,
    transaction_gate: &Mutex<()>,
    batch: &crate::veil_ops::ImportBatchContext,
) -> veil_core::error::Result<bool> {
    let result = manager.add_files_for_import(
        std::slice::from_ref(file),
        password,
        directory_guard,
        transaction_gate,
        batch,
    );
    match result {
        Ok(_) => Ok(false),
        Err(veil_core::error::VeilError::FileAlreadyExists(_)) => {
            veil_core::progress::verifying();
            use crate::comparison::{
                ComparisonChecks, ConflictOutcome, FileInput, check_file_conflict,
                check_path_conflict,
            };
            let result = check_file_conflict(
                manager,
                &file.target,
                password,
                FileInput::External {
                    path: &file.source,
                    version: None,
                },
                ComparisonChecks {
                    hash: true,
                    ..Default::default()
                },
                directory_guard,
            );
            match result {
                Ok(ConflictOutcome::NoConflict) => {
                    if check_path_conflict(manager, &file.target, password, directory_guard)? {
                        Ok(true)
                    } else {
                        // 检测期间目标被移除时重新导入，不能误记为已完成。
                        manager.add_files_for_import(
                            std::slice::from_ref(file),
                            password,
                            directory_guard,
                            transaction_gate,
                            batch,
                        )?;
                        Ok(false)
                    }
                }
                Ok(_) | Err(veil_core::error::VeilError::FileNotFound(_)) => Err(
                    veil_core::error::VeilError::FileAlreadyExists(file.target.clone()),
                ),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerView {
    pub name: String,
    pub id: String,
    pub path: String,
    pub created_at: String,
    pub files: Vec<FileView>,
    pub directories: Vec<veil_core::metadata::DirectoryEntry>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileView {
    pub id: String,
    pub name: String,
    pub size: u64,
    pub version: usize,
    pub encrypted_at: String,
    pub category: &'static str,
    pub preview_mime: Option<&'static str>,
}

impl VeilService {
    fn register_import(
        &self,
        sources: &[String],
        directory: &str,
    ) -> Result<ImportGuard, ServiceError> {
        let destination = if directory.is_empty() {
            String::new()
        } else {
            crate::veil_ops::normalize_veil_dir(directory)?
        };
        let container = self
            .3
            .read()
            .map_err(|_| ServiceError::StateUnavailable)?
            .clone()
            .ok_or(ServiceError::NotOpen)?;
        let mut source_paths = Vec::with_capacity(sources.len());
        for source in sources {
            source_paths.push(
                std::path::absolute(source).map_err(|source| ServiceError::Io {
                    operation: "无法解析导入来源路径",
                    source,
                })?,
            );
        }
        source_paths.sort();
        source_paths.dedup();
        let key = ImportKey {
            container,
            source_paths,
            destination,
        };
        let registry = import_registry();
        let mut active = registry
            .lock()
            .map_err(|_| ServiceError::StateUnavailable)?;
        if !active.insert(key.clone()) {
            return Err(ServiceError::DuplicateImport);
        }
        drop(active);
        Ok(ImportGuard {
            active: registry,
            key,
        })
    }

    /// 调用方需持有返回的句柄，供其他进程查询本进程的线程或任务。
    pub fn listen_execution(&self) -> std::io::Result<crate::execution::ExecutionServer> {
        crate::execution::ExecutionService::global()?.listen()
    }

    /// 接收容器会话请求，执行对应的参数校验和业务流程。
    pub fn execute(&self, request: ServiceRequest<'_>) -> Result<ServiceResponse, ServiceError> {
        let _request = veil_core::logging::request_scope();
        let _secrets = veil_core::logging::sensitive_scope(&[]);
        match &request {
            ServiceRequest::CreateContainer { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            ServiceRequest::OpenContainer { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            ServiceRequest::MoveContainer { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            ServiceRequest::CreateInSpace { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            ServiceRequest::ImportPackageInSpace { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            ServiceRequest::CommitImport { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            ServiceRequest::CommitContainerPack { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            ServiceRequest::RegisterContainer { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            ServiceRequest::ValidateNewPassword { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            ServiceRequest::CommitCreate { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            ServiceRequest::CommitUnpack { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            _ => {}
        }
        let started = std::time::Instant::now();
        let is_transfer = matches!(
            &request,
            ServiceRequest::ImportSessionFiles { .. }
                | ServiceRequest::StreamSessionImport { .. }
                | ServiceRequest::StreamSessionExport { .. }
                | ServiceRequest::ExportSessionFile { .. }
                | ServiceRequest::ExportSessionPackage { .. }
                | ServiceRequest::ImportPackageInSpace { .. }
        );
        let operation = request.log_operation();
        let result = self.execute_inner(request);
        if let Err(error) = &result {
            if let Some(level) = error.log_level(operation) {
                use veil_core::logging::{self, Field};
                let mut context = vec![
                    ("elapsed_ms", Field::Duration(started.elapsed())),
                    ("outcome", Field::Label("failed")),
                ];
                if is_transfer {
                    let progress = self.transfer_progress();
                    context.extend([
                        ("phase", Field::Label(progress.phase)),
                        ("completed_files", Field::Count(progress.completed_files)),
                        ("total_files", Field::Count(progress.total_files)),
                        ("parallel_workers", Field::Count(progress.parallel_workers)),
                    ]);
                }
                logging::failure(
                    level,
                    "service",
                    operation,
                    error.log_code(),
                    error,
                    &context,
                );
            }
        }
        result
    }

    fn execute_inner(&self, request: ServiceRequest<'_>) -> Result<ServiceResponse, ServiceError> {
        let _import = match &request {
            ServiceRequest::ImportSessionFiles { sources, directory }
            | ServiceRequest::StreamSessionImport { sources, directory } => {
                Some(self.register_import(sources, directory)?)
            }
            _ => None,
        };
        // 文件传输任务串行启动进度，避免后台导出请求重置正在运行的传输进度。
        let _import_serial = if _import.is_some()
            || matches!(
                &request,
                ServiceRequest::StreamSessionExport { .. }
                    | ServiceRequest::ExportSessionFile { .. }
                    | ServiceRequest::ExportSessionPackage { .. }
            ) {
            Some(self.2.lock().map_err(|_| ServiceError::StateUnavailable)?)
        } else {
            None
        };
        let operation = match &request {
            ServiceRequest::ImportSessionFiles { .. }
            | ServiceRequest::StreamSessionImport { .. } => Some("import"),
            ServiceRequest::ExportSessionFile { .. }
            | ServiceRequest::StreamSessionExport { .. } => Some("export"),
            ServiceRequest::ImportPackageInSpace { .. } => Some("import_package"),
            ServiceRequest::ExportSessionPackage { .. } => Some("export_package"),
            _ => None,
        };
        let _progress = operation.map(|operation| self.1.begin(operation));
        let execution =
            crate::execution::ExecutionService::global().map_err(|source| ServiceError::Io {
                operation: "无法初始化执行上下文",
                source,
            })?;
        let _scope = execution.enter_task("VeilService::execute");
        let result = self.execute_registered(
            request,
            _import.as_ref().map(|guard| guard.key.container.as_path()),
        );
        if operation.is_some() {
            if matches!(&result, Err(ServiceError::ImportCancelled { .. }))
                || (matches!(operation, Some("import" | "export"))
                    && matches!(
                        &result,
                        Err(ServiceError::Core(veil_core::error::VeilError::Cancelled))
                    ))
            {
                self.1.finish_cancelled();
            } else {
                self.1.finish(result.is_ok());
            }
        }
        result
    }

    pub fn transfer_progress(&self) -> veil_core::progress::Snapshot {
        self.1.snapshot()
    }

    pub fn cancel_transfer(&self) -> bool {
        self.1.request_cancel()
    }

    fn execute_registered(
        &self,
        request: ServiceRequest<'_>,
        import_container: Option<&Path>,
    ) -> Result<ServiceResponse, ServiceError> {
        match request {
            ServiceRequest::GetUserSetting { key } => Ok(ServiceResponse::Setting(
                crate::storage::app::AppStore::global()?.user_setting(key)?,
            )),
            ServiceRequest::SetUserSetting { key, value } => {
                crate::storage::app::AppStore::global()?.set_user_setting(key, value)?;
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::RenameSpace { id, name } => {
                crate::storage::app::AppStore::global()?.rename_space(id, name)?;
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::SetDefaultSpace { id } => {
                crate::storage::app::AppStore::global()?.set_default_space(id)?;
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::DeleteSpace { id } => {
                self.lock_if_space_open(&id)?;
                crate::spaces::delete_space(&id)?;
                Ok(ServiceResponse::Spaces(crate::spaces::list_spaces()?))
            }
            ServiceRequest::DeleteSpaceRecord { id } => {
                self.lock_if_space_open(&id)?;
                crate::spaces::delete_space_record(&id)?;
                Ok(ServiceResponse::Spaces(crate::spaces::list_spaces()?))
            }
            ServiceRequest::DeleteContainer { id } => {
                self.lock_if_open(&id)?;
                crate::spaces::delete_container(&id)?;
                Ok(ServiceResponse::Spaces(crate::spaces::list_spaces()?))
            }
            ServiceRequest::UnregisterContainer { id } => {
                crate::storage::app::AppStore::global()?.unregister(id)?;
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::MoveContainer {
                id,
                target_space,
                password,
            } => {
                let store = crate::storage::app::AppStore::global()?;
                let mut target_space_lock = Some(store.acquire_space_shared_lock(target_space)?);
                crate::spaces::move_container(id, target_space, password)?;
                let destination = store
                    .spaces()?
                    .into_iter()
                    .find(|s| s.id == target_space)
                    .and_then(|s| s.local_path)
                    .ok_or(ServiceError::StateUnavailable)?
                    .join(id);
                if let Some(session) = self
                    .0
                    .lock()
                    .map_err(|_| ServiceError::StateUnavailable)?
                    .as_mut()
                    .filter(|s| s.veil_id == id)
                {
                    session.path = destination.clone();
                    session.password = Zeroizing::new(password.into());
                    *self.6.lock().map_err(|_| ServiceError::StateUnavailable)? =
                        target_space_lock.take();
                    *self.3.write().map_err(|_| ServiceError::StateUnavailable)? =
                        Some(destination);
                }
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::ListRecents => Ok(ServiceResponse::Recents(
                crate::storage::app::AppStore::global()?.records("recent_div", 6)?,
            )),
            ServiceRequest::CheckExecution {
                owner,
                endpoint,
                timeout,
            } => {
                let execution = crate::execution::ExecutionService::global().map_err(|source| {
                    ServiceError::Io {
                        operation: "无法初始化执行上下文",
                        source,
                    }
                })?;
                Ok(ServiceResponse::ExecutionStatus(
                    execution.check_owner(owner, endpoint, timeout),
                ))
            }
            ServiceRequest::CreateContainer {
                parent,
                name,
                password,
            } => self
                .create(parent, name, password)
                .map(ServiceResponse::Container),
            ServiceRequest::OpenContainer { path, password } => {
                self.open(path, password).map(ServiceResponse::Container)
            }
            ServiceRequest::RefreshContainer => self.refresh().map(ServiceResponse::Container),
            ServiceRequest::LockContainer => {
                self.lock()?;
                Ok(ServiceResponse::Locked)
            }
            ServiceRequest::ListSpaces => {
                Ok(ServiceResponse::Spaces(crate::spaces::list_spaces()?))
            }
            ServiceRequest::CreateSpace { name, path } => {
                crate::spaces::create_space(&name, path.as_deref())?;
                Ok(ServiceResponse::Spaces(crate::spaces::list_spaces()?))
            }
            ServiceRequest::CreateInSpace {
                space,
                name,
                password,
            } => {
                let _space_lock = Self::space_lock_for_selector(&space)?;
                let password = Zeroizing::new(password);
                let created = crate::spaces::create_container(&space, &name, password.as_str())?;
                self.open(
                    created.directory.to_string_lossy().into_owned(),
                    password.to_string(),
                )
                .map(ServiceResponse::Container)
            }
            ServiceRequest::ImportSessionFiles { sources, directory } => self
                .import_sources(
                    sources,
                    directory,
                    import_container.ok_or(ServiceError::NotOpen)?,
                )
                .map(ServiceResponse::Container),
            ServiceRequest::StreamSessionImport { sources, directory } => {
                let expected = import_container.ok_or(ServiceError::NotOpen)?;
                let session = self
                    .0
                    .lock()
                    .map_err(|_| ServiceError::StateUnavailable)?
                    .clone()
                    .ok_or(ServiceError::NotOpen)?;
                if session.path != expected {
                    return Err(ServiceError::InvalidInput("容器已切换，请重新导入"));
                }
                let control = Arc::new(crate::import_pipeline::ImportConflictQueue::default());
                *self.4.lock().map_err(|_| ServiceError::StateUnavailable)? =
                    Some(Arc::clone(&control));
                let _registration = StreamingImportRegistration(&self.4);
                let mut options = crate::storage::locks::default_options();
                options.cancellation = Some(self.1.cancellation_flag());
                let manager =
                    VeilManager::for_veil(session.path, session.veil_id).with_lock_options(options);
                let result = crate::import_pipeline::run(
                    &manager,
                    session.password.as_str(),
                    sources,
                    directory,
                    &self.1,
                    &control,
                );
                result.map(ServiceResponse::Container)
            }
            ServiceRequest::PollSessionImportConflicts => {
                let control = self
                    .4
                    .lock()
                    .map_err(|_| ServiceError::StateUnavailable)?
                    .clone();
                Ok(ServiceResponse::ImportConflicts(control.and_then(
                    |control| control.poll(self.1.snapshot().total_files),
                )))
            }
            ServiceRequest::ResolveSessionImportConflicts { id, resolutions } => {
                let control = self
                    .4
                    .lock()
                    .map_err(|_| ServiceError::StateUnavailable)?
                    .clone()
                    .ok_or(ServiceError::InvalidInput("导入任务已结束"))?;
                control.resolve(&id, resolutions)?;
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::PreviewSessionFile { name } => {
                self.preview_file(&name).map(|(bytes, unstable)| {
                    if unstable {
                        ServiceResponse::PreviewUnstable(bytes)
                    } else {
                        ServiceResponse::Preview(bytes)
                    }
                })
            }
            ServiceRequest::ExportSessionFile { name, destination } => {
                let unstable = self.export_file(&name, &destination)?;
                Ok(if unstable {
                    ServiceResponse::ExportUnstable
                } else {
                    ServiceResponse::Done
                })
            }
            ServiceRequest::StreamSessionExport { names, destination } => {
                let session = self
                    .0
                    .lock()
                    .map_err(|_| ServiceError::StateUnavailable)?
                    .clone()
                    .ok_or(ServiceError::NotOpen)?;
                let control = Arc::new(crate::transfer_conflicts::ConflictQueue::default());
                *self.5.lock().map_err(|_| ServiceError::StateUnavailable)? =
                    Some(Arc::clone(&control));
                struct Registration<'a>(
                    &'a Mutex<
                        Option<
                            Arc<
                                crate::transfer_conflicts::ConflictQueue<
                                    crate::export_pipeline::Work,
                                >,
                            >,
                        >,
                    >,
                );
                impl Drop for Registration<'_> {
                    fn drop(&mut self) {
                        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    }
                }
                let _registration = Registration(&self.5);
                let mut options = crate::storage::locks::default_options();
                options.cancellation = Some(self.1.cancellation_flag());
                let manager =
                    VeilManager::for_veil(session.path, session.veil_id).with_lock_options(options);
                crate::export_pipeline::run(
                    &manager,
                    &session.password,
                    names,
                    Path::new(&destination),
                    &self.1,
                    &control,
                    true,
                )?;
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::PollSessionExportConflicts => {
                let control = self
                    .5
                    .lock()
                    .map_err(|_| ServiceError::StateUnavailable)?
                    .clone();
                Ok(ServiceResponse::ImportConflicts(
                    control.and_then(|c| c.poll(self.1.snapshot().total_files)),
                ))
            }
            ServiceRequest::ResolveSessionExportConflicts { id, resolutions } => {
                let control = self
                    .5
                    .lock()
                    .map_err(|_| ServiceError::StateUnavailable)?
                    .clone()
                    .ok_or(ServiceError::InvalidInput("导出任务已结束"))?;
                control.resolve(&id, resolutions)?;
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::ExportSessionPackage { destination } => {
                let session = self.0.lock().map_err(|_| ServiceError::StateUnavailable)?;
                let session = session.as_ref().ok_or(ServiceError::NotOpen)?;
                let manager = VeilManager::for_veil(session.path.clone(), session.veil_id.clone());
                crate::package::VeilPacker::new(destination)
                    .without_overwrite()
                    .pack_container(&manager, session.password.as_str())?;
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::ImportPackageInSpace {
                path,
                space,
                password,
            } => {
                let _space_lock = Self::space_lock_for_selector(&space)?;
                crate::spaces::ensure_space_available(&space)?;
                let password = Zeroizing::new(password);
                let link = crate::config::AppConfig::config_path()?
                    .parent()
                    .ok_or(ServiceError::InvalidInput("应用目录无效"))?
                    .join("links")
                    .join(format!("{}.veil-link", crate::storage::id()));
                let plan = crate::workflow::prepare_unpack(crate::workflow::UnpackOptions {
                    package_path: &path,
                    name: None,
                    work_name: Some(&space),
                    link_output: link.to_str(),
                })?;
                let unpacked = crate::workflow::commit_unpack(plan, password.as_str())?;
                self.open(
                    unpacked.directory.to_string_lossy().into_owned(),
                    password.to_string(),
                )
                .map(ServiceResponse::Container)
            }
            ServiceRequest::RemoveSessionPath { name } => {
                self.remove_path(&name).map(ServiceResponse::Container)
            }
            ServiceRequest::OperateContainer {
                directory,
                veil_id,
                operation,
            } => {
                let _space_lock = Self::space_lock_for_container(veil_id)?;
                let manager = VeilManager::for_veil(directory.to_path_buf(), veil_id);
                Ok(ServiceResponse::Operation(crate::operations::execute(
                    &manager, operation,
                )?))
            }
            ServiceRequest::PrepareContainer { selector } => Ok(
                ServiceResponse::ContainerPrepared(crate::commands::prepare_container(selector)?),
            ),
            ServiceRequest::OperatePrepared {
                container,
                operation,
            } => {
                let _space_lock = Self::space_lock_for_container(container.id())?;
                Ok(ServiceResponse::Operation(crate::commands::operate(
                    container, operation,
                )?))
            }
            ServiceRequest::PrepareImport {
                selector,
                source,
                destination,
            } => Ok(ServiceResponse::ImportPrepared(
                crate::commands::prepare_import(selector, source, destination)?,
            )),
            ServiceRequest::CommitImport { plan, password } => {
                let _space_lock = Self::space_lock_for_container(plan.container_id())?;
                Ok(ServiceResponse::Imported(crate::commands::commit_import(
                    plan, password,
                )?))
            }
            ServiceRequest::PrepareContainerPack { selector, output } => {
                Ok(ServiceResponse::ContainerPackPrepared(
                    crate::commands::prepare_pack(selector, output)?,
                ))
            }
            ServiceRequest::CommitContainerPack { plan, password } => {
                let _space_lock = Self::space_lock_for_container(plan.container_id())?;
                Ok(ServiceResponse::ContainerPacked(
                    crate::commands::commit_pack(plan, password)?,
                ))
            }
            ServiceRequest::ResolveContainer { selector } => {
                let mut config = crate::config::AppConfig::load()?;
                Ok(ServiceResponse::Resolved(config.resolve_veil(selector)?))
            }
            ServiceRequest::ReadConfigSummary => {
                Ok(ServiceResponse::ConfigSummary(crate::config::summary()?))
            }
            ServiceRequest::RegisterContainer {
                directory,
                password,
            } => {
                let metadata = VeilManager::new(directory.into()).read_meta(password)?;
                crate::config::AppConfig::load()?.register_directory(
                    directory,
                    &metadata.veil_id,
                    &metadata.veil_name,
                    &metadata.created_at,
                )?;
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::ConfigPath => Ok(ServiceResponse::ConfigPath(
                crate::config::AppConfig::config_path()?,
            )),
            ServiceRequest::GuessMime { path } => {
                Ok(ServiceResponse::Mime(veil_core::mime::guess_mime(path)))
            }
            ServiceRequest::EffectiveHintLevel { override_value } => Ok(
                ServiceResponse::EffectiveHintLevel(crate::hints::effective_level(override_value)),
            ),
            ServiceRequest::ShouldShowHint {
                kind,
                override_value,
            } => Ok(ServiceResponse::ShowHint(crate::hints::should_show(
                kind,
                override_value,
            ))),
            ServiceRequest::SetHintLevel { level } => {
                let level = crate::config::HintLevel::parse(level)
                    .ok_or(crate::workflow::WorkflowError::InvalidHintLevel)?;
                crate::storage::app::AppStore::global()?
                    .set_user_setting("hint_level", &serde_json::json!(level.as_str()))?;
                Ok(ServiceResponse::HintLevelSet(level))
            }
            ServiceRequest::PrepareLink { target, output } => Ok(ServiceResponse::LinkPrepared(
                crate::link::prepare_link(target, output)?,
            )),
            ServiceRequest::CommitLink { plan } => Ok(ServiceResponse::LinkCreated(
                crate::link::commit_link(plan)?,
            )),
            ServiceRequest::CollectImport {
                source,
                destination,
            } => Ok(ServiceResponse::ImportFiles(
                crate::import::collect_add_specs(source, destination)?,
            )),
            ServiceRequest::PreparePack { name, output } => {
                let path = output
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(format!("{name}.vault.veil")));
                if path.exists() {
                    return Err(crate::workflow::WorkflowError::PackOutputExists(path).into());
                }
                Ok(ServiceResponse::PackPrepared(path))
            }
            ServiceRequest::ValidateNewPassword { password } => {
                if password.is_empty() {
                    return Err(crate::workflow::WorkflowError::PasswordEmpty.into());
                }
                Ok(ServiceResponse::PasswordValid)
            }
            ServiceRequest::PrepareCreate { options } => Ok(ServiceResponse::CreatePrepared(
                crate::workflow::prepare_create(options)?,
            )),
            ServiceRequest::CommitCreate { plan, password } => Ok(ServiceResponse::Created(
                crate::workflow::commit_create(plan, password)?,
            )),
            ServiceRequest::PrepareUnpack { options } => Ok(ServiceResponse::UnpackPrepared(
                crate::workflow::prepare_unpack(options)?,
            )),
            ServiceRequest::CommitUnpack { plan, password } => Ok(ServiceResponse::Unpacked(
                crate::workflow::commit_unpack(plan, password)?,
            )),
        }
    }

    fn create(
        &self,
        parent: String,
        name: String,
        password: String,
    ) -> Result<ContainerView, ServiceError> {
        let _session_change = self.2.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let password = Zeroizing::new(password);
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 100 {
            return Err(ServiceError::InvalidInput(
                "请输入 1 至 100 个字符的容器名称",
            ));
        }
        if password.is_empty() {
            return Err(ServiceError::InvalidInput("密码不能为空"));
        }
        let parent = PathBuf::from(parent);
        if !parent.is_dir() {
            return Err(ServiceError::InvalidInput("请选择存在的存储位置"));
        }
        let parent = std::fs::canonicalize(parent).map_err(|source| ServiceError::Io {
            operation: "无法访问保存位置",
            source,
        })?;
        let id = generate_veil_id();
        let path = parent.join(&id);
        let mut session = self.0.lock().map_err(|_| ServiceError::StateUnavailable)?;
        std::fs::create_dir(&path).map_err(|source| ServiceError::Io {
            operation: "无法创建容器目录",
            source,
        })?;
        let manager = VeilManager::for_veil(path.clone(), id.clone());
        let metadata = manager
            .init_veil_with_id(&id, name, password.as_str())
            .map_err(|source| ServiceError::CreateFailed {
                path: path.clone(),
                source,
            })?;
        crate::config::AppConfig::load()?.register_directory(
            &path,
            &metadata.veil_id,
            &metadata.veil_name,
            &metadata.created_at,
        )?;
        let view = container_view(&path, &metadata);
        *self.3.write().map_err(|_| ServiceError::StateUnavailable)? = Some(path.clone());
        if let Some(previous) = session.as_ref() {
            veil_core::database::invalidate(&previous.path.join(veil_core::database::CONTAINER_DB));
        }
        *session = Some(Session {
            path,
            veil_id: id,
            password,
        });
        self.8.schedule();
        Ok(view)
    }

    fn open(&self, path: String, password: String) -> Result<ContainerView, ServiceError> {
        let _session_change = self.2.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let password = Zeroizing::new(password);
        if password.is_empty() {
            return Err(ServiceError::InvalidInput("请输入容器密码"));
        }
        let path = PathBuf::from(path);
        if !path.is_dir() {
            return Err(ServiceError::InvalidInput("请选择有效的容器目录"));
        }
        let path = std::fs::canonicalize(path).map_err(|source| ServiceError::Io {
            operation: "无法访问容器目录",
            source,
        })?;
        let mut session = self.0.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let metadata = VeilManager::new(path.clone()).read_meta(password.as_str())?;
        crate::config::AppConfig::load()?.register_directory(
            &path,
            &metadata.veil_id,
            &metadata.veil_name,
            &metadata.created_at,
        )?;
        let space_lock = Self::space_lock_for_container(&metadata.veil_id)?;
        let metadata = VeilManager::new(path.clone()).read_meta(password.as_str())?;
        let view = container_view(&path, &metadata);
        *self.3.write().map_err(|_| ServiceError::StateUnavailable)? = Some(path.clone());
        if let Some(previous) = session.as_ref() {
            veil_core::database::invalidate(&previous.path.join(veil_core::database::CONTAINER_DB));
        }
        *self.6.lock().map_err(|_| ServiceError::StateUnavailable)? = space_lock;
        *session = Some(Session {
            path,
            veil_id: metadata.veil_id.clone(),
            password,
        });
        self.8.schedule();
        Ok(view)
    }

    fn refresh(&self) -> Result<ContainerView, ServiceError> {
        let session = self.0.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let session = session.as_ref().ok_or(ServiceError::NotOpen)?;
        let manager = VeilManager::for_veil(session.path.clone(), session.veil_id.clone());
        let metadata = manager.read_meta(session.password.as_str())?;
        Ok(container_view(&session.path, &metadata))
    }

    fn import_sources(
        &self,
        sources: Vec<String>,
        directory: String,
        expected_container: &Path,
    ) -> Result<ContainerView, ServiceError> {
        let settings = crate::storage::app::AppStore::global()?;
        let commit_interval =
            std::time::Duration::from_secs(settings.import_commit_interval_seconds()?);
        let commit_count = settings.import_commit_count()?;
        self.import_sources_with_interval(
            sources,
            directory,
            expected_container,
            commit_interval,
            commit_count,
        )
    }

    fn import_sources_with_interval(
        &self,
        sources: Vec<String>,
        directory: String,
        expected_container: &Path,
        commit_interval: std::time::Duration,
        commit_count: usize,
    ) -> Result<ContainerView, ServiceError> {
        if sources.is_empty() {
            return Err(ServiceError::InvalidInput("请选择文件或文件夹"));
        }
        let directory = if directory.is_empty() {
            String::new()
        } else {
            crate::veil_ops::normalize_veil_dir(&directory)?
        };
        let session = self
            .0
            .lock()
            .map_err(|_| ServiceError::StateUnavailable)?
            .clone()
            .ok_or(ServiceError::NotOpen)?;
        if session.path != expected_container {
            return Err(ServiceError::InvalidInput("容器已切换，请重新导入"));
        }
        let manager = VeilManager::for_veil(session.path.clone(), session.veil_id.clone());
        let mut files = Vec::new();
        let mut directory_paths = Vec::new();
        let mut targets = std::collections::HashSet::new();
        for source in sources {
            if veil_core::progress::check_cancelled().is_err() {
                return Err(ServiceError::ImportCancelled {
                    completed: 0,
                    skipped: 0,
                });
            }
            let path = Path::new(&source);
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or(ServiceError::InvalidInput("文件名称无效"))?;
            let is_directory = path.is_dir();
            let destination = if is_directory {
                if directory.is_empty() {
                    name.to_owned()
                } else {
                    format!("{directory}/{name}")
                }
            } else if directory.is_empty() {
                String::new()
            } else {
                format!("{directory}/")
            };
            if is_directory {
                directory_paths.push(destination.clone());
            }
            let collected = crate::import::collect_add_specs(
                path,
                if destination.is_empty() {
                    None
                } else {
                    Some(&destination)
                },
            )
            .map_err(|error| match error {
                crate::import::ImportError::Cancelled => ServiceError::ImportCancelled {
                    completed: 0,
                    skipped: 0,
                },
                other => ServiceError::Import(other),
            })?;
            for spec in collected {
                if !targets.insert(spec.target.clone()) {
                    return Err(ServiceError::InvalidInput("选择的文件包含相同目标路径"));
                }
                files.push(spec.without_overwrite());
            }
        }
        if files.is_empty() {
            return Err(ServiceError::InvalidInput("选择的文件夹没有可添加的文件"));
        }
        let mut total = Some(0u64);
        for file in &files {
            if veil_core::progress::check_cancelled().is_err() {
                return Err(ServiceError::ImportCancelled {
                    completed: 0,
                    skipped: 0,
                });
            }
            total = total.and_then(|total| total.checked_add(file.source.metadata().ok()?.len()));
        }
        for target in &targets {
            if veil_core::progress::check_cancelled().is_err() {
                return Err(ServiceError::ImportCancelled {
                    completed: 0,
                    skipped: 0,
                });
            }
            for (index, _) in target.match_indices('/') {
                if targets.contains(&target[..index]) {
                    return Err(ServiceError::InvalidInput("选择的文件包含冲突的目标路径"));
                }
            }
        }
        directory_paths.sort();
        directory_paths.dedup();
        let (directory_guard, locked_directories) = if directory_paths.is_empty() {
            (None, Vec::new())
        } else {
            match manager.lock_import_directories(
                &directory_paths,
                session.password.as_str(),
                self.1.cancellation_flag(),
            ) {
                Ok((guard, directories)) => (Some(Arc::new(guard)), directories),
                Err(_) if veil_core::progress::check_cancelled().is_err() => {
                    return Err(ServiceError::ImportCancelled {
                        completed: 0,
                        skipped: 0,
                    });
                }
                Err(error) => return Err(error.into()),
            }
        };
        veil_core::progress::start(total);
        veil_core::progress::set_file_count(files.len());
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let worker_count = veil_core::progress::worker_count(files.len());
        veil_core::progress::set_parallel_workers(worker_count);
        let next = AtomicUsize::new(0);
        let stopped = AtomicBool::new(false);
        let failure = Mutex::new(None);
        let transaction_gate = Mutex::new(());
        let owner = crate::execution::ExecutionService::global()
            .map_err(|source| ServiceError::Io {
                operation: "无法初始化导入执行上下文",
                source,
            })?
            .runtime()
            .current_context()
            .owner()
            .clone();
        struct WorkerEnd<'a> {
            remaining: Arc<AtomicUsize>,
            stopped: &'a AtomicBool,
        }
        impl Drop for WorkerEnd<'_> {
            fn drop(&mut self) {
                if std::thread::panicking() {
                    self.stopped.store(true, Ordering::Release);
                }
                self.remaining.fetch_sub(1, Ordering::Release);
            }
        }
        let logging_context = veil_core::logging::capture_context();
        std::thread::scope(|scope| {
            let logging_context = &logging_context;
            // 队列及待提交批次均限制数量，避免长间隔导入时无限累积租约。
            const QUEUE_LIMIT: usize = 8;
            let (sender, receiver) = veil_core::queue::BoundedQueue::new(QUEUE_LIMIT).split();
            let remaining_workers = Arc::new(AtomicUsize::new(worker_count));
            let directories = Arc::new(crate::directory_cache::DirectoryCache::default());
            for _ in 0..worker_count {
                let progress = self.1.clone();
                let manager = &manager;
                let files = &files;
                let next = &next;
                let stopped = &stopped;
                let failure = &failure;
                let transaction_gate = &transaction_gate;
                let password = session.password.as_str();
                let directory_guard = directory_guard.as_ref();
                let locked_directories = &locked_directories;
                let remaining_workers = Arc::clone(&remaining_workers);
                let context = crate::veil_ops::ImportBatchContext {
                    directories: directories.clone(),
                    sender: sender.clone(),
                    directory_guard: directory_guard.cloned(),
                    owner: owner.clone(),
                };
                scope.spawn(move || {
                    let _logging = logging_context.attach();
                    let _end = WorkerEnd {
                        remaining: remaining_workers,
                        stopped,
                    };
                    let _attachment = progress.attach();
                    loop {
                        if stopped.load(Ordering::Acquire)
                            || veil_core::progress::check_cancelled().is_err()
                        {
                            break;
                        }
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(file) = files.get(index) else { break };
                        veil_core::progress::begin_file(&file.target);
                        let directory = locked_directories
                            .iter()
                            .filter(|directory| directory.covers(&file.target))
                            .max_by_key(|directory| directory.path_len());
                        let scoped_guard = directory_guard.map(Arc::as_ref).zip(directory);
                        match import_one_file(
                            manager,
                            file,
                            password,
                            scoped_guard,
                            transaction_gate,
                            &context,
                        ) {
                            Ok(true) => veil_core::progress::file_done(true),
                            Ok(false) => {}
                            Err(veil_core::error::VeilError::Cancelled) => {
                                stopped.store(true, Ordering::Release);
                                break;
                            }
                            Err(error) => {
                                stopped.store(true, Ordering::Release);
                                let mut failure = failure.lock().unwrap_or_else(|e| e.into_inner());
                                if failure.is_none() {
                                    *failure = Some((file.target.clone(), error));
                                }
                                break;
                            }
                        }
                    }
                });
            }
            drop(sender);
            let mut pending = Vec::new();
            let mut deadline = None;
            loop {
                let remaining = deadline
                    .map(|at: std::time::Instant| {
                        at.saturating_duration_since(std::time::Instant::now())
                    })
                    .unwrap_or(std::time::Duration::from_millis(20));
                let wait = remaining.min(std::time::Duration::from_millis(20));
                let mut disconnected = false;
                if pending.len() < commit_count {
                    match receiver.recv_batch_timeout(commit_count - pending.len(), wait) {
                        Ok(items) => {
                            if pending.is_empty() {
                                deadline = Some(std::time::Instant::now() + commit_interval);
                            }
                            pending.extend(items);
                        }
                        Err(veil_core::queue::RecvError::Closed) => disconnected = true,
                        Err(
                            veil_core::queue::RecvError::Timeout
                            | veil_core::queue::RecvError::Empty,
                        ) => {}
                        Err(veil_core::queue::RecvError::Cancelled) => disconnected = true,
                    }
                } else if !wait.is_zero() {
                    std::thread::sleep(wait);
                }
                if !pending.is_empty() {
                    if remaining_workers.load(Ordering::Acquire) == 0 {
                        if let Ok(items) = receiver.try_recv_batch(commit_count - pending.len()) {
                            pending.extend(items);
                        }
                    }
                    let stop = stopped.load(Ordering::Acquire)
                        || veil_core::progress::check_cancelled().is_err();
                    if stop
                        || disconnected
                        || pending.len() >= commit_count
                        || remaining_workers.load(Ordering::Acquire) == 0
                        || deadline.is_some_and(|at| std::time::Instant::now() >= at)
                    {
                        let batch = std::mem::take(&mut pending);
                        deadline = None;
                        if stop {
                            VeilManager::cancel_import_batch(batch);
                        } else {
                            let first_target = batch[0].target().to_owned();
                            let _gate = transaction_gate
                                .lock()
                                .unwrap_or_else(|error| error.into_inner());
                            if let Err(error) = manager.commit_import_batch(
                                batch,
                                session.password.as_str(),
                                &stopped,
                            ) {
                                stopped.store(true, Ordering::Release);
                                if !matches!(error, veil_core::error::VeilError::Cancelled) {
                                    let name = match &error {
                                        veil_core::error::VeilError::ImportBatchFailed {
                                            file,
                                            ..
                                        } => file.clone(),
                                        _ => first_target,
                                    };
                                    let mut failure =
                                        failure.lock().unwrap_or_else(|error| error.into_inner());
                                    if failure.is_none() {
                                        *failure = Some((name, error));
                                    }
                                }
                            }
                        }
                    }
                }
                if disconnected {
                    break;
                }
            }
        });
        if let Some(guard) = directory_guard.as_ref() {
            if let Ok(credentials) = guard.credentials() {
                for credential in credentials {
                    let _ = std::fs::remove_dir(
                        manager.veil_dir.join(".staging").join(credential.lock_id),
                    );
                }
            }
        }
        let progress = self.1.snapshot();
        let completed = progress.completed_files;
        let skipped = progress.skipped_files;
        if let Some((name, source)) = failure.into_inner().unwrap_or_else(|e| e.into_inner()) {
            drop(directory_guard);
            return Err(ServiceError::ImportFileFailed {
                name,
                completed,
                skipped,
                source,
            });
        }
        if progress.cancel_requested && completed < files.len() as u64 {
            return Err(ServiceError::ImportCancelled { completed, skipped });
        }
        veil_core::progress::finalizing();
        Ok(container_view(
            &session.path,
            &manager.read_meta(session.password.as_str())?,
        ))
    }

    fn preview_file(&self, name: &str) -> Result<(Vec<u8>, bool), ServiceError> {
        if file_kind(name).1.is_none() {
            return Err(ServiceError::InvalidInput("暂不支持预览此文件类型"));
        }
        let session = self.0.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let session = session.as_ref().ok_or(ServiceError::NotOpen)?;
        let manager = VeilManager::for_veil(session.path.clone(), session.veil_id.clone());
        Ok(manager.read_file_bytes_with_status(
            name,
            session.password.as_str(),
            16 * 1024 * 1024,
        )?)
    }

    fn export_file(&self, name: &str, destination: &str) -> Result<bool, ServiceError> {
        if destination.is_empty() {
            return Err(ServiceError::InvalidInput("请选择导出位置"));
        }
        let session = self.0.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let session = session.as_ref().ok_or(ServiceError::NotOpen)?;
        let manager = VeilManager::for_veil(session.path.clone(), session.veil_id.clone());
        Ok(manager.extract_file_without_overwrite_with_status(
            name,
            Path::new(destination),
            session.password.as_str(),
        )?)
    }

    fn remove_path(&self, name: &str) -> Result<ContainerView, ServiceError> {
        let session = self.0.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let session = session.as_ref().ok_or(ServiceError::NotOpen)?;
        let app_store = crate::storage::app::AppStore::global()?;
        let _space_lock = app_store
            .registrations()?
            .into_iter()
            .find(|registration| registration.div_id == session.veil_id)
            .map(|registration| app_store.acquire_space_shared_lock(&registration.space_id))
            .transpose()?;
        let manager = VeilManager::for_veil(session.path.clone(), session.veil_id.clone());
        let (_, ciphers) = manager.remove_path_deferred(name, session.password.as_str())?;
        self.7.enqueue(ciphers);
        self.8.schedule();
        Ok(container_view(
            &session.path,
            &manager.read_meta(session.password.as_str())?,
        ))
    }

    fn lock(&self) -> Result<(), ServiceError> {
        let _session_change = self.2.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let mut session = self.0.lock().map_err(|_| ServiceError::StateUnavailable)?;
        *self.3.write().map_err(|_| ServiceError::StateUnavailable)? = None;
        if let Some(current) = session.as_ref() {
            veil_core::database::invalidate(&current.path.join(veil_core::database::CONTAINER_DB));
        }
        *session = None;
        *self.6.lock().map_err(|_| ServiceError::StateUnavailable)? = None;
        Ok(())
    }

    fn space_lock_for_container(
        identity: &str,
    ) -> Result<Option<Vec<std::fs::File>>, ServiceError> {
        let store = crate::storage::app::AppStore::global()?;
        let Some(registration) = store
            .registrations()?
            .into_iter()
            .find(|registration| registration.div_id == identity)
        else {
            return Ok(None);
        };
        Ok(Some(
            store.acquire_space_shared_lock(&registration.space_id)?,
        ))
    }

    fn space_lock_for_selector(selector: &str) -> Result<Vec<std::fs::File>, ServiceError> {
        let store = crate::storage::app::AppStore::global()?;
        let space_id = store
            .spaces()?
            .into_iter()
            .find(|space| space.id == selector || space.name == selector)
            .map(|space| space.id)
            .ok_or(ServiceError::InvalidInput("空间不存在"))?;
        Ok(store.acquire_space_shared_lock(&space_id)?)
    }

    fn lock_if_open(&self, identity: &str) -> Result<(), ServiceError> {
        let _session_change = self.2.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let mut session = self.0.lock().map_err(|_| ServiceError::StateUnavailable)?;
        if session
            .as_ref()
            .is_some_and(|current| current.veil_id == identity)
        {
            if let Some(current) = session.as_ref() {
                veil_core::database::invalidate(
                    &current.path.join(veil_core::database::CONTAINER_DB),
                );
            }
            *session = None;
            *self.3.write().map_err(|_| ServiceError::StateUnavailable)? = None;
            *self.6.lock().map_err(|_| ServiceError::StateUnavailable)? = None;
        }
        Ok(())
    }

    fn lock_if_space_open(&self, space_identity: &str) -> Result<(), ServiceError> {
        let store = crate::storage::app::AppStore::global()?;
        let spaces = store.spaces()?;
        let Some(target_path) = spaces
            .iter()
            .find(|space| space.id == space_identity)
            .and_then(|space| space.local_path.as_deref())
        else {
            return Ok(());
        };
        let active_identity = self
            .0
            .lock()
            .map_err(|_| ServiceError::StateUnavailable)?
            .as_ref()
            .map(|session| session.veil_id.clone());
        if let Some(identity) = active_identity {
            let active_space = store
                .registrations()?
                .into_iter()
                .find(|registration| registration.div_id == identity)
                .map(|registration| registration.space_id);
            if active_space
                .as_deref()
                .and_then(|id| spaces.iter().find(|space| space.id == id))
                .and_then(|space| space.local_path.as_deref())
                .is_some_and(|active_path| {
                    active_path.starts_with(target_path) || target_path.starts_with(active_path)
                })
            {
                self.lock_if_open(&identity)?;
            }
        }
        Ok(())
    }
}

pub(crate) fn container_view(path: &Path, metadata: &MetaData) -> ContainerView {
    let mut files: Vec<_> = metadata
        .files
        .iter()
        .map(|file| FileView {
            id: file.file_id.clone(),
            name: file.original_name.clone(),
            size: file.size,
            version: file.version_history.len(),
            encrypted_at: file.encrypted_at.clone(),
            category: file_kind(&file.original_name).0,
            preview_mime: file_kind(&file.original_name).1,
        })
        .collect();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    ContainerView {
        name: metadata.veil_name.clone(),
        id: metadata.veil_id.clone(),
        path: path.to_string_lossy().into_owned(),
        created_at: metadata.created_at.clone(),
        files,
        directories: metadata.directories.clone(),
    }
}

/// 分类根据扩展名判断；只有列出的类型支持内置预览。
fn file_kind(name: &str) -> (&'static str, Option<&'static str>) {
    let ext = Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => ("photo", Some("image/jpeg")),
        "png" => ("photo", Some("image/png")),
        "gif" => ("photo", Some("image/gif")),
        "webp" => ("photo", Some("image/webp")),
        "heic" | "bmp" | "tif" | "tiff" => ("photo", None),
        "mp4" | "m4v" => ("video", Some("video/mp4")),
        "webm" => ("video", Some("video/webm")),
        "mov" => ("video", Some("video/quicktime")),
        "avi" | "mkv" => ("video", None),
        "pdf" => ("document", Some("application/pdf")),
        "txt" | "md" | "csv" | "log" => ("document", Some("text/plain")),
        "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" => ("document", None),
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" => ("archive", None),
        _ => ("other", None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_stream_export(
        app: &VeilService,
        names: Vec<String>,
        destination: &Path,
    ) -> std::thread::JoinHandle<Result<ServiceResponse, ServiceError>> {
        let app = app.clone();
        let destination = destination.to_string_lossy().into_owned();
        std::thread::spawn(move || {
            app.execute(ServiceRequest::StreamSessionExport { names, destination })
        })
    }
    fn await_export_conflicts(app: &VeilService) -> crate::import_pipeline::ConflictBatch {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if let ServiceResponse::ImportConflicts(Some(batch)) = app
                .execute(ServiceRequest::PollSessionExportConflicts)
                .unwrap()
            {
                return batch;
            }
            if std::time::Instant::now() > deadline {
                app.cancel_transfer();
                panic!("未收到导出冲突");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    fn await_export_end(
        app: &VeilService,
        task: std::thread::JoinHandle<Result<ServiceResponse, ServiceError>>,
    ) -> Result<ServiceResponse, ServiceError> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !task.is_finished() {
            if std::time::Instant::now() > deadline {
                app.cancel_transfer();
                panic!("导出未结束");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        task.join().unwrap()
    }
    #[test]
    fn stream_export_pages_folders_preserves_empty_directories_and_deduplicates_selection() {
        let (root, manager, app, source) = stream_import_fixture();
        let specs = (0..150)
            .map(|i| crate::veil_ops::AddFileSpec::new(&source, format!("album/{i:03}")))
            .collect::<Vec<_>>();
        manager.add_files(&specs, "password").unwrap();
        manager
            .create_directory("album/nested/empty", "password")
            .unwrap();
        let output = root.path().join("output");
        let task = run_stream_export(
            &app,
            vec!["album".into(), "album/000".into(), "album".into()],
            &output,
        );
        await_export_end(&app, task).unwrap();
        assert!(output.join("album/nested/empty").is_dir());
        for i in 0..150 {
            assert_eq!(
                std::fs::read(output.join(format!("album/{i:03}"))).unwrap(),
                b"original"
            );
        }
        let p = app.transfer_progress();
        assert_eq!(
            (
                p.total_files,
                p.completed_files,
                p.checked_files,
                p.skipped_files
            ),
            (150, 150, 150, 0)
        );
        assert!(p.scan_complete);
        assert_eq!(p.phase, "completed");
    }
    #[test]
    fn stream_export_waits_for_choices_and_reuses_directory_lock() {
        for action in [SessionImportAction::Skip, SessionImportAction::Overwrite] {
            let (root, manager, app, source) = stream_import_fixture();
            manager
                .add_files(
                    &[
                        crate::veil_ops::AddFileSpec::new(&source, "album/same"),
                        crate::veil_ops::AddFileSpec::new(&source, "album/clear"),
                    ],
                    "password",
                )
                .unwrap();
            let output = root.path().join("output");
            std::fs::create_dir_all(output.join("album")).unwrap();
            std::fs::write(output.join("album/same"), b"original").unwrap();
            let task = run_stream_export(&app, vec!["album".into()], &output);
            let batch = await_export_conflicts(&app);
            assert_eq!(batch.conflicts.len(), 1);
            assert!(batch.conflicts[0].same_content);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !output.join("album/clear").exists() {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let db = manager.open_db("password").unwrap();
            let resources = db
                .conn
                .prepare("SELECT resource_id FROM operation_locks")
                .unwrap()
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(resources.len(), 1);
            assert!(resources[0].contains("veil-dir:"));
            let cached: i64 = db
                .conn
                .query_row("SELECT count(*) FROM file_hash", [], |r| r.get(0))
                .unwrap();
            // 夹具预先放入一个源文件，当前导出目录另有两个文件。
            assert_eq!(cached, 3);
            drop(db);
            app.execute(ServiceRequest::ResolveSessionExportConflicts {
                id: batch.id,
                resolutions: vec![SessionImportResolution {
                    target: batch.conflicts[0].target.clone(),
                    action,
                }],
            })
            .unwrap();
            await_export_end(&app, task).unwrap();
            assert_eq!(app.transfer_progress().completed_files, 2);
            assert_eq!(
                app.transfer_progress().skipped_files,
                if matches!(action, SessionImportAction::Skip) {
                    1
                } else {
                    0
                }
            );
            assert_eq!(
                std::fs::read(output.join("album/same")).unwrap(),
                b"original"
            );
        }
    }
    #[test]
    fn stream_export_cancel_keeps_published_files_and_cleans_unpublished_plaintext() {
        let (root, manager, app, source) = stream_import_fixture();
        manager
            .add_files(
                &[
                    crate::veil_ops::AddFileSpec::new(&source, "album/same"),
                    crate::veil_ops::AddFileSpec::new(&source, "album/clear"),
                ],
                "password",
            )
            .unwrap();
        let output = root.path().join("output");
        std::fs::create_dir_all(output.join("album")).unwrap();
        std::fs::write(output.join("album/same"), b"keep").unwrap();
        let task = run_stream_export(&app, vec!["album".into()], &output);
        let _ = await_export_conflicts(&app);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.transfer_progress().completed_files == 0 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(app.cancel_transfer());
        assert!(await_export_end(&app, task).is_err());
        assert_eq!(app.transfer_progress().phase, "cancelled");
        assert_eq!(std::fs::read(output.join("album/same")).unwrap(), b"keep");
        assert_eq!(
            std::fs::read(output.join("album/clear")).unwrap(),
            b"original"
        );
        assert_eq!(std::fs::read_dir(output.join("album")).unwrap().count(), 2);
        assert!(matches!(
            app.execute(ServiceRequest::PollSessionExportConflicts)
                .unwrap(),
            ServiceResponse::ImportConflicts(None)
        ));
    }
    #[test]
    fn stream_export_rejects_changed_overwrite_target_and_internal_plaintext_destination() {
        let (root, manager, app, _) = stream_import_fixture();
        let output = root.path().join("out");
        std::fs::write(&output, b"before").unwrap();
        let task = run_stream_export(&app, vec!["file".into()], &output);
        let batch = await_export_conflicts(&app);
        std::fs::write(&output, b"changed after review").unwrap();
        app.execute(ServiceRequest::ResolveSessionExportConflicts {
            id: batch.id,
            resolutions: vec![SessionImportResolution {
                target: batch.conflicts[0].target.clone(),
                action: SessionImportAction::Overwrite,
            }],
        })
        .unwrap();
        assert!(await_export_end(&app, task).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"changed after review");
        assert_eq!(app.transfer_progress().completed_files, 0);
        assert!(
            app.execute(ServiceRequest::StreamSessionExport {
                names: vec!["file".into()],
                destination: manager
                    .veil_dir
                    .join("plain")
                    .to_string_lossy()
                    .into_owned()
            })
            .is_err()
        );
        assert!(!manager.veil_dir.join("plain").exists());
        assert_eq!(
            std::fs::read_dir(root.path())
                .unwrap()
                .filter(|e| e
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".tmp"))
                .count(),
            0
        );
    }
    #[test]
    fn stream_export_authentication_failure_removes_partial_plaintext() {
        let (root, manager, app, _) = stream_import_fixture();
        let node = manager
            .open_db("password")
            .unwrap()
            .resolve("file")
            .unwrap()
            .unwrap();
        let cipher = manager.veil_dir.join(node.cipher().unwrap());
        let mut bytes = std::fs::read(&cipher).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(cipher, bytes).unwrap();
        let output = root.path().join("output");
        std::fs::create_dir(&output).unwrap();
        std::fs::write(output.join("keep"), b"existing").unwrap();
        assert!(
            app.execute(ServiceRequest::StreamSessionExport {
                names: vec!["file".into()],
                destination: output.join("exported").to_string_lossy().into_owned(),
            })
            .is_err()
        );
        assert!(!output.join("exported").exists());
        assert_eq!(std::fs::read_dir(&output).unwrap().count(), 1);
        assert_eq!(std::fs::read(output.join("keep")).unwrap(), b"existing");
        assert_eq!(app.transfer_progress().completed_files, 0);
    }

    #[test]
    fn stream_export_unbounded_discovery_continues_when_conflicts_are_full() {
        let (root, manager, app, source) = stream_import_fixture();
        manager
            .add_files(
                &(0..180)
                    .map(|i| crate::veil_ops::AddFileSpec::new(&source, format!("album/{i:03}")))
                    .collect::<Vec<_>>(),
                "password",
            )
            .unwrap();
        let output = root.path().join("output");
        std::fs::create_dir_all(output.join("album")).unwrap();
        for i in 0..180 {
            std::fs::write(output.join(format!("album/{i:03}")), b"original").unwrap();
        }
        let task = run_stream_export(&app, vec!["album".into()], &output);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let p = app.transfer_progress();
            if p.scan_complete && p.waiting_files == 128 {
                assert_eq!(p.total_files, 180);
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(app.cancel_transfer());
        assert!(await_export_end(&app, task).is_err());
        assert_eq!(app.transfer_progress().completed_files, 0);
        assert_eq!(
            std::fs::read_dir(output.join("album")).unwrap().count(),
            180
        );
    }
    fn run_stream_import(
        app: &VeilService,
        source: &Path,
    ) -> std::thread::JoinHandle<Result<ServiceResponse, ServiceError>> {
        let worker = app.clone();
        let source = source.to_string_lossy().into_owned();
        std::thread::spawn(move || {
            worker.execute(ServiceRequest::StreamSessionImport {
                sources: vec![source],
                directory: String::new(),
            })
        })
    }
    fn await_stream_conflicts(app: &VeilService) -> crate::import_pipeline::ConflictBatch {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if let ServiceResponse::ImportConflicts(Some(batch)) = app
                .execute(ServiceRequest::PollSessionImportConflicts)
                .unwrap()
            {
                return batch;
            }
            if std::time::Instant::now() > deadline {
                app.cancel_transfer();
                panic!("未收到流式导入冲突");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    #[test]
    fn stream_import_commits_clear_files_while_waiting_for_explicit_choices() {
        let (root, manager, app, source) = stream_import_fixture();
        let folder = root.path().join("album");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("same"), b"original").unwrap();
        std::fs::write(folder.join("new"), b"new").unwrap();
        manager
            .add_files(
                &[crate::veil_ops::AddFileSpec::new(&source, "album/same")],
                "password",
            )
            .unwrap();
        let worker = run_stream_import(&app, &folder);
        let batch = await_stream_conflicts(&app);
        assert_eq!(batch.conflicts.len(), 1);
        assert!(batch.conflicts[0].same_content);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while manager
            .read_file_bytes("album/new", "password", 100)
            .is_err()
        {
            if std::time::Instant::now() > deadline {
                app.cancel_transfer();
                panic!("待确认文件不应阻塞其他文件提交");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let db = manager.open_db("password").unwrap();
        let resources = db
            .conn
            .prepare("SELECT resource_id FROM operation_locks")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(resources.len(), 1);
        assert!(
            resources[0].contains("veil-dir:"),
            "文件夹导入应复用目录锁，不能另加文件锁"
        );
        drop(db);
        assert!(!worker.is_finished());
        app.execute(ServiceRequest::ResolveSessionImportConflicts {
            id: batch.id,
            resolutions: vec![SessionImportResolution {
                target: "album/same".into(),
                action: SessionImportAction::Skip,
            }],
        })
        .unwrap();
        worker.join().unwrap().unwrap();
        let progress = app.transfer_progress();
        assert_eq!(
            (
                progress.total_files,
                progress.completed_files,
                progress.skipped_files
            ),
            (2, 2, 1)
        );
        assert!(progress.scan_complete);
        assert_eq!(progress.waiting_files, 0);
    }
    #[test]
    fn stream_import_rechecks_changed_versions_and_requires_another_choice() {
        let (_root, manager, app, source) = stream_import_fixture();
        let worker = run_stream_import(&app, &source);
        let batch = await_stream_conflicts(&app);
        std::fs::write(&source, b"concurrent edit").unwrap();
        manager.add_file(&source, "password").unwrap();
        app.execute(ServiceRequest::ResolveSessionImportConflicts {
            id: batch.id.clone(),
            resolutions: vec![SessionImportResolution {
                target: "file".into(),
                action: SessionImportAction::Overwrite,
            }],
        })
        .unwrap();
        let again = await_stream_conflicts(&app);
        assert_ne!(again.id, batch.id);
        assert!(
            app.execute(ServiceRequest::ResolveSessionImportConflicts {
                id: batch.id,
                resolutions: vec![SessionImportResolution {
                    target: "file".into(),
                    action: SessionImportAction::Skip
                }]
            })
            .is_err()
        );
        app.execute(ServiceRequest::ResolveSessionImportConflicts {
            id: again.id,
            resolutions: vec![SessionImportResolution {
                target: "file".into(),
                action: SessionImportAction::Skip,
            }],
        })
        .unwrap();
        worker.join().unwrap().unwrap();
        assert_eq!(
            manager.read_file_bytes("file", "password", 100).unwrap(),
            b"concurrent edit"
        );
        assert_eq!(app.transfer_progress().skipped_files, 1);
    }

    #[test]
    fn stream_import_resolves_multiple_bounded_batches_and_fills_missing_hashes() {
        let (root, manager, app, source) = stream_import_fixture();
        let folder = root.path().join("batches");
        std::fs::create_dir(&folder).unwrap();
        let mut specs = Vec::new();
        for index in 0..70 {
            std::fs::write(folder.join(format!("file-{index}")), b"original").unwrap();
            specs.push(crate::veil_ops::AddFileSpec::new(
                &source,
                format!("batches/file-{index}"),
            ));
        }
        manager.add_files(&specs, "password").unwrap();
        manager.open_db("password").unwrap().conn.execute("DELETE FROM file_hash WHERE file_id=(SELECT id FROM file_info WHERE name='file-0')", []).unwrap();
        let worker = run_stream_import(&app, &folder);
        let mut chosen = 0;
        while chosen < 70 {
            let batch = await_stream_conflicts(&app);
            assert!(batch.conflicts.len() <= 32);
            chosen += batch.conflicts.len();
            assert!(
                batch
                    .conflicts
                    .iter()
                    .all(|file| file.same_content && file.error.is_none())
            );
            app.execute(ServiceRequest::ResolveSessionImportConflicts {
                id: batch.id,
                resolutions: batch
                    .conflicts
                    .into_iter()
                    .map(|file| SessionImportResolution {
                        target: file.target,
                        action: SessionImportAction::Skip,
                    })
                    .collect(),
            })
            .unwrap();
        }
        worker.join().unwrap().unwrap();
        let progress = app.transfer_progress();
        assert_eq!(
            (
                progress.completed_files,
                progress.total_files,
                progress.skipped_files
            ),
            (70, 70, 70)
        );
        assert!(
            manager
                .list_files("password")
                .unwrap()
                .iter()
                .all(|file| file.hash.is_some())
        );
    }

    #[test]
    fn stream_import_unbounded_discovery_continues_while_conflict_queue_is_full() {
        let (root, manager, app, source) = stream_import_fixture();
        let folder = root.path().join("many");
        std::fs::create_dir(&folder).unwrap();
        let mut specs = Vec::new();
        for index in 0..600 {
            let path = folder.join(format!("file-{index}"));
            std::fs::write(&path, b"original").unwrap();
            specs.push(crate::veil_ops::AddFileSpec::new(
                &source,
                format!("many/file-{index}"),
            ));
        }
        manager.add_files(&specs, "password").unwrap();
        let worker = run_stream_import(&app, &folder);
        let _ = await_stream_conflicts(&app);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while (app.transfer_progress().waiting_files < 128
            || !app.transfer_progress().scan_complete)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let progress = app.transfer_progress();
        assert_eq!(progress.waiting_files, 128);
        assert_eq!(progress.total_files, 600);
        assert!(progress.scan_complete);
        assert_eq!(progress.completed_files, 0);
        assert!(app.cancel_transfer());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !worker.is_finished() {
            assert!(std::time::Instant::now() < deadline, "取消时队列未退出");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(matches!(
            worker.join().unwrap(),
            Err(ServiceError::ImportCancelled { .. })
        ));
    }

    fn stream_import_fixture() -> (tempfile::TempDir, VeilManager, VeilService, PathBuf) {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("container");
        let manager = VeilManager::new(path.clone());
        let metadata = manager.init_veil("test", "password").unwrap();
        let app = VeilService::default();
        *app.3.write().unwrap() = Some(path.clone());
        *app.0.lock().unwrap() = Some(Session {
            path,
            veil_id: metadata.veil_id.clone(),
            password: Zeroizing::new("password".into()),
        });
        let source = root.path().join("file");
        std::fs::write(&source, b"original").unwrap();
        manager.add_file(&source, "password").unwrap();
        (root, manager, app, source)
    }

    #[test]
    fn import_flushes_on_interval_finishes_last_batch_and_cleans_cancelled_queue() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        for scenario in ["timer", "count", "last", "cancel"] {
            let root = tempfile::TempDir::new().unwrap();
            let path = root.path().join("container");
            let manager = VeilManager::new(path.clone());
            let metadata = manager.init_veil("test", "password").unwrap();
            let app = VeilService::default();
            *app.0.lock().unwrap() = Some(Session {
                path: path.clone(),
                veil_id: metadata.veil_id.clone(),
                password: Zeroizing::new("password".into()),
            });
            let count = if scenario == "last" { 1 } else { 24 };
            let sources = (0..count)
                .map(|index| {
                    let source = root.path().join(format!("file-{index}"));
                    std::fs::write(&source, vec![index as u8; 4096]).unwrap();
                    source.to_string_lossy().into_owned()
                })
                .collect();
            let worker = app.clone();
            let container = path.clone();
            let interval = if scenario == "timer" {
                std::time::Duration::from_millis(50)
            } else {
                std::time::Duration::from_secs(10)
            };
            let threshold = if scenario == "count" { 2 } else { 100 };
            let start = std::time::Instant::now();
            let task = std::thread::spawn(move || {
                let _scope = worker.1.begin("import");
                worker.import_sources_with_interval(
                    sources,
                    String::new(),
                    &container,
                    interval,
                    threshold,
                )
            });
            if scenario == "timer" {
                let limit = std::time::Instant::now() + std::time::Duration::from_secs(8);
                while app.transfer_progress().completed_files == 0 && !task.is_finished() {
                    assert!(std::time::Instant::now() < limit);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                assert!(app.transfer_progress().completed_files > 0);
                assert!(app.transfer_progress().completed_files < count as u64);
                assert!(!task.is_finished());
            }
            if scenario == "count" {
                let limit = std::time::Instant::now() + std::time::Duration::from_secs(8);
                while app.transfer_progress().completed_files < 2 && !task.is_finished() {
                    assert!(std::time::Instant::now() < limit);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                assert!(app.transfer_progress().completed_files >= 2);
                assert!(app.transfer_progress().completed_files < count as u64);
                assert!(!task.is_finished());
                assert!(start.elapsed() < interval);
            }
            if scenario == "cancel" {
                let limit = std::time::Instant::now() + std::time::Duration::from_secs(8);
                while app.transfer_progress().pending_files == 0 && !task.is_finished() {
                    assert!(std::time::Instant::now() < limit);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                assert_eq!(app.transfer_progress().completed_files, 0);
                assert!(manager.read_meta("password").unwrap().files.is_empty());
                assert!(app.cancel_transfer());
            }
            let result = task.join().unwrap();
            assert_eq!(app.transfer_progress().pending_files, 0);
            if scenario == "cancel" {
                assert!(matches!(
                    result,
                    Err(ServiceError::ImportCancelled { completed: 0, .. })
                ));
                assert!(manager.read_meta("password").unwrap().files.is_empty());
                assert_eq!(
                    std::fs::read_dir(&path)
                        .unwrap()
                        .filter_map(Result::ok)
                        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "enc"))
                        .count(),
                    0
                );
            } else {
                assert!(result.is_ok());
                assert_eq!(app.transfer_progress().completed_files, count as u64);
                if scenario == "timer" {
                    assert!(start.elapsed() >= std::time::Duration::from_millis(40));
                } else if scenario == "last" {
                    assert!(start.elapsed() < interval);
                }
            }
        }
    }

    #[test]
    fn multi_file_preparation_and_commit_are_atomic_and_count_every_file() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        for scenario in ["ok", "key", "lease"] {
            let fail = scenario != "ok";
            let root = tempfile::tempdir().unwrap();
            let manager = VeilManager::new(root.path().join("container"));
            manager.init_veil("test", "password").unwrap();
            let source = root.path().join("source");
            std::fs::write(&source, b"content").unwrap();
            let files = (0..16)
                .map(|index| {
                    crate::veil_ops::AddFileSpec::new(&source, format!("album/file-{index}"))
                        .without_overwrite()
                })
                .collect::<Vec<_>>();
            let progress = veil_core::progress::Progress::default();
            let _scope = progress.begin("import");
            veil_core::progress::start(None);
            veil_core::progress::set_file_count(files.len());
            let (sender, receiver) = veil_core::queue::BoundedQueue::new(2).split();
            let context = crate::veil_ops::ImportBatchContext {
                directories: Arc::default(),
                sender,
                directory_guard: None,
                owner: crate::execution::ExecutionService::global()
                    .unwrap()
                    .runtime()
                    .current_context()
                    .owner()
                    .clone(),
            };
            manager
                .add_files_for_import(&files, "password", None, &Mutex::new(()), &context)
                .unwrap();
            assert_eq!(progress.snapshot().pending_files, 16);
            assert!(manager.list_files("password").unwrap().is_empty());
            let mut queued = receiver.recv().unwrap();
            assert_eq!(queued.file_count(), 16);
            match scenario {
                "key" => {
                    queued.invalidate_key();
                }
                "lease" => {
                    manager
                        .open_db("password")
                        .unwrap()
                        .conn
                        .execute(
                            "UPDATE operation_locks SET owner_token='invalidated-token'",
                            [],
                        )
                        .unwrap();
                }
                _ => {}
            }
            let result = manager.commit_import_batch(
                vec![queued],
                "password",
                &std::sync::atomic::AtomicBool::new(false),
            );
            assert_eq!(result.is_err(), fail);
            assert_eq!(progress.snapshot().pending_files, 0);
            assert_eq!(
                progress.snapshot().completed_files,
                if fail { 0 } else { 16 }
            );
            assert_eq!(
                manager.list_files("password").unwrap().len(),
                if fail { 0 } else { 16 }
            );
        }
    }

    #[test]
    fn encrypted_import_records_commit_together_or_roll_back_together() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        for fail in [false, true] {
            let root = tempfile::TempDir::new().unwrap();
            let manager = VeilManager::new(root.path().join("container"));
            manager.init_veil("test", "password").unwrap();
            let paths = [root.path().join("good"), root.path().join("bad")];
            for path in &paths {
                std::fs::write(path, b"content").unwrap();
            }
            let gate = Mutex::new(());
            let owner = crate::execution::ExecutionService::global()
                .unwrap()
                .runtime()
                .current_context()
                .owner()
                .clone();
            let (sender, receiver) = veil_core::queue::BoundedQueue::new(2).split();
            std::thread::scope(|scope| {
                let mut workers = Vec::new();
                for path in &paths {
                    let sender = sender.clone();
                    let gate = &gate;
                    let manager = &manager;
                    let owner = owner.clone();
                    workers.push(scope.spawn(move || {
                        let context = crate::veil_ops::ImportBatchContext {
                            directories: Arc::default(),
                            sender,
                            directory_guard: None,
                            owner,
                        };
                        let spec = crate::veil_ops::AddFileSpec::new(
                            path,
                            path.file_name().unwrap().to_str().unwrap(),
                        )
                        .without_overwrite();
                        import_one_file(manager, &spec, "password", None, gate, &context)
                    }));
                }
                let first = receiver
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap();
                let mut second = receiver
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap();
                for worker in workers {
                    assert!(worker.join().unwrap().is_ok());
                }
                // 加密线程已退出时，待提交租约仍归属于整个导入任务。
                if !fail {
                    std::thread::sleep(std::time::Duration::from_secs(6));
                }
                manager.cleanup("password").unwrap();
                let failed_name = fail.then(|| second.invalidate_key());
                assert!(manager.read_meta("password").unwrap().files.is_empty());
                let result = manager.commit_import_batch(
                    vec![first, second],
                    "password",
                    &std::sync::atomic::AtomicBool::new(false),
                );
                if let Some(name) = &failed_name {
                    assert!(
                        matches!(result, Err(veil_core::error::VeilError::ImportBatchFailed { file, .. }) if &file == name)
                    );
                } else {
                    assert!(result.is_ok());
                }
            });
            assert_eq!(
                manager.read_meta("password").unwrap().files.len(),
                if fail { 0 } else { 2 }
            );
            if fail {
                let db = crate::storage::container::ContainerDb::open(
                    &manager.veil_dir,
                    "password",
                    None,
                )
                .unwrap();
                let hashes: i64 = db
                    .conn
                    .query_row("SELECT count(*) FROM file_hash", [], |row| row.get(0))
                    .unwrap();
                assert_eq!(hashes, 0);
                assert_eq!(
                    std::fs::read_dir(&manager.veil_dir)
                        .unwrap()
                        .filter_map(Result::ok)
                        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "enc"))
                        .count(),
                    0
                );
            }
        }
    }

    #[test]
    fn preview_can_read_during_shared_write_and_export_uses_shared_write() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::tempdir().unwrap();
        let manager = VeilManager::new(root.path().join("container"));
        manager.init_veil("test", "password").unwrap();
        let source = root.path().join("source");
        std::fs::write(&source, b"content").unwrap();
        manager
            .add_files(
                &[crate::veil_ops::AddFileSpec::new(&source, "file.txt")],
                "password",
            )
            .unwrap();
        let db = crate::storage::container::ContainerDb::open(&manager.veil_dir, "password", None)
            .unwrap();
        let file = db.resolve("file.txt").unwrap().unwrap().id;
        let veil_id = db.info().unwrap().0;
        drop(db);
        let app = VeilService::default();
        *app.0.lock().unwrap() = Some(Session {
            path: manager.veil_dir.clone(),
            veil_id,
            password: Zeroizing::new("password".into()),
        });
        let guard = crate::storage::locks::LockManager::new(&manager.veil_dir, "password")
            .acquire(&[file], crate::storage::locks::LockMode::SharedWrite)
            .unwrap();
        assert!(
            matches!(app.execute(ServiceRequest::PreviewSessionFile { name: "file.txt".into() }).unwrap(), ServiceResponse::PreviewUnstable(bytes) if bytes == b"content")
        );
        let db = crate::storage::container::ContainerDb::open(&manager.veil_dir, "password", None)
            .unwrap();
        let count: i64 = db
            .conn
            .query_row("SELECT count(*) FROM operation_locks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        drop(db);
        drop(guard);
        assert!(matches!(
            app.execute(ServiceRequest::ExportSessionFile {
                name: "file.txt".into(),
                destination: root.path().join("out").to_string_lossy().into_owned()
            })
            .unwrap(),
            ServiceResponse::Done
        ));
    }

    #[test]
    fn folder_workers_reuse_the_directory_lease_without_file_leases() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let manager = VeilManager::new(root.path().join("container"));
        manager.init_veil("test", "password").unwrap();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (guard, directories) = manager
            .lock_import_directories(&["album".into()], "password", cancel)
            .unwrap();
        let source = root.path().join("source");
        std::fs::write(&source, b"content").unwrap();
        manager
            .add_files_in_directory(
                &[crate::veil_ops::AddFileSpec::new(&source, "album/file")],
                "password",
                &guard,
                &directories[0],
            )
            .unwrap();
        assert!(
            manager
                .add_files_in_directory(
                    &[crate::veil_ops::AddFileSpec::new(&source, "other/file")],
                    "password",
                    &guard,
                    &directories[0],
                )
                .is_err()
        );
        let db = crate::storage::container::ContainerDb::open(&manager.veil_dir, "password", None)
            .unwrap();
        let resources: Vec<String> = {
            let mut query = db
                .conn
                .prepare("SELECT resource_id FROM operation_locks")
                .unwrap();
            query
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(
            resources,
            [guard.credentials().unwrap()[0].resource_id.clone()]
        );
        drop(db);
        drop(guard);
        assert_eq!(
            manager
                .read_file_bytes("album/file", "password", 100)
                .unwrap(),
            b"content"
        );
    }

    #[test]
    fn folder_import_uses_one_directory_lease_with_parallel_workers() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("container");
        let manager = VeilManager::new(path.clone());
        let metadata = manager.init_veil("test", "password").unwrap();
        let app = VeilService::default();
        *app.3.write().unwrap() = Some(path.clone());
        *app.0.lock().unwrap() = Some(Session {
            path,
            veil_id: metadata.veil_id.clone(),
            password: Zeroizing::new("password".into()),
        });
        let folder = root.path().join("album");
        std::fs::create_dir(&folder).unwrap();
        for index in 0..24 {
            std::fs::write(
                folder.join(format!("file-{index}")),
                vec![index as u8; 4096],
            )
            .unwrap();
        }
        app.execute(ServiceRequest::ImportSessionFiles {
            sources: vec![folder.to_string_lossy().into_owned()],
            directory: String::new(),
        })
        .unwrap();
        assert_eq!(app.transfer_progress().completed_files, 24);
        for index in [0, 11, 23] {
            assert_eq!(
                manager
                    .read_file_bytes(&format!("album/file-{index}"), "password", 4096)
                    .unwrap(),
                vec![index as u8; 4096]
            );
        }
        // 已登记哈希用于重复内容比较；再次导入应跳过相同文件。
        app.execute(ServiceRequest::ImportSessionFiles {
            sources: vec![folder.to_string_lossy().into_owned()],
            directory: String::new(),
        })
        .unwrap();
        assert_eq!(app.transfer_progress().skipped_files, 24);
        assert!(
            manager
                .list_files("password")
                .unwrap()
                .iter()
                .all(|file| file.hash.is_some())
        );
        std::fs::write(folder.join("file-0"), vec![99u8; 4096]).unwrap();
        assert!(matches!(
            app.execute(ServiceRequest::ImportSessionFiles {
                sources: vec![folder.to_string_lossy().into_owned()],
                directory: String::new(),
            }),
            Err(ServiceError::ImportFileFailed {
                source: veil_core::error::VeilError::FileAlreadyExists(_),
                ..
            })
        ));
    }

    #[test]
    fn mixed_file_and_multiple_folder_imports_use_their_own_scopes() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("container");
        let manager = VeilManager::new(path.clone());
        let metadata = manager.init_veil("test", "password").unwrap();
        let app = VeilService::default();
        *app.3.write().unwrap() = Some(path.clone());
        *app.0.lock().unwrap() = Some(Session {
            path,
            veil_id: metadata.veil_id.clone(),
            password: Zeroizing::new("password".into()),
        });
        let first = root.path().join("first");
        let second = root.path().join("second");
        let standalone = root.path().join("single.txt");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        std::fs::write(first.join("a"), b"a").unwrap();
        std::fs::write(second.join("b"), b"b").unwrap();
        std::fs::write(&standalone, b"single").unwrap();
        app.execute(ServiceRequest::ImportSessionFiles {
            sources: [&first, &second, &standalone]
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            directory: String::new(),
        })
        .unwrap();
        assert_eq!(
            manager.read_file_bytes("first/a", "password", 100).unwrap(),
            b"a"
        );
        assert_eq!(
            manager
                .read_file_bytes("second/b", "password", 100)
                .unwrap(),
            b"b"
        );
        assert_eq!(
            manager
                .read_file_bytes("single.txt", "password", 100)
                .unwrap(),
            b"single"
        );
    }

    #[test]
    fn waiting_for_directory_lock_can_be_cancelled() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("container");
        let manager = VeilManager::new(path.clone());
        let metadata = manager.init_veil("test", "password").unwrap();
        manager.create_directory("input", "password").unwrap();
        let root_id = crate::storage::container::ContainerDb::open(&path, "password", None)
            .unwrap()
            .root()
            .unwrap()
            .id;
        let external = crate::storage::locks::LockManager::new(&path, "password")
            .acquire_directory(&root_id, crate::storage::locks::LockMode::Write)
            .unwrap();
        let app = VeilService::default();
        *app.3.write().unwrap() = Some(path.clone());
        *app.0.lock().unwrap() = Some(Session {
            path,
            veil_id: metadata.veil_id.clone(),
            password: Zeroizing::new("password".into()),
        });
        let folder = root.path().join("input");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("file"), b"test").unwrap();
        let worker = app.clone();
        let task = std::thread::spawn(move || {
            worker.execute(ServiceRequest::ImportSessionFiles {
                sources: vec![folder.to_string_lossy().into_owned()],
                directory: String::new(),
            })
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.transfer_progress().phase != "preparing" && !task.is_finished() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let reader = app.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let refresh = std::thread::spawn(move || {
            let _ = sender.send(reader.execute(ServiceRequest::RefreshContainer).is_ok());
        });
        let refreshed = receiver.recv_timeout(std::time::Duration::from_secs(3));
        assert!(app.cancel_transfer());
        assert!(matches!(
            task.join().unwrap(),
            Err(ServiceError::ImportCancelled { completed: 0, .. })
        ));
        refresh.join().unwrap();
        assert_eq!(refreshed.ok(), Some(true));
        assert_eq!(app.transfer_progress().phase, "cancelled");
        drop(external);
    }

    #[test]
    fn shared_directory_lease_can_be_renewed_before_file_commit() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let manager = VeilManager::new(root.path().join("container"));
        manager.init_veil("test", "password").unwrap();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (guard, directories) = manager
            .lock_import_directories(&[String::new()], "password", cancel)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_secs(6));
        let source = root.path().join("file");
        std::fs::write(&source, b"content").unwrap();
        manager
            .add_files_in_directory(
                &[crate::veil_ops::AddFileSpec::new(&source, "file")],
                "password",
                &guard,
                &directories[0],
            )
            .unwrap();
        drop(guard);
        assert_eq!(
            manager.read_file_bytes("file", "password", 100).unwrap(),
            b"content"
        );
    }

    #[test]
    fn parallel_small_file_import_keeps_every_file_and_reports_progress() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("container");
        let manager = VeilManager::new(path.clone());
        let metadata = manager.init_veil("test", "password").unwrap();
        let app = VeilService::default();
        *app.3.write().unwrap() = Some(path.clone());
        *app.0.lock().unwrap() = Some(Session {
            path,
            veil_id: metadata.veil_id.clone(),
            password: Zeroizing::new("password".into()),
        });
        let mut sources = Vec::new();
        for index in 0..12 {
            let path = root.path().join(format!("file-{index}.txt"));
            std::fs::write(&path, vec![index as u8; 4096]).unwrap();
            sources.push(path.to_string_lossy().into_owned());
        }
        app.execute(ServiceRequest::ImportSessionFiles {
            sources,
            directory: "nested".into(),
        })
        .unwrap();
        let progress = app.transfer_progress();
        assert_eq!(
            (
                progress.phase,
                progress.completed_files,
                progress.total_files
            ),
            ("completed", 12, 12)
        );
        assert_eq!(
            (progress.processed_bytes, progress.total_bytes),
            (12 * 4096, Some(12 * 4096))
        );
        assert!((1..=4).contains(&progress.parallel_workers));
        assert_eq!(manager.read_meta("password").unwrap().files.len(), 12);
        for index in [0, 5, 11] {
            assert_eq!(
                manager
                    .read_file_bytes(&format!("nested/file-{index}.txt"), "password", 4096)
                    .unwrap(),
                vec![index as u8; 4096]
            );
        }
    }

    #[test]
    fn cancelling_import_preserves_committed_files_and_allows_retry() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("container");
        let manager = VeilManager::new(path.clone());
        let metadata = manager.init_veil("test", "password").unwrap();
        let app = VeilService::default();
        *app.3.write().unwrap() = Some(path.clone());
        *app.0.lock().unwrap() = Some(Session {
            path,
            veil_id: metadata.veil_id.clone(),
            password: Zeroizing::new("password".into()),
        });
        let mut sources = Vec::new();
        for index in 0..24 {
            let path = root.path().join(format!("file-{index}.txt"));
            std::fs::write(&path, vec![index as u8; 1024 * 1024]).unwrap();
            sources.push(path.to_string_lossy().into_owned());
        }
        let worker = app.clone();
        let worker_sources = sources.clone();
        let task = std::thread::spawn(move || {
            worker.execute(ServiceRequest::ImportSessionFiles {
                sources: worker_sources,
                directory: String::new(),
            })
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while app.transfer_progress().completed_files == 0 && !task.is_finished() {
            assert!(std::time::Instant::now() < deadline, "导入未开始");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(app.cancel_transfer());
        let result = task.join().unwrap();
        let completed = match result {
            Err(ServiceError::ImportCancelled { completed, .. }) => completed,
            _ => panic!("预期中止导入"),
        };
        assert!(completed > 0 && completed < 24);
        assert_eq!(app.transfer_progress().phase, "cancelled");
        assert_eq!(
            manager.read_meta("password").unwrap().files.len(),
            completed as usize
        );
        app.execute(ServiceRequest::ImportSessionFiles {
            sources,
            directory: String::new(),
        })
        .unwrap();
        assert_eq!(manager.read_meta("password").unwrap().files.len(), 24);
        assert_eq!(app.transfer_progress().skipped_files, completed);
    }

    #[test]
    fn individual_file_commit_does_not_finish_the_import_progress() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let manager = VeilManager::new(root.path().join("container"));
        manager.init_veil("test", "password").unwrap();
        let source = root.path().join("file.txt");
        std::fs::write(&source, b"data").unwrap();
        let progress = veil_core::progress::Progress::default();
        let _scope = progress.begin("import");
        veil_core::progress::start(Some(8));
        veil_core::progress::set_file_count(2);
        veil_core::progress::begin_file("file.txt");
        manager
            .add_files_without_cleanup(
                &[crate::veil_ops::AddFileSpec::new(&source, "file.txt")],
                "password",
            )
            .unwrap();
        assert_eq!(
            std::fs::read_dir(manager.veil_dir.join(".staging"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(progress.snapshot().phase, "processing");
        veil_core::progress::file_done(false);
        veil_core::progress::begin_file("next.txt");
        manager
            .add_files_without_cleanup(
                &[crate::veil_ops::AddFileSpec::new(&source, "next.txt")],
                "password",
            )
            .unwrap();
        assert_eq!(progress.snapshot().phase, "processing");
        veil_core::progress::finalizing();
        assert_eq!(progress.snapshot().phase, "finalizing");
    }

    #[test]
    fn duplicate_import_uses_container_target_and_selected_absolute_paths() {
        let root = tempfile::TempDir::new().unwrap();
        let source = root.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let first = source.join("first.txt");
        let second = source.join("second.txt");
        std::fs::write(&first, b"first").unwrap();
        std::fs::write(&second, b"second").unwrap();
        let app = VeilService::default();
        let container = root.path().join("container");
        *app.3.write().unwrap() = Some(container.clone());
        let first_path = first.to_string_lossy().into_owned();
        let second_path = second.to_string_lossy().into_owned();
        let directory_path = source.to_string_lossy().into_owned();
        let keys = vec![directory_path.clone(), first_path.clone()];
        let original_keys = keys.clone();
        let session_lock = app.0.lock().unwrap();
        let worker = app.clone();
        let original = std::thread::spawn(move || {
            worker.execute(ServiceRequest::ImportSessionFiles {
                sources: original_keys,
                directory: "target".into(),
            })
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !import_registry()
            .lock()
            .unwrap()
            .iter()
            .any(|key| key.container == container)
        {
            assert!(std::time::Instant::now() < deadline, "导入任务未登记");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // 重复判断只使用所选路径，目录内容和文件内容的变化不改变任务标识。
        std::fs::write(&first, b"changed").unwrap();
        std::fs::write(source.join("added.txt"), b"new").unwrap();
        let duplicate = VeilService::default();
        *duplicate.3.write().unwrap() = Some(container.clone());
        let (send, receive) = std::sync::mpsc::channel();
        let duplicate_thread = std::thread::spawn(move || {
            let result = duplicate.execute(ServiceRequest::ImportSessionFiles {
                sources: vec![first_path, directory_path],
                directory: "target".into(),
            });
            send.send(matches!(result, Err(ServiceError::DuplicateImport)))
                .unwrap();
        });
        let result = receive.recv_timeout(std::time::Duration::from_secs(2));
        assert!(app.register_import(&keys, "other").is_ok());
        assert!(app.register_import(&[second_path], "target").is_ok());
        let other_container = VeilService::default();
        *other_container.3.write().unwrap() = Some(root.path().join("other-container"));
        assert!(other_container.register_import(&keys, "target").is_ok());
        drop(session_lock);
        duplicate_thread.join().unwrap();
        assert!(result.unwrap());
        assert!(matches!(
            original.join().unwrap(),
            Err(ServiceError::NotOpen)
        ));
        assert!(app.register_import(&keys, "target").is_ok());
    }

    #[test]
    fn partial_import_is_committed_and_retry_skips_only_matching_files() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("container");
        let manager = VeilManager::new(path.clone());
        let meta = manager.init_veil("test", "password").unwrap();
        let app = VeilService::default();
        *app.3.write().unwrap() = Some(path.clone());
        *app.0.lock().unwrap() = Some(Session {
            path,
            veil_id: meta.veil_id.clone(),
            password: Zeroizing::new("password".into()),
        });
        let first = root.path().join("first.txt");
        let conflict = root.path().join("conflict.txt");
        let last = root.path().join("last.txt");
        std::fs::write(&first, b"first").unwrap();
        std::fs::write(&conflict, b"original").unwrap();
        manager.add_file(&conflict, "password").unwrap();
        std::fs::write(&conflict, b"changed!").unwrap();
        std::fs::write(&last, b"last").unwrap();
        let request = || ServiceRequest::ImportSessionFiles {
            sources: [&first, &conflict, &last]
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            directory: String::new(),
        };
        let error = match app.execute(request()) {
            Err(error) => error,
            Ok(_) => panic!("预期同名文件冲突"),
        };
        let completed = match error {
            ServiceError::ImportFileFailed {
                completed,
                skipped: 0,
                ..
            } => completed,
            _ => panic!("预期报告部分完成的导入"),
        };
        assert_eq!(
            manager.read_meta("password").unwrap().files.len(),
            completed as usize + 1
        );
        for (name, expected) in [
            ("first.txt", b"first".as_slice()),
            ("last.txt", b"last".as_slice()),
        ] {
            if let Ok(content) = manager.read_file_bytes(name, "password", 100) {
                assert_eq!(content, expected);
            }
        }
        assert_eq!(
            manager
                .read_file_bytes("conflict.txt", "password", 100)
                .unwrap(),
            b"original"
        );
        std::fs::write(&conflict, b"original").unwrap();
        app.execute(request()).unwrap();
        let progress = app.transfer_progress();
        assert_eq!(
            (
                progress.phase,
                progress.completed_files,
                progress.skipped_files,
                progress.total_files
            ),
            ("completed", 3, completed + 1, 3)
        );
        assert_eq!(
            manager
                .read_file_bytes("last.txt", "password", 100)
                .unwrap(),
            b"last"
        );
    }

    #[test]
    fn failed_transfer_reports_failed_status() {
        let app = VeilService::default();
        *app.3.write().unwrap() = Some(PathBuf::from("/test/container"));
        assert!(
            app.execute(ServiceRequest::ImportSessionFiles {
                sources: vec![],
                directory: String::new()
            })
            .is_err()
        );
        let progress = app.transfer_progress();
        assert_eq!(progress.operation, "import");
        assert_eq!(progress.phase, "failed");
    }

    fn container(response: ServiceResponse) -> ContainerView {
        match response {
            ServiceResponse::Container(view) => view,
            _ => panic!("预期返回容器"),
        }
    }

    #[test]
    fn create_lock_and_open_container() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let app = VeilService::default();
        let created = container(
            app.execute(ServiceRequest::CreateContainer {
                parent: root.path().display().to_string(),
                name: "我的容器".into(),
                password: "password".into(),
            })
            .unwrap(),
        );
        assert_eq!(created.name, "我的容器");
        assert!(Path::new(&created.path).join("veil.db").exists());
        assert_eq!(
            container(app.execute(ServiceRequest::RefreshContainer).unwrap()).id,
            created.id
        );
        let source = root.path().join("example.txt");
        std::fs::write(&source, b"example").unwrap();
        VeilManager::new(PathBuf::from(&created.path))
            .add_file(&source, "password")
            .unwrap();
        let refreshed = container(app.execute(ServiceRequest::RefreshContainer).unwrap());
        assert_eq!(refreshed.files.len(), 1);
        assert_eq!(refreshed.files[0].name, "example.txt");
        assert_eq!(refreshed.files[0].version, 1);
        assert!(matches!(
            app.execute(ServiceRequest::LockContainer),
            Ok(ServiceResponse::Locked)
        ));
        assert!(matches!(
            app.execute(ServiceRequest::RefreshContainer),
            Err(ServiceError::NotOpen)
        ));
        assert!(
            app.execute(ServiceRequest::OpenContainer {
                path: created.path.clone(),
                password: "wrong".into()
            })
            .is_err()
        );
        let opened = container(
            app.execute(ServiceRequest::OpenContainer {
                path: created.path,
                password: "password".into(),
            })
            .unwrap(),
        );
        assert_eq!(opened.id, created.id);
    }

    #[test]
    fn requests_preserve_version_conflicts() {
        use crate::operations::{ContainerOperation as Op, ContainerOperationResult as Out};
        use crate::veil_ops::AddFileSpec;
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let service = VeilService::default();
        let created = container(
            service
                .execute(ServiceRequest::CreateContainer {
                    parent: root.path().display().to_string(),
                    name: "test".into(),
                    password: "password".into(),
                })
                .unwrap(),
        );
        let dir = Path::new(&created.path);
        let source = root.path().join("source.txt");
        std::fs::write(&source, b"one").unwrap();
        let run = |operation| {
            service.execute(ServiceRequest::OperateContainer {
                directory: dir,
                veil_id: &created.id,
                operation,
            })
        };
        let files = [AddFileSpec::new(&source, "source.txt")];
        assert!(matches!(
            run(Op::AddFiles {
                files: &files,
                password: "password"
            }),
            Ok(ServiceResponse::Operation(Out::Added(_)))
        ));
        let output = root.path().join("export.txt");
        let version = match run(Op::ExtractFileWithVersion {
            name: "source.txt",
            output: &output,
            password: "password",
        })
        .unwrap()
        {
            ServiceResponse::Operation(Out::Version(version)) => version,
            _ => panic!("预期返回版本快照"),
        };
        std::fs::write(&source, b"two").unwrap();
        run(Op::AddFiles {
            files: &files,
            password: "password",
        })
        .unwrap();
        let stale = [AddFileSpec::new(&source, "source.txt").with_base_version(version)];
        assert!(matches!(
            run(Op::AddFiles {
                files: &stale,
                password: "password"
            }),
            Err(ServiceError::Core(
                veil_core::error::VeilError::VersionConflict(_)
            ))
        ));
        let package = root.path().join("existing.veil");
        std::fs::write(&package, b"existing").unwrap();
        assert!(
            run(Op::Pack {
                output: &package,
                password: "password"
            })
            .is_err()
        );
        assert_eq!(std::fs::read(package).unwrap(), b"existing");
    }
}
