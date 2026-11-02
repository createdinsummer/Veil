//! 文件系统持久化辅助函数。
//!
//! 本模块只处理崩溃一致性所需的底层动作：创建目录后同步父目录、先写临时文件再
//! 原子替换目标文件，以及同步目录项。加密格式和业务事务仍由各模块负责。

use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// 在 macOS 上请求设备刷新；其他平台使用系统文件同步。
pub(crate) fn sync_file(file: &fs::File) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        loop {
            // SAFETY: 文件描述符来自仍然存活的 File，F_FULLFSYNC 不读取额外参数。
            if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    file.sync_all()
}

/// 区分替换前失败和替换后持久化状态不确定。
pub struct AtomicWriteError {
    pub error: io::Error,
    pub published: bool,
}

#[cfg(any(test, feature = "fault-injection"))]
#[doc(hidden)]
pub fn fault_point(stage: &str) {
    if std::env::var("VEIL_TEST_FAILPOINT").as_deref() == Ok(stage) {
        std::process::exit(86);
    }
    if std::env::var("VEIL_TEST_PAUSEPOINT").as_deref() == Ok(stage) {
        let root = std::path::PathBuf::from(std::env::var_os("VEIL_FAULT_TEST_ROOT").unwrap());
        fs::write(root.join("paused"), stage).unwrap();
        let start = std::time::Instant::now();
        while !root.join("resume").exists() {
            assert!(start.elapsed() < std::time::Duration::from_secs(20));
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

/// 返回路径的父目录；没有父目录时使用当前目录。
fn parent_or_current(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// 同步目录项，确保重命名、创建和删除在该目录中持久化。
///
/// Unix 平台直接对目录执行 `fsync`。Windows 标准库没有稳定暴露目录句柄同步接口，
/// 因此该平台保留原子重命名语义，并在本函数中不额外执行目录同步。
pub fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let directory = fs::File::open(path)?;
        directory.sync_all()?;
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}

/// 同步目标路径所在目录。
pub(crate) fn sync_parent(path: &Path) -> io::Result<()> {
    sync_directory(parent_or_current(path))
}

/// 创建目录树，并在创建每一层后同步其父目录。
///
/// 普通 `create_dir_all` 只保证进程内可见；断电或系统崩溃后，新目录项可能丢失。
/// 本函数从最上层缺失目录开始创建，使最终返回时整条目录链已经持久化。
pub fn create_dir_all_durable(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() || path.is_dir() {
        return Ok(());
    }

    let mut missing = Vec::new();
    let mut current = path.to_path_buf();
    while !current.as_os_str().is_empty() && !current.exists() {
        missing.push(current.clone());
        current.pop();
    }

    for directory in missing.iter().rev() {
        match fs::create_dir(directory) {
            Ok(()) => sync_parent(directory)?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }

    if path.is_dir() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("路径不是目录: {}", path.display()),
        ))
    }
}

/// 先完整写入同目录临时文件，再原子替换目标路径并同步目录。
///
/// 调用返回成功后，目标路径要么仍是旧内容，要么是完整的新内容；不会留下目标文件
/// 的半截内容。进程在写入过程中被终止时，残留临时文件只使用 `.veil-tmp-` 前缀，
/// 不会遮蔽正式文件。
///
/// 重命名完成后目录同步失败无法再安全回滚：此时正式文件已经是完整的新内容，若
/// 返回错误，调用方可能错误删除新数据。因此提交后的目录同步按尽力而为处理。
pub(crate) fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    atomic_write_inner(path, data, false).map_err(|error| error.error)
}

pub fn atomic_write_metadata(path: &Path, data: &[u8]) -> Result<(), AtomicWriteError> {
    atomic_write_inner(path, data, true)
}

