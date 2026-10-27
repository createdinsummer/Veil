//! 面向调用方的容器操作请求与结果。

use crate::veil_ops::{
    AddFileSpec, ContainerPathKind, MovedPathKind, RemovedPathKind, VeilManager,
};
use std::path::Path;
use veil_core::error::VeilError;
use veil_core::metadata::{FileEntry, MetaData, MetaHeader};
use veil_core::version::VersionSnapshot;

/// 单次容器操作；密码由调用方保管，仅在调用期间借用。
pub enum ContainerOperation<'a> {
    ReadInfo {
        password: &'a str,
    },
    DiskUsage,
    ReadMetadata {
        password: &'a str,
    },
    ReadMetadataSnapshot {
        password: &'a str,
    },
    ReadHeader,
    ListFiles {
        password: &'a str,
    },
    PathKind {
        path: &'a str,
        password: &'a str,
    },
    AddFiles {
        files: &'a [AddFileSpec],
        password: &'a str,
    },
    AddFile {
        source: &'a Path,
        password: &'a str,
    },
    ExtractFile {
        name: &'a str,
        output: &'a Path,
        password: &'a str,
    },
    ExtractFileWithVersion {
        name: &'a str,
        output: &'a Path,
        password: &'a str,
    },
    RemovePath {
        path: &'a str,
        password: &'a str,
    },
    MovePath {
        from: &'a str,
        to: &'a str,
        password: &'a str,
    },
    ChangePassword {
        old: &'a str,
        new: &'a str,
        full: bool,
    },
    Pack {
        output: &'a Path,
        password: &'a str,
    },
}

/// 单次容器操作结果。
pub enum ContainerOperationResult {
    Info(ContainerInfo),
    DiskUsage(u64),
    Metadata(MetaData),
    MetadataSnapshot(MetaData, Vec<u8>),
    Header(MetaHeader),
    Files(Vec<FileEntry>),
    PathKind(ContainerPathKind),
    Added(Vec<String>),
    AddedFile(String),
    Version(VersionSnapshot),
    Removed(RemovedPathKind),
    Moved(MovedPathKind, String),
    Done,
    Packed { file_count: usize, size: u64 },
}

/// 容器信息及统计数据。
pub struct ContainerInfo {
    pub metadata: MetaData,
    pub disk_usage: u64,
    pub mime_distribution: std::collections::BTreeMap<String, usize>,
}

pub(crate) fn execute(
    manager: &VeilManager,
    operation: ContainerOperation<'_>,
) -> Result<ContainerOperationResult, VeilError> {
    use ContainerOperation as Op;
    use ContainerOperationResult as Out;
    Ok(match operation {
        Op::ReadInfo { password } => {
            let metadata = manager.read_meta(password)?;
            let disk_usage = directory_size(&manager.veil_dir)?;
            let mut mime_distribution = std::collections::BTreeMap::new();
            for file in &metadata.files {
                let mime = veil_core::mime::guess_mime(&file.original_name)
                    .unwrap_or_else(|| "application/octet-stream".into());
                *mime_distribution.entry(mime).or_insert(0) += 1;
            }
            Out::Info(ContainerInfo {
                metadata,
                disk_usage,
                mime_distribution,
            })
        }
        Op::DiskUsage => Out::DiskUsage(directory_size(&manager.veil_dir)?),
        Op::ReadMetadata { password } => Out::Metadata(manager.read_meta(password)?),
        Op::ReadMetadataSnapshot { password } => {
            let (meta, bytes) = manager.read_meta_snapshot(password)?;
            Out::MetadataSnapshot(meta, bytes)
        }
        Op::ReadHeader => Out::Header(manager.read_meta_header()?),
        Op::ListFiles { password } => Out::Files(manager.list_files(password)?),
        Op::PathKind { path, password } => Out::PathKind(manager.path_kind(path, password)?),
        Op::AddFiles { files, password } => Out::Added(manager.add_files(files, password)?),
        Op::AddFile { source, password } => Out::AddedFile(manager.add_file(source, password)?),
        Op::ExtractFile {
            name,
            output,
            password,
        } => {
            manager.extract_file(name, output, password)?;
            Out::Done
        }
        Op::ExtractFileWithVersion {
            name,
            output,
            password,
        } => Out::Version(manager.extract_file_with_version(name, output, password)?),
        Op::RemovePath { path, password } => Out::Removed(manager.remove_path(path, password)?),
        Op::MovePath { from, to, password } => {
            let (kind, path) = manager.move_path(from, to, password)?;
            Out::Moved(kind, path)
        }
        Op::ChangePassword { old, new, full } => {
            if full {
                manager.change_password_full(old, new)?
            } else {
                manager.change_password(old, new)?
            };
            Out::Done
        }
        Op::Pack { output, password } => {
            let (metadata, bytes) = manager.read_meta_snapshot(password)?;
            crate::package::VeilPacker::new(output)
                .without_overwrite()
                .pack(&manager.veil_dir, &metadata, &bytes)?;
            Out::Packed {
                file_count: metadata.files.len(),
                size: std::fs::metadata(output)?.len(),
            }
        }
    })
}

fn directory_size(path: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            total = total.saturating_add(directory_size(&entry.path())?);
        } else if file_type.is_file() {
            total = total.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(total)
}
