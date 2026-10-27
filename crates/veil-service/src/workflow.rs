//! 容器创建与解包的准备、校验及执行。

use crate::config::GlobalConfig;
use crate::lifecycle::{self, CreationError};
use crate::work::WorkType;
use std::path::{Path, PathBuf};
use veil_core::error::VeilError;
use veil_core::metadata::{MetaHeader, generate_veil_id};
use veil_core::veil_package::VeilUnpacker;

/// 创建容器使用的原始参数。
pub struct CreateOptions<'a> {
    pub target: &'a str,
    pub link_output: Option<&'a str>,
    pub work_name: Option<&'a str>,
    pub work_root: Option<&'a Path>,
    pub dedicated: bool,
    pub portable: bool,
}

/// 解包使用的原始参数。
pub struct UnpackOptions<'a> {
    pub package_path: &'a str,
    pub name: Option<&'a str>,
    pub work_name: Option<&'a str>,
    pub link_output: Option<&'a str>,
}

/// 工作流错误，调用方可映射成各自的提示。
#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    #[error("不能同时指定工作区名称和路径")]
    ConflictingWorkOptions,
    #[error("专属工作区需要指定路径")]
    DedicatedRequiresPath,
    #[error("便携模式与工作区参数冲突")]
    PortableConflictsWork,
    #[error("链接名称无效")]
    InvalidLinkName,
    #[error("容器名称无效")]
    InvalidName,
    #[error("链接扩展名无效：{}", .0.display())]
    InvalidLinkExtension(PathBuf),
    #[error("链接已存在：{}", .0.display())]
    LinkExists(PathBuf),
    #[error("容器目录已存在：{}", .0.display())]
    ContainerExists(PathBuf),
    #[error("专属工作区非空：{}", .0.display())]
    DedicatedNotEmpty(PathBuf),
    #[error("打包文件不存在：{}", .0.display())]
    PackageNotFound(PathBuf),
    #[error("无法确定容器名称")]
    UnpackNameInvalid,
    #[error("打包文件缺少容器 ID")]
    UnpackMissingId,
    #[error("解包目标已存在或专属工作区非空：{}", .0.display())]
    UnpackDirectoryExists(PathBuf),
    #[error("解包链接扩展名无效：{}", .0.display())]
    UnpackInvalidLinkExtension(PathBuf),
    #[error("链接缺少容器 ID")]
    LinkMissingId,
    #[error("链接扩展名无效：{}", .0.display())]
    LinkInvalidExtension(PathBuf),
    #[error("打包文件已存在：{}", .0.display())]
    PackOutputExists(PathBuf),
    #[error("密码不能为空")]
    PasswordEmpty,
    #[error("提示级别无效")]
    InvalidHintLevel,
    #[error("{0}")]
    Core(#[from] VeilError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("创建容器失败 {}：{source}", directory.display())]
    CreateFailed {
        directory: PathBuf,
        #[source]
        source: VeilError,
    },
    #[error("登记链接失败 {}：{source}", path.display())]
    LinkRegistrationFailed {
        path: PathBuf,
        #[source]
        source: VeilError,
    },
}

/// 经服务层准备的创建计划，字段不能由调用方修改。
pub struct CreatePlan {
    config: GlobalConfig,
    veil_id: String,
    name: String,
    link_path: PathBuf,
    work_root: PathBuf,
    directory: PathBuf,
    dedicated: bool,
    portable_mode: bool,
}
impl CreatePlan {
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    pub fn portable_mode(&self) -> bool {
        self.portable_mode
    }
}

/// 经服务层准备的解包计划，字段不能由调用方修改。
pub struct UnpackPlan {
    config: GlobalConfig,
    package_path: PathBuf,
    veil_id: String,
    name: String,
    link_path: PathBuf,
    work_root: PathBuf,
    directory: PathBuf,
    dedicated: bool,
}

/// 创建结果，供调用方展示。
pub struct CreatedContainer {
    pub name: String,
    pub link_path: PathBuf,
    pub directory: PathBuf,
}
/// 解包结果，供调用方展示。
pub struct UnpackedContainer {
    pub name: String,
    pub link_path: PathBuf,
    pub directory: PathBuf,
    pub file_count: usize,
}