fn atomic_write_inner(path: &Path, data: &[u8], strict: bool) -> Result<(), AtomicWriteError> {
    let before = |error| AtomicWriteError {
        error,
        published: false,
    };
    let parent = parent_or_current(path);
    create_dir_all_durable(parent).map_err(before)?;

    let mut temp_file = tempfile::Builder::new()
        .prefix(".veil-tmp-")
        .tempfile_in(parent)
        .map_err(before)?;
    temp_file.write_all(data).map_err(before)?;
    #[cfg(any(test, feature = "fault-injection"))]
    if strict {
        fault_point("config_temp_written");
    }
    sync_file(temp_file.as_file()).map_err(before)?;
    #[cfg(any(test, feature = "fault-injection"))]
    if strict {
        fault_point("before_replace");
    }
    temp_file
        .persist(path)
        .map_err(|error| before(error.error))?;
    #[cfg(any(test, feature = "fault-injection"))]
    if strict {
        fault_point("after_replace");
    }
    #[cfg(any(test, feature = "fault-injection"))]
    if strict && std::env::var("VEIL_TEST_FAILPOINT").as_deref() == Ok("after_replace_error") {
        return Err(AtomicWriteError {
            error: io::Error::other("注入目录同步失败"),
            published: true,
        });
    }
    let synced = sync_directory(parent);
    if strict {
        synced.map_err(|error| AtomicWriteError {
            error,
            published: true,
        })?;
        #[cfg(any(test, feature = "fault-injection"))]
        fault_point("after_commit");
    }

    Ok(())
}

/// 判断目录项是否由本项目的临时文件命名规则生成。
pub fn is_internal_temp_name(name: &str) -> bool {
    name.starts_with(".veil-tmp-")
        || name.starts_with(".veil-pw-")
        || name.starts_with(".veil-file-")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证原子替换后旧内容被完整新内容取代，且不残留临时文件。
    #[test]
    fn atomic_write_replaces_complete_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("state.bin");
        atomic_write(&path, b"old").unwrap();
        atomic_write(&path, b"new-state").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new-state");
        assert!(
            fs::read_dir(dir.path())
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !is_internal_temp_name(&entry.file_name().to_string_lossy()))
        );
    }

    /// 验证目录树创建后目标目录可用。
    #[test]
    fn create_dir_all_durable_creates_nested_paths() {
        let dir = tempfile::TempDir::new().unwrap();
        let nested = dir.path().join("a").join("b").join("c");
        create_dir_all_durable(&nested).unwrap();
        assert!(nested.is_dir());
    }
}

/// 原子移动且拒绝覆盖目标，适用于密文发布和容器目录移动。
pub fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let source = std::ffi::CString::new(source.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "路径包含 NUL"))?;
        let destination = std::ffi::CString::new(destination.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "路径包含 NUL"))?;
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        // SAFETY: 两个 C 字符串在调用期间有效；RENAME_EXCL 禁止替换目标。
        let result = unsafe {
            libc::renameatx_np(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(any(target_os = "linux", target_os = "android"))]
        // SAFETY: syscall 仅在目标平台定义；参数来自仍有效的路径字符串。
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(not(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "linux",
            target_os = "android"
        )))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "此平台尚未实现排他重命名",
        ));
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "linux",
            target_os = "android"
        ))]
        return if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        };
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileW(source: *const u16, destination: *const u16) -> i32;
        }
        let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        if source[..source.len() - 1].contains(&0)
            || destination[..destination.len() - 1].contains(&0)
        {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "路径包含 NUL"));
        }
        // SAFETY: UTF-16 路径以 NUL 结束，调用期间不会释放；MoveFileW 不覆盖目标。
        return if unsafe { MoveFileW(source.as_ptr(), destination.as_ptr()) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        };
    }
    #[cfg(not(any(unix, windows)))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "此平台尚未实现排他重命名",
    ))
}

/// 向全新文件复制密文并持久化，禁止覆盖并保持私有文件权限。
pub fn copy_file_new(source: &Path, destination: &Path) -> io::Result<()> {
    let mut input = fs::File::open(source)?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        output.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    io::copy(&mut input, &mut output)?;
    sync_file(&output)?;
    Ok(())
}

#[cfg(test)]
mod exclusive_publish_tests {
    use super::*;
    #[test]
    fn existing_file_and_existing_directory_are_never_replaced() {
        let root = tempfile::TempDir::new().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        fs::write(&source, b"new").unwrap();
        fs::write(&target, b"keep").unwrap();
        assert!(rename_no_replace(&source, &target).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"keep");
        fs::remove_file(&source).unwrap();
        fs::remove_file(&target).unwrap();
        fs::create_dir(&source).unwrap();
        fs::create_dir(&target).unwrap();
        assert!(rename_no_replace(&source, &target).is_err());
        assert!(source.is_dir());
        assert!(target.is_dir());
    }
}
