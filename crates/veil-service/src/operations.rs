//! 面向调用方的容器操作请求与结果。
//!
//! 客户端与容器交互的统一入口：把「读取信息、列出文件、增删改、打包」
//! 等操作定义成请求枚举（ContainerOperation），统一转交给 VeilManager
//! 执行，再包装成结果枚举（ContainerOperationResult）。密码由调用方保管，
//! 请求层仅借用密码；实际解锁、租约及脱敏能力由底层模块提供，可能持有受保护的密码副本。
//! 本文件包含请求/结果类型、ContainerInfo 展示统计及 execute 分派函数，依赖 VeilManager、备份与文件系统能力。

use crate::veil_ops::{
    AddFileSpec, ContainerPathKind, MovedPathKind, RemovedPathKind, VeilManager,
};
use std::path::Path;
use veil_core::error::VeilError;
use veil_core::metadata::{FileEntry, MetaData};
use veil_core::version::VersionSnapshot;

/// 单次容器操作；密码由调用方保管，仅在调用期间借用。
///
/// 每个变体携带一次操作所需的最小参数；password 字段借用调用方的密码
/// 字符串，操作返回后即不再引用。
pub enum ContainerOperation<'a> {
    /// 读取容器信息与统计（元数据＋磁盘占用＋MIME 分布）。
    ReadInfo {
        /// 本容器数据库密码，借用调用方字符串；不在请求类型内验证。
        password: &'a str,
    },
    /// 仅统计容器目录占用的磁盘字节数。
    DiskUsage,
    /// 读取容器元数据（身份、文件清单、目录、密钥）。
    ReadMetadata {
        /// 本容器数据库密码，借用调用方字符串；不在请求类型内验证。
        password: &'a str,
    },
    /// 读取解密元数据及原始加密数据库文件字节；后者不是元数据 JSON 序列化。
    ReadMetadataSnapshot {
        /// 本容器数据库密码，借用调用方字符串；不在请求类型内验证。
        password: &'a str,
    },
    /// 列出容器内全部文件条目。
    ListFiles {
        /// 本容器数据库密码，借用调用方字符串；不在请求类型内验证。
        password: &'a str,
    },
    /// 在容器内新建目录。
    CreateDirectory {
        /// 容器内部逻辑路径，不是磁盘路径，具体合法性由对应业务方法检查。
        path: &'a str,
        /// 本容器数据库密码，借用调用方字符串；不在请求类型内验证。
        password: &'a str,
    },
    /// 分页列出某个目录的子节点（offset/limit 控制页）。
    ListChildren {
        /// 待列出子项的容器内部父目录路径，分页规则交给底层。
        parent: &'a str,
        /// 从结果开头跳过的节点数（u32），用于分页。
        offset: u32,
        /// 本页最多返回的节点数（u32），实际限制由底层方法处理。
        limit: u32,
        /// 本容器数据库密码，借用调用方字符串；不在请求类型内验证。
        password: &'a str,
    },
    /// 查询某路径是目录还是文件。
    PathKind {
        /// 容器内部逻辑路径，不是磁盘路径，具体合法性由对应业务方法检查。
        path: &'a str,
        /// 本容器数据库密码，借用调用方字符串；不在请求类型内验证。
        password: &'a str,
    },
    /// 批量添加本地文件到容器。
    AddFiles {
        /// 批量导入规格切片；每项 AddFileSpec 对应一个本地来源与容器内目标，按底层导入规则处理。
        files: &'a [AddFileSpec],
        /// 本容器数据库密码，借用调用方字符串；不在请求类型内验证。
        password: &'a str,
    },
    /// 添加单个本地文件到容器根目录。
    AddFile {
        /// 本地来源文件路径，添加时由底层检查是否可读取。
        source: &'a Path,
        /// 本容器数据库密码，借用调用方字符串；不在请求类型内验证。
        password: &'a str,
    },
    /// 解密导出某个文件到本地路径。
    ExtractFile {
        /// 待导出的容器内部文件路径，需能定位文件条目。
        name: &'a str,
        /// 本地输出路径，文件/目录含义和覆盖策略依对应操作而定。
        output: &'a Path,
        /// 本容器数据库密码，借用调用方字符串；不在请求类型内验证。
        password: &'a str,
    },
    /// 解密导出文件并返回其版本快照。
    ExtractFileWithVersion {
        /// 待导出的容器内部文件路径，需能定位文件条目。
        name: &'a str,
        /// 本地输出路径，文件/目录含义和覆盖策略依对应操作而定。
        output: &'a Path,
        /// 本容器数据库密码，借用调用方字符串；不在请求类型内验证。
        password: &'a str,
    },
    /// 删除容器内的文件或目录。
    RemovePath {
        /// 容器内部逻辑路径，不是磁盘路径，具体合法性由对应业务方法检查。
        path: &'a str,
        /// 本容器数据库密码，借用调用方字符串；不在请求类型内验证。
        password: &'a str,
    },
    /// 移动/重命名容器内的文件或目录。
    MovePath {
        /// 容器内待移动或改名的源逻辑路径。
        from: &'a str,
        /// 容器内移动后的目标逻辑路径。
        to: &'a str,
        /// 本容器数据库密码，借用调用方字符串；不在请求类型内验证。
        password: &'a str,
    },
    /// 修改容器密码；full=true 时同时轮换全部文件的加密密钥。
    ChangePassword {
        /// 改密前用于打开数据库的旧密码。
        old: &'a str,
        /// 改密后使用的新密码，合法性由改密流程检查。
        new: &'a str,
        /// true 运行完整密钥轮换流程，false 仅改变数据库的密码保护。
        full: bool,
    },
    /// 把容器打包成第 2 版 .veil 加密包。
    Pack {
        /// 本地输出路径，文件/目录含义和覆盖策略依对应操作而定。
        output: &'a Path,
        /// 本容器数据库密码，借用调用方字符串；不在请求类型内验证。
        password: &'a str,
    },
}