fn default_link_path(name: &str) -> PathBuf {
    let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let default = dir.join(format!("{name}.veil-link"));
    if !default.exists() {
        return default;
    }
    let timestamp = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
    let base = crate::work::veil_name_with_suffix(name, &timestamp);
    for index in 1u32.. {
        let file = if index == 1 {
            format!("{base}.veil-link")
        } else {
            format!("{base}-{index}.veil-link")
        };
        let candidate = dir.join(file);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("默认链接名称序号不可能耗尽")
}

fn directory_is_empty(path: &Path) -> std::io::Result<bool> {
    if !path.is_dir() {
        return Ok(false);
    }
    Ok(std::fs::read_dir(path)?.next().transpose()?.is_none())
}

fn check_create_paths(plan: &CreatePlan) -> Result<(), WorkflowError> {
    if plan.link_path.extension().and_then(|v| v.to_str()) != Some("veil-link") {
        return Err(WorkflowError::InvalidLinkExtension(plan.link_path.clone()));
    }
    if plan.link_path.exists() {
        return Err(WorkflowError::LinkExists(plan.link_path.clone()));
    }
    if plan.dedicated && plan.work_root.exists() && !directory_is_empty(&plan.work_root)? {
        return Err(WorkflowError::DedicatedNotEmpty(plan.work_root.clone()));
    }
    if plan.directory.exists() {
        return Err(WorkflowError::ContainerExists(plan.directory.clone()));
    }
    plan.config.ensure_veil_id_available(&plan.veil_id)?;
    Ok(())
}

/// 检查原始参数并准备创建，不写入容器或配置。
pub(crate) fn prepare_create(options: CreateOptions<'_>) -> Result<CreatePlan, WorkflowError> {
    if options.work_name.is_some() && options.work_root.is_some() {
        return Err(WorkflowError::ConflictingWorkOptions);
    }
    if options.dedicated && options.work_root.is_none() {
        return Err(WorkflowError::DedicatedRequiresPath);
    }
    if options.portable && (options.work_name.is_some() || options.work_root.is_some()) {
        return Err(WorkflowError::PortableConflictsWork);
    }
    let target = Path::new(options.target);
    let name = if target.extension().and_then(|v| v.to_str()) == Some("veil-link") {
        target
            .file_stem()
            .and_then(|v| v.to_str())
            .ok_or(WorkflowError::InvalidLinkName)?
            .to_owned()
    } else if target.parent().is_some_and(|p| !p.as_os_str().is_empty()) {
        target
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or(WorkflowError::InvalidName)?
            .to_owned()
    } else {
        options.target.to_owned()
    };
    if name.trim().is_empty() {
        return Err(WorkflowError::InvalidName);
    }
    let link_path = if let Some(link) = options.link_output {
        PathBuf::from(link)
    } else if target.extension().and_then(|v| v.to_str()) == Some("veil-link") {
        target.to_path_buf()
    } else {
        default_link_path(&name)
    };
    if link_path.extension().and_then(|v| v.to_str()) != Some("veil-link") {
        return Err(WorkflowError::InvalidLinkExtension(link_path));
    }
    if link_path.exists() {
        return Err(WorkflowError::LinkExists(link_path));
    }
    let absolute = if link_path.is_absolute() {
        link_path.clone()
    } else {
        std::env::current_dir()?.join(&link_path)
    };
    let link_dir = absolute.parent().unwrap_or_else(|| Path::new("."));
    let (config, work) = GlobalConfig::creation_work(
        link_dir,
        options.work_name,
        options.work_root,
        options.dedicated,
    )?;
    let work_root = config.resolve_work_root(&work)?;
    let portable_mode =
        options.portable || veil_core::volume::volume_for_path(link_dir)?.if_out_volume;
    let veil_id = generate_veil_id();
    let directory = work_root.join(&veil_id);
    let plan = CreatePlan {
        config,
        veil_id,
        name,
        link_path,
        work_root,
        directory,
        dedicated: matches!(work.work_type, WorkType::Dedicated),
        portable_mode,
    };
    check_create_paths(&plan)?;
    Ok(plan)
}

/// 创建前重新校验路径，随后创建并登记容器。
pub(crate) fn commit_create(
    mut plan: CreatePlan,
    password: &str,
) -> Result<CreatedContainer, WorkflowError> {
    if password.is_empty() {
        return Err(WorkflowError::PasswordEmpty);
    }
    check_create_paths(&plan)?;
    let existed = plan.work_root.exists();
    lifecycle::create_registered_container(
        &mut plan.config,
        &plan.directory,
        &plan.veil_id,
        &plan.name,
        password,
        &plan.work_root,
        existed,
        &plan.link_path,
    )
    .map_err(|error| match error {
        CreationError::ContainerExists(path) => WorkflowError::ContainerExists(path),
        CreationError::LinkExists(path) => WorkflowError::LinkExists(path),
        CreationError::Initialize(source) => WorkflowError::CreateFailed {
            directory: plan.directory.clone(),
            source,
        },
        CreationError::Link(source) => WorkflowError::LinkRegistrationFailed {
            path: plan.link_path.clone(),
            source,
        },
        CreationError::Register(source) => WorkflowError::Core(source),
    })?;
    Ok(CreatedContainer {
        name: plan.name,
        link_path: plan.link_path,
        directory: plan.directory,
    })
}

fn check_unpack_paths(plan: &UnpackPlan) -> Result<(), WorkflowError> {
    if !plan.package_path.exists() {
        return Err(WorkflowError::PackageNotFound(plan.package_path.clone()));
    }
    if plan.dedicated && plan.work_root.exists() && !directory_is_empty(&plan.work_root)? {
        return Err(WorkflowError::UnpackDirectoryExists(plan.work_root.clone()));
    }
    if plan.link_path.extension().and_then(|v| v.to_str()) != Some("veil-link") {
        return Err(WorkflowError::UnpackInvalidLinkExtension(
            plan.link_path.clone(),
        ));
    }
    if plan.link_path.exists() {
        return Err(WorkflowError::LinkExists(plan.link_path.clone()));
    }
    if plan.directory.exists() {
        return Err(WorkflowError::UnpackDirectoryExists(plan.directory.clone()));
    }
    plan.config.ensure_veil_id_available(&plan.veil_id)?;
    Ok(())
}

/// 检查包路径和工作区，准备解包，不写入容器或配置。
pub(crate) fn prepare_unpack(options: UnpackOptions<'_>) -> Result<UnpackPlan, WorkflowError> {
    let package_path = PathBuf::from(options.package_path);
    if !package_path.exists() {
        return Err(WorkflowError::PackageNotFound(package_path));
    }
    let name = if let Some(name) = options.name {
        name.to_owned()
    } else {
        let stem = package_path
            .file_stem()
            .and_then(|v| v.to_str())
            .ok_or(WorkflowError::UnpackNameInvalid)?;
        stem.strip_suffix(".vault").unwrap_or(stem).to_owned()
    };
    if name.trim().is_empty() {
        return Err(WorkflowError::UnpackNameInvalid);
    }
    let location = options
        .link_output
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let (config, work) = GlobalConfig::creation_work(&location, options.work_name, None, false)?;
    let work_root = config.resolve_work_root(&work)?;
    let dedicated = matches!(work.work_type, WorkType::Dedicated);
    if dedicated && work_root.exists() && !directory_is_empty(&work_root)? {
        return Err(WorkflowError::UnpackDirectoryExists(work_root));
    }
    let encrypted = VeilUnpacker::new(&package_path).read_encrypted_metadata()?;
    let header = MetaHeader::from_bytes(&encrypted)?;
    if header.veil_id.is_empty() {
        return Err(WorkflowError::UnpackMissingId);
    }
    config.ensure_veil_id_available(&header.veil_id)?;
    let link_path = options
        .link_output
        .map(PathBuf::from)
        .unwrap_or_else(|| default_link_path(&name));
    crate::work::validate_veil_id(&header.veil_id)?;
    let directory = work_root.join(&header.veil_id);
    let plan = UnpackPlan {
        config,
        package_path,
        veil_id: header.veil_id,
        name,
        link_path,
        work_root,
        directory,
        dedicated,
    };
    check_unpack_paths(&plan)?;
    Ok(plan)
}

/// 解包前重新校验路径，再验证密码并执行还原。
pub(crate) fn commit_unpack(
    mut plan: UnpackPlan,
    password: &str,
) -> Result<UnpackedContainer, WorkflowError> {
    check_unpack_paths(&plan)?;
    let count = crate::package::unpack_registered(
        &plan.package_path,
        &plan.directory,
        &plan.veil_id,
        &plan.name,
        password,
        &mut plan.config,
        &plan.work_root,
        &plan.link_path,
    )?;
    Ok(UnpackedContainer {
        name: plan.name,
        link_path: plan.link_path,
        directory: plan.directory,
        file_count: count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ServiceError, ServiceRequest, ServiceResponse, VeilService};

    #[test]
    fn create_requests_validate_and_recheck_dedicated_directory() {
        let temporary = tempfile::TempDir::new().unwrap();
        let work = temporary.path().join("dedicated");
        std::fs::create_dir(&work).unwrap();
        let link = temporary.path().join("test.veil-link");
        let link_text = link.to_str().unwrap();
        let service = VeilService::default();
        let options = || CreateOptions {
            target: "test",
            link_output: Some(link_text),
            work_name: None,
            work_root: Some(&work),
            dedicated: true,
            portable: false,
        };
        let plan = match service
            .execute(ServiceRequest::PrepareCreate { options: options() })
            .unwrap()
        {
            ServiceResponse::CreatePrepared(plan) => plan,
            _ => panic!("预期返回创建计划"),
        };
        std::fs::write(work.join("existing"), b"keep").unwrap();
        match service.execute(ServiceRequest::CommitCreate {
            plan,
            password: "password",
        }) {
            Err(ServiceError::Workflow(WorkflowError::DedicatedNotEmpty(path))) => {
                assert_eq!(path.canonicalize().unwrap(), work.canonicalize().unwrap())
            }
            Err(error) => panic!("错误类型不符：{error:?}"),
            Ok(_) => panic!("非空专属工作区不应允许创建"),
        }
        assert_eq!(std::fs::read(work.join("existing")).unwrap(), b"keep");
        assert!(!link.exists());
        assert!(matches!(
            service.execute(ServiceRequest::PrepareCreate { options: options() }),
            Err(ServiceError::Workflow(WorkflowError::DedicatedNotEmpty(_)))
        ));
        std::fs::remove_file(work.join("existing")).unwrap();
        let plan = match service
            .execute(ServiceRequest::PrepareCreate { options: options() })
            .unwrap()
        {
            ServiceResponse::CreatePrepared(plan) => plan,
            _ => panic!("预期返回创建计划"),
        };
        std::fs::write(&link, b"keep link").unwrap();
        assert!(
            matches!(service.execute(ServiceRequest::CommitCreate { plan, password: "password" }), Err(ServiceError::Workflow(WorkflowError::LinkExists(path))) if path == link)
        );
        assert_eq!(std::fs::read(&link).unwrap(), b"keep link");
    }

    #[test]
    fn unpack_request_rechecks_dedicated_directory_before_writing() {
        let temporary = tempfile::TempDir::new().unwrap();
        let work = temporary.path().join("dedicated");
        std::fs::create_dir(&work).unwrap();
        let package = temporary.path().join("package.veil");
        std::fs::write(&package, b"unchanged").unwrap();
        let link = temporary.path().join("test.veil-link");
        let id = generate_veil_id();
        let plan = UnpackPlan {
            config: GlobalConfig::load_from(&temporary.path().join("config.jsonc")).unwrap(),
            package_path: package,
            directory: work.join(&id),
            veil_id: id,
            name: "test".into(),
            link_path: link.clone(),
            work_root: work.clone(),
            dedicated: true,
        };
        std::fs::write(work.join("existing"), b"keep").unwrap();
        let result = VeilService::default().execute(ServiceRequest::CommitUnpack {
            plan,
            password: "password",
        });
        assert!(
            matches!(result, Err(ServiceError::Workflow(WorkflowError::UnpackDirectoryExists(path))) if path == work)
        );
        assert_eq!(std::fs::read(work.join("existing")).unwrap(), b"keep");
        assert!(!link.exists());
    }

    #[test]
    fn service_rejects_invalid_inputs_without_cli() {
        let service = VeilService::default();
        assert!(matches!(
            service.execute(ServiceRequest::PrepareCreate {
                options: CreateOptions {
                    target: "test",
                    link_output: None,
                    work_name: Some("default"),
                    work_root: Some(Path::new("work")),
                    dedicated: false,
                    portable: false,
                }
            }),
            Err(ServiceError::Workflow(
                WorkflowError::ConflictingWorkOptions
            ))
        ));
        assert!(matches!(
            service.execute(ServiceRequest::PrepareCreate {
                options: CreateOptions {
                    target: "test",
                    link_output: Some("invalid"),
                    work_name: None,
                    work_root: None,
                    dedicated: false,
                    portable: false,
                }
            }),
            Err(ServiceError::Workflow(WorkflowError::InvalidLinkExtension(
                _
            )))
        ));
        assert!(matches!(
            service.execute(ServiceRequest::ValidateNewPassword { password: "" }),
            Err(ServiceError::Workflow(WorkflowError::PasswordEmpty))
        ));
        let temporary = tempfile::TempDir::new().unwrap();
        let package = temporary.path().join("existing.veil");
        std::fs::write(&package, b"keep").unwrap();
        assert!(
            matches!(service.execute(ServiceRequest::PreparePack { name: "test", output: package.to_str() }), Err(ServiceError::Workflow(WorkflowError::PackOutputExists(path))) if path == package)
        );
        assert_eq!(std::fs::read(package).unwrap(), b"keep");
    }
}
