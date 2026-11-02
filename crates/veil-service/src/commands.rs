//! 为各调用方组合容器定位与文件操作。

use crate::config::{AppConfig, ResolvedVeil};
use crate::import::collect_add_specs;
use crate::operations::{ContainerOperation, ContainerOperationResult};
use crate::veil_ops::{AddFileSpec, VeilManager};
use crate::workflow::WorkflowError;
use std::path::{Path, PathBuf};
use veil_core::error::VeilError;

/// 已定位且绑定容器身份的操作目标。
#[derive(Clone)]
pub struct ContainerHandle {
    resolved: ResolvedVeil,
}
impl ContainerHandle {
    pub fn name(&self) -> &str {
        &self.resolved.veil_name
    }
    pub fn directory(&self) -> &Path {
        &self.resolved.veil_dir
    }
    pub fn recovered_link(&self) -> Option<&Path> {
        if self.resolved.recovered_link {
            self.resolved.link_path.as_deref()
        } else {
            None
        }
    }
    fn manager(&self) -> VeilManager {
        VeilManager::for_veil(self.resolved.veil_dir.clone(), &self.resolved.veil_id)
    }
}

pub(crate) fn prepare_container(selector: &str) -> Result<ContainerHandle, VeilError> {
    let mut config = AppConfig::load()?;
    Ok(ContainerHandle {
        resolved: config.resolve_veil(selector)?,
    })
}

pub(crate) fn operate(
    container: &ContainerHandle,
    operation: ContainerOperation<'_>,
) -> Result<ContainerOperationResult, VeilError> {
    crate::operations::execute(&container.manager(), operation)
}

/// 服务层准备的文件导入计划。
pub struct ImportPlan {
    container: ContainerHandle,
    files: Vec<AddFileSpec>,
}
impl ImportPlan {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
    pub fn recovered_link(&self) -> Option<&Path> {
        self.container.recovered_link()
    }
}
/// 导入结果，包含展示所需的目标与密文名称。
pub struct ImportResult {
    pub targets: Vec<String>,
    pub encrypted_names: Vec<String>,
}

pub(crate) fn prepare_import(
    selector: &str,
    source: &Path,
    destination: Option<&str>,
) -> Result<ImportPlan, crate::session::ServiceError> {
    let container = prepare_container(selector)?;
    let files =
        collect_add_specs(source, destination).map_err(crate::session::ServiceError::Import)?;
    Ok(ImportPlan { container, files })
}
pub(crate) fn commit_import(plan: ImportPlan, password: &str) -> Result<ImportResult, VeilError> {
    let targets = plan.files.iter().map(|file| file.target.clone()).collect();
    let encrypted_names = plan.container.manager().add_files(&plan.files, password)?;
    Ok(ImportResult {
        targets,
        encrypted_names,
    })
}

/// 服务层准备的打包计划。
pub struct PackPlan {
    container: ContainerHandle,
    output: PathBuf,
}
impl PackPlan {
    pub fn recovered_link(&self) -> Option<&Path> {
        self.container.recovered_link()
    }
}
pub struct PackResult {
    pub name: String,
    pub link_path: Option<PathBuf>,
    pub output: PathBuf,
    pub file_count: usize,
    pub size: u64,
}

pub(crate) fn prepare_pack(
    selector: &str,
    output: Option<&str>,
) -> Result<PackPlan, crate::session::ServiceError> {
    let container = prepare_container(selector)?;
    let output = output
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{}.vault.veil", container.name())));
    if output.exists() {
        return Err(WorkflowError::PackOutputExists(output).into());
    }
    Ok(PackPlan { container, output })
}
pub(crate) fn commit_pack(
    plan: PackPlan,
    password: &str,
) -> Result<PackResult, crate::session::ServiceError> {
    if plan.output.exists() {
        return Err(WorkflowError::PackOutputExists(plan.output).into());
    }
    let manager = plan.container.manager();
    let pack = crate::package::VeilPacker::new(&plan.output)
        .without_overwrite()
        .pack_container(&manager, password);
    if let Err(VeilError::Io(ref error)) = pack
        && error.kind() == std::io::ErrorKind::AlreadyExists
    {
        return Err(WorkflowError::PackOutputExists(plan.output).into());
    }
    let count = pack?;
    let size = std::fs::metadata(&plan.output)
        .map_err(VeilError::Io)?
        .len();
    Ok(PackResult {
        name: plan.container.resolved.veil_name,
        link_path: plan.container.resolved.link_path,
        output: plan.output,
        file_count: count,
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ServiceError, ServiceRequest, ServiceResponse, VeilService};

    #[test]
    fn import_and_pack_requests_execute_and_preserve_existing_output() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let directory = root.path().join("container");
        let manager = VeilManager::new(directory.clone());
        let metadata = manager.init_veil("test", "password").unwrap();
        let handle = ContainerHandle {
            resolved: ResolvedVeil {
                veil_name: "test".into(),
                veil_id: metadata.veil_id.clone(),
                veil_dir: directory,
                link_path: None,
                recovered_link: false,
            },
        };
        let source = root.path().join("file.txt");
        std::fs::write(&source, b"hello").unwrap();
        let plan = ImportPlan {
            container: handle.clone(),
            files: vec![AddFileSpec::new(&source, "file.txt")],
        };
        let service = VeilService::default();
        let result = service
            .execute(ServiceRequest::CommitImport {
                plan,
                password: "password",
            })
            .unwrap();
        match result {
            ServiceResponse::Imported(result) => {
                assert_eq!(result.targets, ["file.txt"]);
                assert_eq!(result.encrypted_names.len(), 1);
            }
            _ => panic!("预期返回导入结果"),
        }
        let output = root.path().join("output.veil");
        std::fs::write(&output, b"keep").unwrap();
        let plan = PackPlan {
            container: handle.clone(),
            output: output.clone(),
        };
        assert!(matches!(
            service.execute(ServiceRequest::CommitContainerPack {
                plan,
                password: "password"
            }),
            Err(ServiceError::Workflow(WorkflowError::PackOutputExists(_)))
        ));
        assert_eq!(std::fs::read(&output).unwrap(), b"keep");
        std::fs::remove_file(&output).unwrap();
        let plan = PackPlan {
            container: handle,
            output: output.clone(),
        };
        match service
            .execute(ServiceRequest::CommitContainerPack {
                plan,
                password: "password",
            })
            .unwrap()
        {
            ServiceResponse::ContainerPacked(result) => {
                assert_eq!(result.file_count, 1);
                assert!(result.size > 0);
            }
            _ => panic!("预期返回打包结果"),
        }
        assert!(output.exists());
    }
}