/// 单次容器操作结果。
///
/// 每个变体对应一种操作的成功产物；出错时统一返回 VeilError，不在本枚举
/// 里细分错误。
pub enum ContainerOperationResult {
    /// 容器信息与统计。
    Info(ContainerInfo),
    /// 磁盘占用字节数。
    DiskUsage(u64),
    /// 容器元数据。
    Metadata(MetaData),
    /// 解密元数据及 veil.db 原始加密文件字节，不含独立密文文件，不是完整备份。
    MetadataSnapshot(MetaData, Vec<u8>),
    /// 全部文件条目。
    Files(Vec<FileEntry>),
    /// 子节点列表（分页）。
    Nodes(Vec<crate::storage::container::Node>),
    /// 新建目录后的完整路径。
    DirectoryCreated(String),
    /// 某路径的类型。
    PathKind(ContainerPathKind),
    /// 批量添加后生成的目标路径列表。
    Added(Vec<String>),
    /// 单个添加后的目标路径。
    AddedFile(String),
    /// 导出文件时的版本快照。
    Version(VersionSnapshot),
    /// 删除操作的产物描述。
    Removed(RemovedPathKind),
    /// 移动操作的产物描述与新路径。
    Moved(MovedPathKind, String),
    /// 无返回值的操作（导出、改密）成功标志。
    Done,
    /// 打包成功：包内文件数与包文件大小。
    Packed { file_count: usize, size: u64 },
}

/// 容器信息及统计数据。
pub struct ContainerInfo {
    /// 容器元数据（身份、名称、创建时间、文件与目录清单、密钥材料）。
    pub metadata: MetaData,
    /// 容器目录普通文件逻辑长度之和，不等于文件系统实际分配块数。
    pub disk_usage: u64,
    /// 按 MIME 类型统计的文件数量分布，键是 MIME 类型、值是文件个数。
    pub mime_distribution: std::collections::BTreeMap<String, usize>,
}

