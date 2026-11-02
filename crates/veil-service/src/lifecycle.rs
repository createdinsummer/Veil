//! 容器创建的事务编排。

use crate::config::AppConfig;
use crate::veil_ops::VeilManager;
use std::path::Path;
use veil_core::error::VeilError;

/// 创建过程中的失败阶段，便于调用方提供对应提示。
#[derive(Debug, thiserror::Error)]
pub enum CreationError {
    #[error("容器目录已存在：{}", .0.display())]
    ContainerExists(std::path::PathBuf),
    #[error("链接已存在：{}", .0.display())]
    LinkExists(std::path::PathBuf),
    #[error("创建容器失败：{0}")]
    Initialize(#[source] VeilError),
    #[error("登记容器失败：{0}")]
    Register(#[source] VeilError),
    #[error("登记容器失败，数据保留在 {}：{source}", directory.display())]
    RegisterInitialized {
        directory: std::path::PathBuf,
        #[source]
        source: VeilError,
    },
    #[error("登记链接失败：{0}")]
    Link(#[source] VeilError),
}

/// 初始化容器并登记链接；只清理尚未初始化的自建目录，保留已经提交的容器。
pub(crate) fn create_registered_container(
    config: &mut AppConfig,
    veil_dir: &Path,
    veil_id: &str,
    veil_name: &str,
    password: &str,
    work_root: &Path,
    work_root_existed: bool,
    link_path: &Path,
) -> Result<(), CreationError> {
    crate::work::validate_veil_id(veil_id).map_err(CreationError::Register)?;
    if veil_dir != work_root.join(veil_id) || veil_name.trim().is_empty() || password.is_empty() {
        return Err(CreationError::Register(VeilError::WorkError(
            "容器参数无效".into(),
        )));
    }
    config
        .ensure_veil_id_available(veil_id)
        .map_err(CreationError::Register)?;
    if veil_dir.exists() {
        return Err(CreationError::ContainerExists(veil_dir.to_path_buf()));
    }
    if link_path.exists() {
        return Err(CreationError::LinkExists(link_path.to_path_buf()));
    }
    crate::fsutil::create_dir_all_durable(work_root)
        .map_err(|e| CreationError::Initialize(e.into()))?;
    std::fs::create_dir(veil_dir).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            CreationError::ContainerExists(veil_dir.into())
        } else {
            CreationError::Initialize(e.into())
        }
    })?;
    let mut initialized = false;
    let manager = VeilManager::for_veil(veil_dir.to_path_buf(), veil_id.to_owned());
    let result = (|| {
        let metadata = manager
            .init_veil_with_id(veil_id, veil_name, password)
            .map_err(CreationError::Initialize)?;
        initialized = true;
        #[cfg(feature = "fault-injection")]
        veil_core::fsutil::fault_point("after_container_init");
        config
            .register_directory(veil_dir, &metadata.veil_id, veil_name, &metadata.created_at)
            .map_err(|source| CreationError::RegisterInitialized {
                directory: veil_dir.into(),
                source,
            })?;
        config
            .register_link_at(&metadata.veil_id, veil_name, veil_dir, link_path)
            .map_err(CreationError::Link)?;
        Ok(())
    })();
    if result.is_err() && !initialized {
        let _ = std::fs::remove_dir_all(veil_dir);
        if !work_root_existed && work_root != veil_dir {
            let _ = std::fs::remove_dir(work_root);
        }
    }
    result
}
