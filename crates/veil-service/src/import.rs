//! 收集待导入文件与容器内目标路径。
use crate::veil_ops::AddFileSpec;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("导入已中止")]
    Cancelled,
    #[error("源路径不存在：{}", .0.display())]
    SourceNotFound(PathBuf),
    #[error("源路径不是文件或目录：{}", .0.display())]
    NotFile(PathBuf),
    #[error("目标路径无效：{0}")]
    InvalidDestination(String),
    #[error("无法读取目录 {}：{error}", path.display())]
    DirectoryRead { path: PathBuf, error: String },
}

/// 将文件或目录展开为工作区添加请求。
pub fn collect_add_specs(
    source: &Path,
    dest: Option<&str>,
) -> Result<Vec<AddFileSpec>, ImportError> {
    if !source.exists() {
        return Err(ImportError::SourceNotFound(source.to_path_buf()));
    }
    if source.is_file() {
        let source_name = file_name(source)?;
        let target = match dest {
            Some(dest) if is_directory_target(dest) => {
                join_veil_dir(dest.trim_end_matches(['/', '\\']), &source_name)?
            }
            Some(dest) => dest.to_string(),
            None => source_name,
        };

        return Ok(vec![AddFileSpec::new(source, target)]);
    }

    if !source.is_dir() {
        return Err(ImportError::NotFile(source.to_path_buf()));
    }

    let root_name = file_name(source)?;
    let target_root = match dest {
        Some(dest) if !dest.is_empty() => dest.trim_end_matches(['/', '\\']).to_string(),
        Some(_) => return Err(ImportError::InvalidDestination(String::new())),
        None => root_name,
    };

    let mut specs = Vec::new();
    for entry in WalkDir::new(source).follow_links(false) {
        if veil_core::progress::check_cancelled().is_err() {
            return Err(ImportError::Cancelled);
        }
        let entry = entry.map_err(|error| ImportError::DirectoryRead {
            path: source.to_path_buf(),
            error: error.to_string(),
        })?;

        if !entry.file_type().is_file() {
            continue;
        }

        let relative =
            entry
                .path()
                .strip_prefix(source)
                .map_err(|error| ImportError::DirectoryRead {
                    path: entry.path().to_path_buf(),
                    error: error.to_string(),
                })?;
        let relative = path_to_veil_dir(relative)?;
        let target = if target_root.is_empty() {
            relative
        } else {
            join_veil_dir(&target_root, &relative)?
        };

        specs.push(AddFileSpec::new(entry.path(), target));
    }

    Ok(specs)
}

/// 返回路径最后一级文件名。
fn file_name(path: &Path) -> Result<String, ImportError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| ImportError::InvalidDestination(path.display().to_string()))
}

/// 判断目标参数是否明确表示目录。
fn is_directory_target(path: &str) -> bool {
    path.ends_with('/') || path.ends_with('\\')
}

/// 拼接容器内路径，统一使用 `/`。
fn join_veil_dir(prefix: &str, suffix: &str) -> Result<String, ImportError> {
    let prefix = prefix.trim_matches('/');
    let suffix = suffix.trim_matches('/');
    let target = match (prefix.is_empty(), suffix.is_empty()) {
        (true, true) => String::new(),
        (true, false) => suffix.to_string(),
        (false, true) => prefix.to_string(),
        (false, false) => format!("{}/{}", prefix, suffix),
    };

    if target.is_empty() {
        return Err(ImportError::InvalidDestination(String::new()));
    }

    Ok(target)
}

/// 把本地相对路径转换为统一的容器内路径。
fn path_to_veil_dir(path: &Path) -> Result<String, ImportError> {
    let mut parts = Vec::new();
    for component in path.components() {
        let part = component
            .as_os_str()
            .to_str()
            .ok_or_else(|| ImportError::InvalidDestination(path.display().to_string()))?;
        parts.push(part);
    }

    join_veil_dir("", &parts.join("/"))
}