/// 分派一次容器业务请求，并把底层结果包装为统一的客户端响应。
/// 入参：manager（&VeilManager）包含容器路径、预期身份和锁选项；operation（ContainerOperation）是一次按值消费的请求，其借用数据须在本次同步调用期间有效。
/// 返回值：成功返回与请求对应的 ContainerOperationResult；导出/改密以 Done 表示成功，打包返回文件数和输出文件字节数。
/// 异常：密码、路径、锁、数据库、认证或文件系统等底层 VeilError 直接传播；打包完成后读取输出大小失败也返回错误，此时包可能已经发布。
/// 注意事项：同步执行并可能阻塞等锁/读写；不额外建立跨操作事务。元数据含解密密钥；请求只借用密码，但底层锁管理器/日志脱敏器可能保存受保护副本，不能理解为返回时所有副本都已销毁。
pub(crate) fn execute(
    manager: &VeilManager,
    operation: ContainerOperation<'_>,
) -> Result<ContainerOperationResult, VeilError> {
    use ContainerOperation as Op;
    use ContainerOperationResult as Out;
    // 改密操作涉及的旧/新密码提前登记进脱敏器，防止意外进入日志。
    match &operation {
        Op::ChangePassword { old, new, .. } => {
            veil_core::logging::remember_sensitive(old);
            veil_core::logging::remember_sensitive(new);
        }
        _ => {}
    }
    Ok(match operation {
        // 读信息：元数据＋目录磁盘占用＋按扩展名猜 MIME 后的分布统计。
        Op::ReadInfo { password } => {
            let metadata = manager.read_meta(password)?;
            let disk_usage = directory_size(&manager.veil_dir)?;
            // 遍历全部文件条目，按扩展名猜测 MIME 类型并计数。
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
        // 磁盘展示统计：递归普通文件逻辑长度，符号链接与特殊文件不计入。
        Op::DiskUsage => Out::DiskUsage(directory_size(&manager.veil_dir)?),
        // 读元数据。
        Op::ReadMetadata { password } => Out::Metadata(manager.read_meta(password)?),
        // 返回解密元数据与加密数据库主文件字节；二者由底层读租约流程取得。
        Op::ReadMetadataSnapshot { password } => {
            let (meta, bytes) = manager.read_meta_snapshot(password)?;
            Out::MetadataSnapshot(meta, bytes)
        }
        // 列出全部文件。
        Op::ListFiles { password } => Out::Files(manager.list_files(password)?),
        // 新建目录，返回新建目录的路径。
        Op::CreateDirectory { path, password } => {
            Out::DirectoryCreated(manager.create_directory(path, password)?)
        }
        // 分页列出子节点。
        Op::ListChildren {
            parent,
            offset,
            limit,
            password,
        } => Out::Nodes(manager.list_children(parent, offset, limit, password)?),
        // 查询路径类型（目录/文件）。
        Op::PathKind { path, password } => Out::PathKind(manager.path_kind(path, password)?),
        // 批量添加文件，返回目标路径列表。
        Op::AddFiles { files, password } => Out::Added(manager.add_files(files, password)?),
        // 单文件添加，返回目标路径。
        Op::AddFile { source, password } => Out::AddedFile(manager.add_file(source, password)?),
        // 解密导出文件（无额外返回值）。
        Op::ExtractFile {
            name,
            output,
            password,
        } => {
            manager.extract_file(name, output, password)?;
            Out::Done
        }
        // 解密导出并返回版本快照。
        Op::ExtractFileWithVersion {
            name,
            output,
            password,
        } => Out::Version(manager.extract_file_with_version(name, output, password)?),
        // 删除路径，返回删除产物描述。
        Op::RemovePath { path, password } => Out::Removed(manager.remove_path(path, password)?),
        // 移动/重命名，返回移动产物描述与新路径。
        Op::MovePath { from, to, password } => {
            let (kind, path) = manager.move_path(from, to, password)?;
            Out::Moved(kind, path)
        }
        // 改密：full 走完整改密（轮换文件密钥重写全部密文），否则只改数据库密码保护。
        Op::ChangePassword { old, new, full } => {
            if full {
                manager.change_password_full(old, new)?
            } else {
                manager.change_password(old, new)?
            };
            Out::Done
        }
        // 打包：不允许覆盖已有输出文件；成功后回报文件数与包大小。
        Op::Pack { output, password } => {
            let count = crate::package::VeilPacker::new(output)
                .without_overwrite()
                .pack_container(manager, password)?;
            Out::Packed {
                file_count: count,
                size: std::fs::metadata(output)?.len(),
            }
        }
    })
}

/// 累计容器目录下普通文件的逻辑长度，供界面展示存储规模。
/// 入参：path（&Path）为要统计的目录。
/// 返回值：成功返回 u64 字节总数，超过上限时饱和为 u64::MAX。
/// 异常：目录遍历、条目类型或元数据读取失败返回 io::Error。
/// 注意事项：递归普通目录，跳过符号链接和特殊文件；统计文件长度而非真实分配磁盘块，硬链接也可能重复计数。无快照锁，结果可能受并发变化影响。
fn directory_size(path: &Path) -> std::io::Result<u64> {
    // 累加已访问子树中的普通文件长度；使用饱和加法避免极端目录导致整数回绕。
    let mut total = 0u64;
    // 遍历目录条目：子目录递归累计，普通文件累加文件长度。
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
