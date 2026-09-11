//! 磁盘卷识别与稳定标识。
//!
//! `.veil-link` 根据卷 ID 判断目标工作区是否仍位于同一设备，因此这里需要把路径
//! 归一化为卷根，并生成跨进程稳定的卷描述。Unix 使用设备号，Windows 使用盘符；
//! `is_external` 只用于判断工作区是否可移动，不参与加密身份。

use crate::error::VeilError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 路径所在磁盘卷的可持久化描述。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VolumeInfo {
    /// 平台相关的稳定卷 ID。
    pub volume_id: String,
    /// 用于展示和恢复提示的卷名称。
    pub volume_label: String,
    /// 当前系统上的卷根路径。
    pub mount_path: PathBuf,
    /// 是否被识别为外部或可移动卷。
    pub is_external: bool,
}

/// 识别给定路径所在的磁盘卷。
///
/// 路径不存在时先向上寻找最近的已存在祖先，因此可在目标目录创建前完成卷探测。
///
/// # 错误
/// 路径无法解析、最近祖先不存在，或平台卷信息读取失败时返回错误。
pub fn volume_for_path(path: &Path) -> Result<VolumeInfo, VeilError> {
    let probe = nearest_existing_path(path)?;
    volume_for_existing_path(&probe)
}

/// 返回路径自身或其最近的已存在祖先。
///
/// 相对路径先基于当前工作目录展开；找到路径后会尝试 canonicalize，规范化失败时
/// 保留原路径。
///
/// # 错误
/// 当前目录不可访问，或向上遍历到文件系统根后仍不存在可用路径时返回错误。
pub fn nearest_existing_path(path: &Path) -> Result<PathBuf, VeilError> {
    // 相对路径先固定到当前工作目录，后续才能可靠地逐级 pop。
    let mut current = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };

    loop {
        // 第一个存在路径即可用于探测卷，创建目标目录前也可安全调用。
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
/// 在 Unix 上按设备号识别卷，并将挂载路径归约到同一设备的根目录。
fn volume_for_existing_path(path: &Path) -> Result<VolumeInfo, VeilError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(path)?;
    let device = metadata.dev();
    let mut mount_path = path.to_path_buf();

    // 同一设备内向上归约；父目录设备号变化处就是挂载点边界。
    while let Some(parent) = mount_path.parent() {
        let parent_device = std::fs::metadata(parent).ok().map(|meta| meta.dev());
        if parent_device != Some(device) {
            break;
        }
        mount_path = parent.to_path_buf();
    }

    // 根文件系统使用固定标签，其他挂载点使用目录名作为展示标签。
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
/// 判断 Unix 挂载路径是否属于本实现认定的外部卷位置。
fn is_external_mount(mount_path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        // macOS 外接卷通常挂载在 /Volumes 下。
        return mount_path.starts_with("/Volumes");
    }

    #[cfg(not(target_os = "macos"))]
    {
        // Linux 常见外部挂载由发行版放在 media、mnt 或 run/media。
        mount_path.starts_with("/media")
            || mount_path.starts_with("/mnt")
            || mount_path.starts_with("/run/media")
    }
}

#[cfg(windows)]
/// 在 Windows 上按盘符识别卷，并以用户主目录所在盘符判断是否为外部卷。
fn volume_for_existing_path(path: &Path) -> Result<VolumeInfo, VeilError> {
    use std::path::Component;

    /// 提取路径中的盘符并统一为大写。
    fn root_key(path: &Path) -> String {
        match path.components().next() {
            // 盘符比较统一大写，避免 C: 与 c: 被当成不同卷。
            Some(Component::Prefix(prefix)) => prefix.as_os_str().to_string_lossy().to_uppercase(),
            _ => "?".to_string(),
        }
    }

    // Windows 先用盘符归约卷根，再与用户主目录盘符比较判断是否外部卷。
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
