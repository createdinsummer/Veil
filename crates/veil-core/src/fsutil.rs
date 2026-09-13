//! 文件系统持久化辅助函数。
//!
//! 本模块只处理崩溃一致性所需的底层动作：创建目录后同步父目录、先写临时文件再
//! 原子替换目标文件，以及同步目录项。加密格式和业务事务仍由各模块负责。

use std::fs;
use std::io::{self, Write};
use std::path::Path;

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
pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
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
pub(crate) fn create_dir_all_durable(path: &Path) -> io::Result<()> {
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
    let parent = parent_or_current(path);
    create_dir_all_durable(parent)?;

    let mut temp_file = tempfile::Builder::new()
        .prefix(".veil-tmp-")
        .tempfile_in(parent)?;
    temp_file.write_all(data)?;
    temp_file.as_file().sync_all()?;
    temp_file.persist(path).map_err(|error| error.error)?;
    let _ = sync_directory(parent);

    Ok(())
}

/// 判断目录项是否由本项目的临时文件命名规则生成。
pub(crate) fn is_internal_temp_name(name: &str) -> bool {
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
