//! 收集待导入文件与容器内目标路径。
//!
//! 本模块只做「采集」，不做实际加密写入：把用户给定的本地文件或目录
//! 展开成一组 AddFileSpec（源文件路径 → 容器内目标路径），交给导入流水线
//! 分批传输。目录按深度优先遍历，遍历期间可被取消；容器内路径统一使用
//! / 连接由本地目录组件生成的相对路径；用户直接指定的目标文本仍需下游规范校验。
//! 核心入口 collect_add_specs 依赖 WalkDir 和标准路径接口，使用进度模块读取当前任务取消状态。
use crate::veil_ops::AddFileSpec;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// 采集阶段的错误：源不可用、目标无效、遍历失败或用户取消。
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// 当前目录遍历检查点发现用户取消，未返回部分请求列表。
    #[error("导入已中止")]
    Cancelled,
    /// 源路径存在性检查未通过，携带原始本地路径。
    #[error("源路径不存在：{}", .0.display())]
    SourceNotFound(PathBuf),
    /// 源是无法按普通文件或目录处理的节点，携带本地路径。
    #[error("源路径不是文件或目录：{}", .0.display())]
    NotFile(PathBuf),
    /// 目标或默认名称无法解释，携带用于提示的目标/来源描述文本。
    #[error("目标路径无效：{0}")]
    InvalidDestination(String),
    /// 目录枚举/路径转换失败；path 为来源位置，error 为底层错误文案，不是可直接安全导出的日志内容。
    #[error("无法读取目录 {}：{error}", path.display())]
    DirectoryRead { path: PathBuf, error: String },
}

impl ImportError {
    /// 功能：为采集失败提供不带路径或原始错误正文的稳定诊断分类。
    /// 入参：self: &ImportError，为取消、源缺失、非文件、目标无效或目录读取失败之一。
    /// 返回值：&'static str，对应 CANCELLED/SOURCE_NOT_FOUND/NOT_FILE/INVALID_DESTINATION/DIRECTORY_READ。
    /// 异常：枚举匹配，不返回错误。
    /// 注意事项：只分类，不记录日志或重试；错误变体中保存的明文路径仍须经过日志隐私边界。
    pub(crate) fn log_code(&self) -> &'static str {
        match self {
            Self::Cancelled => "CANCELLED",
            Self::SourceNotFound(..) => "SOURCE_NOT_FOUND",
            Self::NotFile(..) => "NOT_FILE",
            Self::InvalidDestination(..) => "INVALID_DESTINATION",
            Self::DirectoryRead { .. } => "DIRECTORY_READ",
        }
    }
}

/// 功能：把一个本地文件或目录展开成后续加密导入所需的源路径与容器内目标映射。
/// 入参：source: &Path 为存在的文件/目录；dest: `Option<&str>` 为目标：文件遇末尾 / 或反斜杠时视为目录，否则为完整路径；
/// 目录遇 None 使用源目录名，Some(空串) 拒绝，非空值去掉末尾分隔符作目标根。
/// 返回值：Ok(`Vec<AddFileSpec>`)，每项是一份普通文件的待传输请求；空目录可返回空列表，不额外生成空目录节点。
/// 异常：源不存在为 SourceNotFound、特殊节点为 NotFile、名称非 UTF-8 或目标无法拼接为 InvalidDestination；遍历失败为 DirectoryRead，目录遍历检查点取消为 Cancelled。
/// 注意事项：只采集，不加密、不写库或持有源文件句柄；源可在随后传输前变化。目录条目不跟随符号链接，显式传入的 source 文件类型判断会遵循标准路径接口语义。
/// 单文件直接给定目标会原样保存，不在此做完整容器路径校验；本函数也不保证把字符串中的每个反斜杠都转换成 /。
pub fn collect_add_specs(
    source: &Path,
    dest: Option<&str>,
) -> Result<Vec<AddFileSpec>, ImportError> {
    // 源不存在：直接报错，不把「路径字符串」当作文件内容导入。
    if !source.exists() {
        return Err(ImportError::SourceNotFound(source.to_path_buf()));
    }
    // 源是文件：目标规则见函数说明，单文件只产生一条请求。
    if source.is_file() {
        let source_name = file_name(source)?;
        let target = match dest {
            // dest 以分隔符结尾 → 视为目录，把源文件名拼进去。
            Some(dest) if is_directory_target(dest) => {
                join_veil_dir(dest.trim_end_matches(['/', '\\']), &source_name)?
            }
            // dest 是普通路径 → 直接作为完整目标路径。
            Some(dest) => dest.to_string(),
            // 未给 dest → 放在容器根目录，用源文件名。
            None => source_name,
        };

        return Ok(vec![AddFileSpec::new(source, target)]);
    }

    // 既不是文件也不是目录（如设备节点、管道）：报「不是文件」。
    if !source.is_dir() {
        return Err(ImportError::NotFile(source.to_path_buf()));
    }

    // 源是目录：先确定容器内的根目录名。
    let root_name = file_name(source)?;
    let target_root = match dest {
        // 非空 dest 作为容器内根目录（去掉末尾分隔符）。
        Some(dest) if !dest.is_empty() => dest.trim_end_matches(['/', '\\']).to_string(),
        // 空 dest：目录导入必须有明确目标，报错。
        Some(_) => return Err(ImportError::InvalidDestination(String::new())),
        // 未给 dest：用源目录名作为容器内根目录。
        None => root_name,
    };

    // 结果集合：每一条对应目录树中的一个普通文件。
    let mut specs = Vec::new();
    // 深度优先遍历源目录；不跟随符号链接，避免把链接目标当目录内容复制。
    for entry in WalkDir::new(source).follow_links(false) {
        // 每访问一个条目都检查取消标记，让长遍历可以及时中止。
        if veil_core::progress::check_cancelled().is_err() {
            return Err(ImportError::Cancelled);
        }
        // 目录项读取失败（如无权限）→ 携带具体路径与错误信息返回。
        let entry = entry.map_err(|error| ImportError::DirectoryRead {
            path: source.to_path_buf(),
            error: error.to_string(),
        })?;

        // 目录导入只收文件；目录本身由导入流水线按需创建。
        if !entry.file_type().is_file() {
            continue;
        }

        // 计算该文件相对源目录的路径，作为容器内的相对路径。
        let relative =
            entry
                .path()
                .strip_prefix(source)
                .map_err(|error| ImportError::DirectoryRead {
                    path: entry.path().to_path_buf(),
                    error: error.to_string(),
                })?;
        let relative = path_to_veil_dir(relative)?;
        // 把相对路径拼到容器内根目录下，得到完整目标路径。
        let target = if target_root.is_empty() {
            relative
        } else {
            join_veil_dir(&target_root, &relative)?
        };

        specs.push(AddFileSpec::new(entry.path(), target));
    }

    Ok(specs)
}

