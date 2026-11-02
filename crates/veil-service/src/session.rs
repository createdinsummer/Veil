//! 面向调用方的容器会话请求入口。

use crate::veil_ops::VeilManager;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use veil_core::metadata::{MetaData, generate_veil_id};
use zeroize::Zeroizing;

/// 接收来自命令行、桌面端或移动端的业务请求。
#[derive(Clone, Default)]
pub struct VeilService(Arc<Mutex<Option<Session>>>);

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
    PreviewSessionFile {
        name: String,
    },
    ExportSessionFile {
        name: String,
        destination: String,
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

/// 业务请求的结果。
pub enum ServiceResponse {
    ExecutionStatus(crate::execution::Liveness),
    Container(ContainerView),
    Locked,
    Spaces(Vec<crate::spaces::SpaceView>),
    Recents(Vec<crate::storage::app::RecentContainer>),
    Setting(serde_json::Value),
    Preview(Vec<u8>),
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
    #[error("容器创建未完成：{source}。目录保留在 {path}")]
    CreateFailed {
        path: PathBuf,
        #[source]
        source: veil_core::error::VeilError,
    },
    #[error("服务响应类型不符合请求")]
    UnexpectedResponse,
}

struct Session {
    path: PathBuf,
    veil_id: String,
    password: Zeroizing<String>,
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
    /// 调用方需持有返回的句柄，供其他进程查询本进程的线程或任务。
    pub fn listen_execution(&self) -> std::io::Result<crate::execution::ExecutionServer> {
        crate::execution::ExecutionService::global()?.listen()
    }

    /// 接收容器会话请求，执行对应的参数校验和业务流程。
    pub fn execute(&self, request: ServiceRequest<'_>) -> Result<ServiceResponse, ServiceError> {
        let execution =
            crate::execution::ExecutionService::global().map_err(|source| ServiceError::Io {
                operation: "无法初始化执行上下文",
                source,
            })?;
        let _scope = execution.enter_task("VeilService::execute");
        self.execute_registered(request)
    }

    fn execute_registered(
        &self,
        request: ServiceRequest<'_>,
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
            ServiceRequest::UnregisterContainer { id } => {
                crate::storage::app::AppStore::global()?.unregister(id)?;
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::MoveContainer {
                id,
                target_space,
                password,
            } => {
                crate::spaces::move_container(id, target_space, password)?;
                let store = crate::storage::app::AppStore::global()?;
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
                    session.path = destination;
                    session.password = Zeroizing::new(password.into());
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
            ServiceRequest::CreateSpace { name } => {
                crate::spaces::create_space(&name)?;
                Ok(ServiceResponse::Spaces(crate::spaces::list_spaces()?))
            }
            ServiceRequest::CreateInSpace {
                space,
                name,
                password,
            } => {
                let password = Zeroizing::new(password);
                let created = crate::spaces::create_container(&space, &name, password.as_str())?;
                self.open(
                    created.directory.to_string_lossy().into_owned(),
                    password.to_string(),
                )
                .map(ServiceResponse::Container)
            }
            ServiceRequest::ImportSessionFiles { sources, directory } => self
                .import_sources(sources, directory)
                .map(ServiceResponse::Container),
            ServiceRequest::PreviewSessionFile { name } => {
                self.preview_file(&name).map(ServiceResponse::Preview)
            }
            ServiceRequest::ExportSessionFile { name, destination } => {
                self.export_file(&name, &destination)?;
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::RemoveSessionPath { name } => {
                self.remove_path(&name).map(ServiceResponse::Container)
            }
            ServiceRequest::OperateContainer {
                directory,
                veil_id,
                operation,
            } => {
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
            } => Ok(ServiceResponse::Operation(crate::commands::operate(
                container, operation,
            )?)),
            ServiceRequest::PrepareImport {
                selector,
                source,
                destination,
            } => Ok(ServiceResponse::ImportPrepared(
                crate::commands::prepare_import(selector, source, destination)?,
            )),
            ServiceRequest::CommitImport { plan, password } => Ok(ServiceResponse::Imported(
                crate::commands::commit_import(plan, password)?,
            )),
            ServiceRequest::PrepareContainerPack { selector, output } => {
                Ok(ServiceResponse::ContainerPackPrepared(
                    crate::commands::prepare_pack(selector, output)?,
                ))
            }
            ServiceRequest::CommitContainerPack { plan, password } => Ok(
                ServiceResponse::ContainerPacked(crate::commands::commit_pack(plan, password)?),
            ),
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
        *session = Some(Session {
            path,
            veil_id: id,
            password,
        });
        Ok(view)
    }

    fn open(&self, path: String, password: String) -> Result<ContainerView, ServiceError> {
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
        let view = container_view(&path, &metadata);
        *session = Some(Session {
            path,
            veil_id: metadata.veil_id.clone(),
            password,
        });
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
    ) -> Result<ContainerView, ServiceError> {
        if sources.is_empty() {
            return Err(ServiceError::InvalidInput("请选择文件或文件夹"));
        }
        let directory = if directory.is_empty() {
            String::new()
        } else {
            crate::veil_ops::normalize_veil_dir(&directory)?
        };
        let session = self.0.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let session = session.as_ref().ok_or(ServiceError::NotOpen)?;
        let mut files = Vec::new();
        let mut targets = std::collections::HashSet::new();
        for source in sources {
            let path = Path::new(&source);
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or(ServiceError::InvalidInput("文件名称无效"))?;
            let destination = if path.is_dir() {
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
            for spec in crate::import::collect_add_specs(
                path,
                if destination.is_empty() {
                    None
                } else {
                    Some(&destination)
                },
            )? {
                if !targets.insert(spec.target.clone()) {
                    return Err(ServiceError::InvalidInput("选择的文件包含相同目标路径"));
                }
                files.push(spec.without_overwrite());
            }
        }
        if files.is_empty() {
            return Err(ServiceError::InvalidInput("选择的文件夹没有可添加的文件"));
        }
        for target in &targets {
            for (index, _) in target.match_indices('/') {
                if targets.contains(&target[..index]) {
                    return Err(ServiceError::InvalidInput("选择的文件包含冲突的目标路径"));
                }
            }
        }
        let manager = VeilManager::for_veil(session.path.clone(), session.veil_id.clone());
        manager.add_files(&files, session.password.as_str())?;
        Ok(container_view(
            &session.path,
            &manager.read_meta(session.password.as_str())?,
        ))
    }

    fn preview_file(&self, name: &str) -> Result<Vec<u8>, ServiceError> {
        if file_kind(name).1.is_none() {
            return Err(ServiceError::InvalidInput("暂不支持预览此文件类型"));
        }
        let session = self.0.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let session = session.as_ref().ok_or(ServiceError::NotOpen)?;
        let manager = VeilManager::for_veil(session.path.clone(), session.veil_id.clone());
        Ok(manager.read_file_bytes(name, session.password.as_str(), 16 * 1024 * 1024)?)
    }

    fn export_file(&self, name: &str, destination: &str) -> Result<(), ServiceError> {
        if destination.is_empty() {
            return Err(ServiceError::InvalidInput("请选择导出位置"));
        }
        let session = self.0.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let session = session.as_ref().ok_or(ServiceError::NotOpen)?;
        let manager = VeilManager::for_veil(session.path.clone(), session.veil_id.clone());
        manager.extract_file_without_overwrite(
            name,
            Path::new(destination),
            session.password.as_str(),
        )?;
        Ok(())
    }

    fn remove_path(&self, name: &str) -> Result<ContainerView, ServiceError> {
        let session = self.0.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let session = session.as_ref().ok_or(ServiceError::NotOpen)?;
        let manager = VeilManager::for_veil(session.path.clone(), session.veil_id.clone());
        manager.remove_path(name, session.password.as_str())?;
        Ok(container_view(
            &session.path,
            &manager.read_meta(session.password.as_str())?,
        ))
    }

    fn lock(&self) -> Result<(), ServiceError> {
        *self.0.lock().map_err(|_| ServiceError::StateUnavailable)? = None;
        Ok(())
    }
}

fn container_view(path: &Path, metadata: &MetaData) -> ContainerView {
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
