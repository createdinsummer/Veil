//! 供桌面端使用的空间与容器登记流程。
use crate::config::GlobalConfig;
use crate::veil_ops::VeilManager;
use crate::work::WorkType;
use crate::workflow::{self, CreateOptions, CreatedContainer, WorkflowError};
use serde::Serialize;
use std::path::{Path, PathBuf};
use veil_core::error::VeilError;
use veil_core::metadata::generate_veil_id;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceView {
    pub id: String,
    pub name: String,
    pub path: String,
    pub containers: Vec<ContainerSummary>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerSummary {
    pub id: String,
    pub name: String,
    pub path: String,
}

/// 读取现有工作区，并通过容器明文头发现目录中的容器。
pub(crate) fn list_spaces() -> Result<Vec<SpaceView>, VeilError> {
    let mut config = GlobalConfig::load()?;
    config.ensure_default_work()?;
    let mut spaces = Vec::new();
    let works = config
        .work
        .default
        .iter()
        .map(|work| ("default", "默认空间", work))
        .chain(config.work.custom.iter().map(|(key, work)| {
            (
                key.as_str(),
                match &work.work_type {
                    WorkType::Custom(name) => name.as_str(),
                    _ => key.as_str(),
                },
                work,
            )
        }));
    for (id, name, work) in works {
        let root = config.resolve_work_root(work)?;
        let root = if root.exists() {
            std::fs::canonicalize(root)?
        } else {
            root
        };
        let mut containers = Vec::new();
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => Some(entries),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if let Some(entries) = entries {
            for entry in entries {
                let entry = entry?;
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                let directory = std::fs::canonicalize(entry.path())?;
                let header = match VeilManager::new(directory.clone()).read_meta_header() {
                    Ok(header) => header,
                    Err(_) => continue,
                };
                if entry.file_name() != std::ffi::OsStr::new(&header.veil_id) {
                    continue;
                }
                containers.push(ContainerSummary {
                    id: header.veil_id,
                    name: header.veil_name,
                    path: directory.to_string_lossy().into_owned(),
                });
            }
        }
        containers.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        spaces.push(SpaceView {
            id: id.to_owned(),
            name: name.to_owned(),
            path: root.to_string_lossy().into_owned(),
            containers,
        });
    }
    if spaces.len() > 1 {
        spaces[1..].sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    }
    Ok(spaces)
}

/// 创建命名空间以及对应的本机文件夹。
pub(crate) fn create_space(name: &str) -> Result<(), VeilError> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > 80
        || name.chars().any(char::is_control)
        || name == "default"
        || name == "默认空间"
    {
        return Err(VeilError::WorkError("空间名称无效".into()));
    }
    let mut config = GlobalConfig::load()?;
    if config.work.custom.contains_key(name) {
        return Err(VeilError::WorkError("空间名称已存在".into()));
    }
    let relative = PathBuf::from("works").join(generate_veil_id());
    config.register_custom_work(name.to_owned(), relative, None)?;
    let root = config.resolve_work_root(
        config
            .work
            .custom
            .get(name)
            .ok_or_else(|| VeilError::WorkError("空间登记失败".into()))?,
    )?;
    let parent = root
        .parent()
        .ok_or_else(|| VeilError::WorkError("空间位置无效".into()))?;
    crate::fsutil::create_dir_all_durable(parent)?;
    std::fs::create_dir(&root)?;
    if let Err(error) = config.save() {
        let _ = std::fs::remove_dir(&root);
        return Err(error);
    }
    Ok(())
}

/// 使用已有工作区和标准创建流程注册容器与链接。
pub(crate) fn create_container(
    space: &str,
    name: &str,
    password: &str,
) -> Result<CreatedContainer, WorkflowError> {
    if name.trim().is_empty()
        || name.chars().count() > 100
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
    {
        return Err(WorkflowError::InvalidName);
    }
    let link = GlobalConfig::config_path()?
        .parent()
        .ok_or_else(|| VeilError::ConfigError("配置目录无效".into()))?
        .join("links")
        .join(format!("{}.veil-link", generate_veil_id()));
    let link = link
        .to_str()
        .ok_or_else(|| VeilError::WorkError("链接路径不是有效的 UTF-8".into()))?;
    let work_name = if space == "default" {
        None
    } else {
        Some(space)
    };
    let plan = workflow::prepare_create(CreateOptions {
        target: name,
        link_output: Some(link),
        work_name,
        work_root: None::<&Path>,
        dedicated: false,
        portable: false,
    })?;
    workflow::commit_create(plan, password)
}