/// 功能：从源路径取得用于容器默认目标名的最后一段。
/// 入参：path: &Path，为本地文件或目录路径。
/// 返回值：Ok(String)，为最后文件名的 UTF-8 文本副本。
/// 异常：没有文件名或名称非 UTF-8 时返回 InvalidDestination，携带展示路径。
/// 注意事项：不访问磁盘、不清理名称里的其他特殊字符，也不验证容器目标路径规则。
fn file_name(path: &Path) -> Result<String, ImportError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| ImportError::InvalidDestination(path.display().to_string()))
}

/// 功能：解释导入目标参数是否明确要求把源文件放到一个目录下。
/// 入参：path: &str 为目标原始文本。
/// 返回值：末字符为 / 或反斜杠返回 true，其他为 false。
/// 异常：字符串检查，不返回错误。
/// 注意事项：仅按末尾字符，不查询容器目录是否存在，也不把无末尾分隔符的真实目录自动识别为目录目标。
fn is_directory_target(path: &str) -> bool {
    // 以 `/` 或 `\` 结尾的字符串按目录处理；其余按完整路径处理。
    path.ends_with('/') || path.ends_with('\\')
}

/// 功能：把容器目标根与文件相对路径合成一条非空逻辑路径。
/// 入参：prefix/suffix: &str 分别为目标根和相对后缀，只移除两者首尾的正斜杠。
/// 返回值：Ok(String)，非空部分以一个 / 连接，只有一部分时直接采用该部分。
/// 异常：两部分去掉正斜杠后都为空返回 InvalidDestination。
/// 注意事项：不会清理内部重复分隔符、反斜杠、.. 或绝对路径语义；完整合法性由后续目标校验负责。
fn join_veil_dir(prefix: &str, suffix: &str) -> Result<String, ImportError> {
    // 只去掉两部分首尾的正斜杠，避免拼接边界再多出斜杠；内部重复分隔符和反斜杠仍保留。
    let prefix = prefix.trim_matches('/');
    let suffix = suffix.trim_matches('/');
    let target = match (prefix.is_empty(), suffix.is_empty()) {
        (true, true) => String::new(),
        (true, false) => suffix.to_string(),
        (false, true) => prefix.to_string(),
        (false, false) => format!("{}/{}", prefix, suffix),
    };

    // 拼接结果为空（例如只给了 `/`）不是合法目标。
    if target.is_empty() {
        return Err(ImportError::InvalidDestination(String::new()));
    }

    Ok(target)
}

/// 功能：把目录遍历产生的本地相对组件连接成容器内路径，减少平台分隔符差异。
/// 入参：path: &Path，通常为文件相对导入源目录的路径。
/// 返回值：Ok(String)，各组件转 UTF-8 后用 / 拼接，再经 join_veil_dir 拒绝空结果。
/// 异常：任一组件非 UTF-8 或最终结果空时返回 InvalidDestination。
/// 注意事项：本工具不限定组件必须是 Normal，也不规范化业务路径；调用方应传入 strip_prefix 得到的可信相对路径。
fn path_to_veil_dir(path: &Path) -> Result<String, ImportError> {
    // 逐级收集路径组件名（每级代表容器内的一层目录）。
    let mut parts = Vec::new();
    for component in path.components() {
        let part = component
            .as_os_str()
            .to_str()
            .ok_or_else(|| ImportError::InvalidDestination(path.display().to_string()))?;
        parts.push(part);
    }

    // 用 `/` 连接各组件：容器内路径与平台无关。
    join_veil_dir("", &parts.join("/"))
}
