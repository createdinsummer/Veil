//! 磁盘卷识别与稳定描述。

use crate::error::VeilError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VolumeInfo {
    pub volume_id: String,
    pub volume_label: String,
    pub mount_path: PathBuf,
    pub is_external: bool,
}

pub fn volume_for_path(path: &Path) -> Result<VolumeInfo, VeilError> {
    let probe = nearest_existing_path(path)?;
    volume_for_existing_path(&probe)
}

pub fn nearest_existing_path(path: &Path) -> Result<PathBuf, VeilError> {
    let mut current = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };

    loop {
        if current.exists() {
            return Ok(std::fs::canonicalize(&current).unwrap_or(current));
        }
        if !current.pop() {
            return Err(VeilError::ConfigError(format!(
                "找不到可用路径: {}",
                path.display()
            )));
        }
    }
}

#[cfg(unix)]
fn volume_for_existing_path(path: &Path) -> Result<VolumeInfo, VeilError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(path)?;
    let device = metadata.dev();
    let mut mount_path = path.to_path_buf();

    while let Some(parent) = mount_path.parent() {
        let parent_device = std::fs::metadata(parent).ok().map(|meta| meta.dev());
        if parent_device != Some(device) {
            break;
        }
        mount_path = parent.to_path_buf();
    }

    let root = Path::new("/");
    let is_external = mount_path != root && is_external_mount(&mount_path);
    let volume_label = if mount_path == root {
        "local-system".to_string()
    } else {
        mount_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("external-volume")
            .to_string()
    };

    Ok(VolumeInfo {
        volume_id: format!("fs:{:x}", device),
        volume_label,
        mount_path,
        is_external,
    })
}

#[cfg(unix)]
fn is_external_mount(mount_path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        return mount_path.starts_with("/Volumes");
    }

    #[cfg(not(target_os = "macos"))]
    {
        mount_path.starts_with("/media")
            || mount_path.starts_with("/mnt")
            || mount_path.starts_with("/run/media")
    }
}

#[cfg(windows)]
fn volume_for_existing_path(path: &Path) -> Result<VolumeInfo, VeilError> {
    use std::path::Component;

    fn root_key(path: &Path) -> String {
        match path.components().next() {
            Some(Component::Prefix(prefix)) => prefix.as_os_str().to_string_lossy().to_uppercase(),
            _ => "?".to_string(),
        }
    }

    let mount_path = PathBuf::from(root_key(path));
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    let is_external = home
        .as_deref()
        .map(root_key)
        .map(|home_root| {
            !mount_path
                .to_string_lossy()
                .eq_ignore_ascii_case(&home_root)
        })
        .unwrap_or(true);
    let volume_label = mount_path.to_string_lossy().into_owned();

    Ok(VolumeInfo {
        volume_id: format!("drive:{}", volume_label.to_ascii_lowercase()),
        volume_label,
        mount_path,
        is_external,
    })
}
