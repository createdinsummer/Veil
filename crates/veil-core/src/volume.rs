//! 运行时枚举挂载卷、识别路径所属卷；卷信息不写入配置或链接。

use crate::error::VeilError;
use std::path::{Path, PathBuf};

/// 当前挂载卷的运行时描述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    pub volume_name: String,
    pub root_path: PathBuf,
    pub if_out_volume: bool,
}

/// 枚举实际挂载的外部卷，不扫描普通目录，也不创建任何配置。
pub fn mounted_volumes() -> Result<Vec<VolumeInfo>, VeilError> {
    Ok(mount_roots()?
        .into_iter()
        .filter(|root| is_external_mount(root))
        .map(|root_path| VolumeInfo {
            volume_name: root_path
                .file_name()
                .unwrap_or(root_path.as_os_str())
                .to_string_lossy()
                .into_owned(),
            root_path,
            if_out_volume: true,
        })
        .collect())
}

/// 选择最长匹配的挂载点，支持嵌套挂载和同设备的绑定挂载。
pub fn volume_for_path(path: &Path) -> Result<VolumeInfo, VeilError> {
    let probe = nearest_existing_path(path)?;
    let root_path = mount_roots()?
        .into_iter()
        .filter(|root| probe.starts_with(root))
        .max_by_key(|root| root.components().count())
        .ok_or_else(|| VeilError::ConfigError(format!("无法识别路径所属卷: {}", path.display())))?;
    Ok(VolumeInfo {
        volume_name: root_path
            .file_name()
            .unwrap_or(root_path.as_os_str())
            .to_string_lossy()
            .into_owned(),
        if_out_volume: is_external_mount(&root_path),
        root_path,
    })
}

/// 路径尚未创建时，用最近的已存在祖先识别所在卷。
pub fn nearest_existing_path(path: &Path) -> Result<PathBuf, VeilError> {
    let mut current = std::path::absolute(path)?;
    loop {
        match std::fs::canonicalize(&current) {
            Ok(path) => {
                #[cfg(windows)]
                if let Some(std::path::Component::Prefix(prefix)) = path.components().next()
                    && let std::path::Prefix::VerbatimDisk(letter) = prefix.kind()
                {
                    let mut normalized =
                        PathBuf::from(format!("{}:\\", (letter as char).to_ascii_uppercase()));
                    for part in path.components().skip(2) {
                        normalized.push(part.as_os_str());
                    }
                    return Ok(normalized);
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if !current.pop() {
            return Err(VeilError::ConfigError(format!(
                "找不到可用路径: {}",
                path.display()
            )));
        }
    }
}

// iOS 只使用应用私有目录，不枚举系统或外接卷。
#[cfg(target_os = "ios")]
fn mount_roots() -> Result<Vec<PathBuf>, VeilError> {
    Ok(vec![PathBuf::from("/")])
}

#[cfg(target_os = "macos")]
fn mount_roots() -> Result<Vec<PathBuf>, VeilError> {
    use std::os::unix::ffi::OsStrExt;
    // getfsstat 写入调用方拥有的缓冲区，避免 getmntinfo 的共享静态缓冲区竞争。
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if count < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut mounts = Vec::<libc::statfs>::with_capacity(count as usize + 16);
    let count = unsafe {
        libc::getfsstat(
            mounts.as_mut_ptr(),
            (mounts.capacity() * std::mem::size_of::<libc::statfs>()) as _,
            libc::MNT_NOWAIT,
        )
    };
    if count < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // 系统最多写入缓冲区容量个完整结构体。
    unsafe {
        mounts.set_len((count as usize).min(mounts.capacity()));
    }
    Ok(mounts
        .iter()
        .map(|mount| {
            let name = unsafe { std::ffi::CStr::from_ptr(mount.f_mntonname.as_ptr()) };
            PathBuf::from(std::ffi::OsStr::from_bytes(name.to_bytes()))
        })
        .collect())
}

#[cfg(target_os = "linux")]
fn mount_roots() -> Result<Vec<PathBuf>, VeilError> {
    use std::os::unix::ffi::OsStringExt;
    // mountinfo 的第 5 列是挂载点；空格等字节使用八进制转义。
    let content = std::fs::read_to_string("/proc/self/mountinfo")?;
    Ok(content
        .lines()
        .filter_map(|line| line.split_whitespace().nth(4))
        .map(|field| {
            let mut bytes = Vec::new();
            let raw = field.as_bytes();
            let mut i = 0;
            while i < raw.len() {
                if raw[i] == b'\\'
                    && i + 3 < raw.len()
                    && raw[i + 1..i + 4].iter().all(|b| (b'0'..=b'7').contains(b))
                {
                    bytes.push(
                        (raw[i + 1] - b'0') * 64 + (raw[i + 2] - b'0') * 8 + raw[i + 3] - b'0',
                    );
                    i += 4;
                } else {
                    bytes.push(raw[i]);
                    i += 1;
                }
            }
            PathBuf::from(std::ffi::OsString::from_vec(bytes))
        })
        .collect())
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLogicalDrives() -> u32;
    fn GetDriveTypeW(root: *const u16) -> u32;
}

#[cfg(windows)]
fn mount_roots() -> Result<Vec<PathBuf>, VeilError> {
    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok((0..26)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| PathBuf::from(format!("{}:\\", (b'A' + i) as char)))
        .collect())
}

fn is_external_mount(root: &Path) -> bool {
    #[cfg(target_os = "ios")]
    {
        let _ = root;
        false
    }
    #[cfg(target_os = "macos")]
    {
        root.starts_with("/Volumes")
    }
    #[cfg(target_os = "linux")]
    {
        root.starts_with("/media") || root.starts_with("/mnt") || root.starts_with("/run/media")
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<_> = root.as_os_str().encode_wide().chain(Some(0)).collect();
        let kind = unsafe { GetDriveTypeW(wide.as_ptr()) };
        let home_is_here = dirs::home_dir()
            .and_then(|home| nearest_existing_path(&home).ok())
            .is_some_and(|home| home.starts_with(root));
        matches!(kind, 2 | 3) && !home_is_here
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_existing_and_future_paths_without_creating_them() {
        let temp = tempfile::TempDir::new().unwrap();
        let future = temp.path().join("future/work");
        assert_eq!(
            volume_for_path(temp.path()).unwrap(),
            volume_for_path(&future).unwrap()
        );
        assert!(!future.exists());
        for volume in mounted_volumes().unwrap() {
            assert!(volume.root_path.is_dir());
        }
    }
}
