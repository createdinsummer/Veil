//! 容器创建与解包的准备、校验及执行。
//!
//! 所属模块：Veil 服务层工作流模块，负责把 CLI、桌面端等入口传入的选项转换为计划，
//! 并在提交阶段创建或还原容器、登记链接和更新应用配置。
//! 主要数据：CreateOptions / UnpackOptions 是入口参数；CreatePlan / UnpackPlan 保存
//! 已规范化的路径和身份；CreatedContainer / UnpackedContainer 是业务结果；WorkflowError
//! 区分参数冲突、路径占用、I/O 和底层容器错误。
//! 核心流程：prepare_create / prepare_unpack 解析并规划（配置选择可能写入应用库）；commit_create / commit_unpack
//! 会再次检查并执行写入；check_*_paths 用于发现计划准备后磁盘状态发生的变化。
//! 依赖能力：应用配置、工作区解析、容器生命周期管理、加密备份读取、文件系统检查、
//! 稳定容器 ID 生成和时间格式化。
//! 整体目标：将“先验证并展示计划”和“确认后写入数据”分开，避免入口参数错误、路径
//! 冲突以及检查后状态变化造成覆盖；所有创建和解包入口复用同一套业务规则。

use crate::config::AppConfig;
use crate::lifecycle::{self, CreationError};
use crate::package::VeilUnpacker;
use crate::work::WorkType;
use std::path::{Path, PathBuf};
use veil_core::error::VeilError;
use veil_core::metadata::generate_veil_id;

/// 创建容器使用的原始参数。
///
/// 字段保留调用方输入，路径规范化、工作区解析和目标目录计算由 prepare_create 完成，
/// 因此本结构本身不表示请求已经通过校验。
pub struct CreateOptions<'a> {
    /// 用户输入的容器名或 .veil-link 链接路径；不能为空。
    pub target: &'a str,
    /// 可选的链接输出路径；省略时沿用目标链接或自动生成默认路径。
    pub link_output: Option<&'a str>,
    /// 可选工作区名称；不能与 work_root 同时指定。
    pub work_name: Option<&'a str>,
    /// 可选工作区根目录；由配置层按规则解析。
    pub work_root: Option<&'a Path>,
    /// 是否创建只容纳一个容器的专属工作区；开启时必须提供 work_root。
    pub dedicated: bool,
    /// 是否强制启用便携模式；不能与工作区名称或路径选项组合。
    pub portable: bool,
}

impl WorkflowError {
    /// 将工作流失败原因映射为固定诊断代码。
    /// 入参：self（&WorkflowError）为业务错误。
    /// 返回值：与变体对应的 &'static str，不拼入路径或密码。
    /// 异常：无错误返回。
    /// 注意事项：代码用于日志聚合，不代替面向用户的错误正文。
    pub(crate) fn log_code(&self) -> &'static str {
        match self {
            Self::ConflictingWorkOptions => "CONFLICTING_WORK_OPTIONS",
            Self::DedicatedRequiresPath => "DEDICATED_REQUIRES_PATH",
            Self::PortableConflictsWork => "PORTABLE_CONFLICTS_WORK",
            Self::InvalidLinkName => "INVALID_LINK_NAME",
            Self::InvalidName => "INVALID_NAME",
            Self::InvalidLinkExtension(..) => "INVALID_LINK_EXTENSION",
            Self::LinkExists(..) => "LINK_EXISTS",
            Self::ContainerExists(..) => "CONTAINER_EXISTS",
            Self::DedicatedNotEmpty(..) => "DEDICATED_NOT_EMPTY",
            Self::PackageNotFound(..) => "PACKAGE_NOT_FOUND",
            Self::UnpackNameInvalid => "UNPACK_NAME_INVALID",
            Self::UnpackMissingId => "UNPACK_MISSING_ID",
            Self::UnpackDirectoryExists(..) => "UNPACK_DIRECTORY_EXISTS",
            Self::UnpackInvalidLinkExtension(..) => "UNPACK_INVALID_LINK_EXTENSION",
            Self::LinkMissingId => "LINK_MISSING_ID",
            Self::LinkInvalidExtension(..) => "LINK_INVALID_EXTENSION",
            Self::PackOutputExists(..) => "PACK_OUTPUT_EXISTS",
            Self::PasswordEmpty => "PASSWORD_EMPTY",
            Self::InvalidHintLevel => "INVALID_HINT_LEVEL",
            Self::Core(..) => "CORE",
            Self::Io(..) => "IO",
            Self::CreateFailed { .. } => "CREATE_FAILED",
            Self::LinkRegistrationFailed { .. } => "LINK_REGISTRATION_FAILED",
        }
    }
}

/// 解包使用的原始参数。
///
/// 该结构只承载入口提供的信息；加密包内的稳定容器 ID 要等密码验证并读取元数据后才能确定。
pub struct UnpackOptions<'a> {
    /// .veil 加密备份包的文件路径。
    pub package_path: &'a str,
    /// 可选的导入后展示名；省略时从包文件名推导。
    pub name: Option<&'a str>,
    /// 可选的目标工作区名称。
    pub work_name: Option<&'a str>,
    /// 可选的链接输出路径；省略时在当前目录生成默认链接。
    pub link_output: Option<&'a str>,
}

/// 工作流错误，调用方可映射成各自的提示。
///
/// 各错误按业务原因区分，供 CLI、桌面和移动入口共享底层流程并展示合适文案。携带路径的
/// 变体保留冲突位置；Core 和 Io 变体保留底层错误原因。
#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    /// 同时传入工作区名称和工作区路径，二者属于互斥配置。
    #[error("不能同时指定工作区名称和路径")]
    ConflictingWorkOptions,
    /// 请求专属工作区但没有给出该工作区的目录路径。
    #[error("专属工作区需要指定路径")]
    DedicatedRequiresPath,
    /// 同时使用便携模式和工作区选项，存储策略无法合并。
    #[error("便携模式与工作区参数冲突")]
    PortableConflictsWork,
    /// 无法从目标路径中取得可用的链接文件名。
    #[error("链接名称无效")]
    InvalidLinkName,
    /// 容器名称为空或目标路径中的文件名不是有效文本。
    #[error("容器名称无效")]
    InvalidName,
    /// 新建链接的扩展名不是规定的 .veil-link。
    #[error("链接扩展名无效：{}", .0.display())]
    InvalidLinkExtension(PathBuf),
    /// 链接目标已经存在，防止替换已有入口。
    #[error("链接已存在：{}", .0.display())]
    LinkExists(PathBuf),
    /// 容器目标目录已存在，防止覆盖已有数据。
    #[error("容器目录已存在：{}", .0.display())]
    ContainerExists(PathBuf),
    /// 专属工作区已有条目，不满足单容器工作区约束。
    #[error("专属工作区非空：{}", .0.display())]
    DedicatedNotEmpty(PathBuf),
    /// 输入的加密备份包路径不存在。
    #[error("打包文件不存在：{}", .0.display())]
    PackageNotFound(PathBuf),
    /// 没有提供名称，且无法从加密包文件名推导名称。
    #[error("无法确定容器名称")]
    UnpackNameInvalid,
    /// 加密包元数据缺少用于登记容器的稳定 ID。
    #[error("打包文件缺少容器 ID")]
    UnpackMissingId,
    /// 解包目标目录被占用，或专属工作区不为空。
    #[error("解包目标已存在或专属工作区非空：{}", .0.display())]
    UnpackDirectoryExists(PathBuf),
    /// 解包产生的链接路径未使用 .veil-link 扩展名。
    #[error("解包链接扩展名无效：{}", .0.display())]
    UnpackInvalidLinkExtension(PathBuf),
    /// 要登记的链接没有关联稳定容器 ID。
    #[error("链接缺少容器 ID")]
    LinkMissingId,
    /// 既有链接操作收到扩展名不符的路径，供公共错误映射使用。
    #[error("链接扩展名无效：{}", .0.display())]
    LinkInvalidExtension(PathBuf),
    /// 备份包输出路径已经占用。
    #[error("打包文件已存在：{}", .0.display())]
    PackOutputExists(PathBuf),
    /// 调用方提供空密码，不能初始化或解密容器。
    #[error("密码不能为空")]
    PasswordEmpty,
    /// 提示级别不属于服务支持的取值。
    #[error("提示级别无效")]
    InvalidHintLevel,
    /// 容器加密、元数据或其他核心业务流程失败。
    #[error("{0}")]
    Core(#[from] VeilError),
    /// 文件系统读取或写入失败。
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// 创建目录或初始化数据库失败；directory 指向本次创建目标。
    #[error("创建容器失败 {}：{source}", directory.display())]
    CreateFailed {
        directory: PathBuf,
        #[source]
        source: VeilError,
    },
    /// 容器创建后登记链接失败；容器目录会保留以便诊断或恢复。
    #[error("登记链接失败 {}：{source}", path.display())]
    LinkRegistrationFailed {
        path: PathBuf,
        #[source]
        source: VeilError,
    },
}

/// 经服务层准备的创建计划，字段不能由调用方修改。
///
/// 保存配置快照、稳定容器 ID、展示名、工作区和输出路径。提交前仍会重新检查路径，因为
/// 用户确认期间磁盘内容可能变化。
pub struct CreatePlan {
    /// 保存容器和工作区登记信息的应用配置快照。
    config: AppConfig,
    /// 新容器的稳定身份标识，也用作容器目录名。
    veil_id: String,
    /// 用户在容器列表中看到的名称。
    name: String,
    /// 指向容器的链接文件输出路径。
    link_path: PathBuf,
    /// 容器所属工作区的根目录。
    work_root: PathBuf,
    /// 由工作区根目录和稳定 ID 组合得到的容器目录。
    directory: PathBuf,
    /// 工作区是否只能容纳当前这个容器。
    dedicated: bool,
    /// 根据显式选项或卷属性推导的便携标志；当前提交函数不据此改变工作区路径。
    portable_mode: bool,
}
impl CreatePlan {
    /// 借用创建计划的实际容器目标位置，供界面显示。
    /// 入参：self（&CreatePlan）为准备完成的计划。
    /// 返回值：与计划同寿命的 &Path。
    /// 异常：无错误返回。
    /// 注意事项：不创建或再次检查目录；计划准备后的外部状态可能变化。
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    /// 读取创建计划推导出的便携模式标志。
    /// 入参：self（&CreatePlan）为创建计划。
    /// 返回值：bool，显式便携选项或链接位置卷被判断为外置卷时为 true。
    /// 异常：无错误返回。
    /// 注意事项：当前提交代码不根据该标志改变创建目录；空间位置仍由配置选择结果决定。
    pub fn portable_mode(&self) -> bool {
        self.portable_mode
    }
}

/// 经服务层准备的解包计划，字段不能由调用方修改。
///
/// 准备阶段尚不知道包内容器 ID，因此 veil_id 为空，directory 先表示待定位置；验证密码后，
/// commit_unpack 从加密元数据读出 ID、计算最终目录并重新校验。
pub struct UnpackPlan {
    /// 用于登记导入容器的应用配置。
    config: AppConfig,
    /// 等待解密并还原的备份包路径。
    package_path: PathBuf,
    /// 解密元数据前未知的容器稳定 ID。
    veil_id: String,
    /// 导入后展示给用户的容器名称。
    name: String,
    /// 解包完成后创建的链接路径。
    link_path: PathBuf,
    /// 容器所属工作区根目录。
    work_root: PathBuf,
    /// 解包目标目录；提交阶段会按包内稳定 ID 更新。
    directory: PathBuf,
    /// 工作区是否只能容纳当前容器。
    dedicated: bool,
}

/// 创建结果，供调用方展示。
pub struct CreatedContainer {
    /// 创建后的容器展示名称。
    pub name: String,
    /// 用户打开容器时使用的链接路径。
    pub link_path: PathBuf,
    /// 实际保存加密数据的目录。
    pub directory: PathBuf,
}
/// 解包结果，供调用方展示。
pub struct UnpackedContainer {
    /// 导入后的容器展示名称。
    pub name: String,
    /// 导入后生成的链接路径。
    pub link_path: PathBuf,
    /// 还原出的加密容器目录。
    pub directory: PathBuf,
    /// 本次从包中成功还原的文件数。
    pub file_count: usize,
}

/// 为容器选择当前目录下尚未存在的默认链接名。
/// 入参：name（&str）为展示名；本函数不会先对首选原名做文件名清理。
/// 返回值：首选 name.veil-link；占用时用处理后的名称、秒级时间和递增序号生成候选 PathBuf。
/// 异常：无 Result 返回；理论上 u32 序号耗尽会到 unreachable! 并 panic。
/// 注意事项：current_dir 失败退回相对路径“.”；exists 不预留目标且可能把权限错误或悬空链接视为不存在，发布仍需防覆盖。
fn default_link_path(name: &str) -> PathBuf {
    let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let default = dir.join(format!("{name}.veil-link"));
    if !default.exists() {
        return default;
    }
    let timestamp = chrono::Local::now().format("%Y%m%d%H%M%S").to_string();
    let base = crate::work::veil_name_with_suffix(name, &timestamp);
    // 同一秒内可能创建多个同名容器，逐个追加序号直到找到未占用名称。
    for index in 1u32.. {
        let file = if index == 1 {
            format!("{base}.veil-link")
        } else {
            format!("{base}-{index}.veil-link")
        };
        let candidate = dir.join(file);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("默认链接名称序号不可能耗尽")
}

/// 检查专属工作区当前是否已有目录项。
/// 入参：path（&Path）为候选根目录。
/// 返回值：is_dir 为 false 时返回 false；目录能读取且无首个条目时 true。
/// 异常：读取目录或首个条目失败返回 io::Error。
/// 注意事项：不递归，只需一个条目即可判断非空；is_dir 自身吞掉的元数据错误会表现为 false。
fn directory_is_empty(path: &Path) -> std::io::Result<bool> {
    if !path.is_dir() {
        return Ok(false);
    }
    Ok(std::fs::read_dir(path)?.next().transpose()?.is_none())
}

/// 复核创建的链接路径、专属工作区、目标容器目录和本机 ID 登记。
/// 入参：plan（&CreatePlan）为待提交计划。
/// 返回值：各项检查通过返回 Ok(())。
/// 异常：链接扩展名错误/已占用、专属目录非空、容器目录占用、ID 冲突或底层查询/I/O 失败返回 WorkflowError。
/// 注意事项：此处不预留路径；后续还需原子 create_dir/create_new。ensure_veil_id_available 可能打开/初始化应用库，不能承诺绝对无副作用。
fn check_create_paths(plan: &CreatePlan) -> Result<(), WorkflowError> {
    if plan.link_path.extension().and_then(|v| v.to_str()) != Some("veil-link") {
        return Err(WorkflowError::InvalidLinkExtension(plan.link_path.clone()));
    }
    if plan.link_path.exists() {
        return Err(WorkflowError::LinkExists(plan.link_path.clone()));
    }
    if plan.dedicated && plan.work_root.exists() && !directory_is_empty(&plan.work_root)? {
        return Err(WorkflowError::DedicatedNotEmpty(plan.work_root.clone()));
    }
    if plan.directory.exists() {
        return Err(WorkflowError::ContainerExists(plan.directory.clone()));
    }
    plan.config.ensure_veil_id_available(&plan.veil_id)?;
    Ok(())
}


/// 把入口选项解析为可展示的创建计划，为后续初始化确定身份与路径。
/// 入参：options（CreateOptions）含 target 名称/链接、可选 link_output、work_name 或 work_root、dedicated、portable；工作区名称与路径互斥，专属必须给路径，便携不能搭配工作区选项。
/// 返回值：返回含新 UUID、名称、链接、工作区、目录和便携标志的 CreatePlan。
/// 异常：选项冲突、空白名称、扩展名/路径占用、工作区/卷查询或配置失败返回 WorkflowError。
/// 注意事项：不创建容器数据库或链接；配置加载和工作区选择可能创建应用库、默认空间或登记，因此不是纯只读。通用入口仅检查名称非空白，不采用 spaces 的100字符校验。
pub(crate) fn prepare_create(options: CreateOptions<'_>) -> Result<CreatePlan, WorkflowError> {
    // 工作区名称和路径定义了两种不同的选择方式，先拒绝同时传入的冲突组合。
    if options.work_name.is_some() && options.work_root.is_some() {
        return Err(WorkflowError::ConflictingWorkOptions);
    }
    if options.dedicated && options.work_root.is_none() {
        return Err(WorkflowError::DedicatedRequiresPath);
    }
    if options.portable && (options.work_name.is_some() || options.work_root.is_some()) {
        return Err(WorkflowError::PortableConflictsWork);
    }
    // 将用户目标解释为容器名或链接路径，供后续推导展示名称和默认链接位置。
    let target = Path::new(options.target);
    let name = if target.extension().and_then(|v| v.to_str()) == Some("veil-link") {
        target
            .file_stem()
            .and_then(|v| v.to_str())
            .ok_or(WorkflowError::InvalidLinkName)?
            .to_owned()
    } else if target.parent().is_some_and(|p| !p.as_os_str().is_empty()) {
        target
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or(WorkflowError::InvalidName)?
            .to_owned()
    } else {
        options.target.to_owned()
    };
    if name.trim().is_empty() {
        return Err(WorkflowError::InvalidName);
    }
    // 输出路径优先级：显式链接路径、输入中已包含的链接路径、自动生成的默认路径。
    let link_path = if let Some(link) = options.link_output {
        PathBuf::from(link)
    } else if target.extension().and_then(|v| v.to_str()) == Some("veil-link") {
        target.to_path_buf()
    } else {
        default_link_path(&name)
    };
    if link_path.extension().and_then(|v| v.to_str()) != Some("veil-link") {
        return Err(WorkflowError::InvalidLinkExtension(link_path));
    }
    if link_path.exists() {
        return Err(WorkflowError::LinkExists(link_path));
    }
    // 为卷检测计算绝对链接目录；计划仍保存原 link_path，creation_work 的位置参数当前不决定工作区。
    let absolute = if link_path.is_absolute() {
        link_path.clone()
    } else {
        std::env::current_dir()?.join(&link_path)
    };
    let link_dir = absolute.parent().unwrap_or_else(|| Path::new("."));
    let (config, work) = AppConfig::creation_work(
        link_dir,
        options.work_name,
        options.work_root,
        options.dedicated,
    )?;
    let work_root = config.resolve_work_root(&work)?;
    // 未显式指定时，根据链接所在卷是否为外置卷自动选择便携规则。
    let portable_mode =
        options.portable || veil_core::volume::volume_for_path(link_dir)?.if_out_volume;
    let veil_id = generate_veil_id();
    let directory = work_root.join(&veil_id);
    let plan = CreatePlan {
        config,
        veil_id,
        name,
        link_path,
        work_root,
        directory,
        dedicated: matches!(work.work_type, WorkType::Dedicated),
        portable_mode,
    };
    check_create_paths(&plan)?;
    Ok(plan)
}

/// 提交创建计划，初始化全新加密数据库并登记入口。
/// 入参：plan（CreatePlan）被消费；password（&str）为新容器非空密码。
/// 返回值：全部创建及登记成功返回 CreatedContainer。
/// 异常：空密码、准备后路径变化、初始化、登记、链接或 I/O 失败返回 WorkflowError。
/// 注意事项：依赖生命周期层处理部分失败；初始化成功后登记失败保留数据。该入口不因 portable_mode 标志另选存储位置。
pub(crate) fn commit_create(
    plan: CreatePlan,
    password: &str,
) -> Result<CreatedContainer, WorkflowError> {
    commit_create_with_initializer(plan, password, |directory, id, name, password| {
        crate::veil_ops::VeilManager::for_veil(directory.into(), id)
            .init_veil_with_id(id, name, password)
    })
}

/// 复用创建的目录占位和登记流程，并允许调用方提供实际初始化方式。
/// 入参：plan 为消费的 CreatePlan；password 为非空密码；initialize 为 FnOnce(&Path, &str, &str, &str) -> `Result<MetaData, VeilError>`，依次收到已占位目录、ID、名称、密码。
/// 返回值：初始化和登记成功返回 CreatedContainer。
/// 异常：参数/路径冲突、回调错误、登记或链接失败映射为 WorkflowError。
/// 注意事项：回调应只初始化已占位目录并返回可信同身份元数据。回调返回 Err 时生命周期层可能递归删除该目录，即使回调已写入部分有效数据；返回 Ok 后才进入保留数据策略。
pub(crate) fn commit_create_with_initializer(
    mut plan: CreatePlan,
    password: &str,
    initialize: impl FnOnce(&Path, &str, &str, &str) -> Result<veil_core::metadata::MetaData, VeilError>,
) -> Result<CreatedContainer, WorkflowError> {
    if password.is_empty() {
        return Err(WorkflowError::PasswordEmpty);
    }
    check_create_paths(&plan)?;
    let existed = plan.work_root.exists();
    lifecycle::create_registered_container(
        &mut plan.config,
        &plan.directory,
        &plan.veil_id,
        &plan.name,
        password,
        &plan.work_root,
        existed,
        &plan.link_path,
        initialize,
    )
    .map_err(|error| match error {
        CreationError::ContainerExists(path) => WorkflowError::ContainerExists(path),
        CreationError::LinkExists(path) => WorkflowError::LinkExists(path),
        CreationError::Initialize(source) => WorkflowError::CreateFailed {
            directory: plan.directory.clone(),
            source,
        },
        CreationError::Link(source) => WorkflowError::LinkRegistrationFailed {
            path: plan.link_path.clone(),
            source: VeilError::WorkError(format!(
                "{source}；容器保留在 {}",
                plan.directory.display()
            )),
        },
        CreationError::RegisterInitialized { directory, source } => {
            WorkflowError::CreateFailed { directory, source }
        }
        CreationError::Register(source) => WorkflowError::Core(source),
    })?;
    Ok(CreatedContainer {
        name: plan.name,
        link_path: plan.link_path,
        directory: plan.directory,
    })
}

/// 复核解包输入是否存在、专属根目录和链接输出是否可用。
/// 入参：plan（&UnpackPlan）可处于 ID 未知或已解密阶段，空 ID 时跳过本机身份占用检查。
/// 返回值：检查通过返回 Ok(())。
/// 异常：包缺失、专属目录非空、链接后缀/占用或本机 ID 冲突/底层错误返回 WorkflowError。
/// 注意事项：不检查最终 directory 是否存在，也不认证包内容；真正目标发布由解包层的不覆盖操作负责。
fn check_unpack_paths(plan: &UnpackPlan) -> Result<(), WorkflowError> {
    if !plan.package_path.exists() {
        return Err(WorkflowError::PackageNotFound(plan.package_path.clone()));
    }
    if plan.dedicated && plan.work_root.exists() && !directory_is_empty(&plan.work_root)? {
        return Err(WorkflowError::UnpackDirectoryExists(plan.work_root.clone()));
    }
    if plan.link_path.extension().and_then(|v| v.to_str()) != Some("veil-link") {
        return Err(WorkflowError::UnpackInvalidLinkExtension(
            plan.link_path.clone(),
        ));
    }
    if plan.link_path.exists() {
        return Err(WorkflowError::LinkExists(plan.link_path.clone()));
    }
    if !plan.veil_id.is_empty() {
        plan.config.ensure_veil_id_available(&plan.veil_id)?;
    }
    Ok(())
}

/// 解析还原目标及链接方案，并检查加密包外层是否可读取。
/// 入参：options（UnpackOptions）含 package_path、可选展示 name、work_name 和 link_output；名称省略时取包文件 stem 并剥去可选 .vault。
/// 返回值：返回 UnpackPlan，veil_id 为空，directory 暂为 work_root/pending。
/// 异常：包缺失、空白/无法推导名称、专属目录/链接冲突、包外层格式或配置/I/O 失败返回 WorkflowError。
/// 注意事项：不使用密码、不认证加密正文；读取外层加密元数据只做格式准备。配置/工作区选择可能写应用库或创建目录，不能承诺完全不写配置。
pub(crate) fn prepare_unpack(options: UnpackOptions<'_>) -> Result<UnpackPlan, WorkflowError> {
    let package_path = PathBuf::from(options.package_path);
    if !package_path.exists() {
        return Err(WorkflowError::PackageNotFound(package_path));
    }
    // 优先使用调用方指定的展示名；否则从包文件名推导并剥去可选的复合后缀。
    let name = if let Some(name) = options.name {
        name.to_owned()
    } else {
        let stem = package_path
            .file_stem()
            .and_then(|v| v.to_str())
            .ok_or(WorkflowError::UnpackNameInvalid)?;
        stem.strip_suffix(".vault").unwrap_or(stem).to_owned()
    };
    if name.trim().is_empty() {
        return Err(WorkflowError::UnpackNameInvalid);
    }
    let location = options
        .link_output
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let (config, work) = AppConfig::creation_work(&location, options.work_name, None, false)?;
    let work_root = config.resolve_work_root(&work)?;
    let dedicated = matches!(work.work_type, WorkType::Dedicated);
    if dedicated && work_root.exists() && !directory_is_empty(&work_root)? {
        return Err(WorkflowError::UnpackDirectoryExists(work_root));
    }
    VeilUnpacker::new(&package_path).read_encrypted_metadata()?;
    let link_path = options
        .link_output
        .map(PathBuf::from)
        .unwrap_or_else(|| default_link_path(&name));
    let directory = work_root.join("pending");
    let plan = UnpackPlan {
        config,
        package_path,
        veil_id: String::new(),
        name,
        link_path,
        work_root,
        directory,
        dedicated,
    };
    check_unpack_paths(&plan)?;
    Ok(plan)
}

/// 验证密码读出包内身份，再还原并登记该身份容器。
/// 入参：plan（UnpackPlan）被消费；password（&str）为包内加密数据库密码。
/// 返回值：完成验证、发布、登记及链接后返回 UnpackedContainer，含文件数、路径和展示名。
/// 异常：输入/目标冲突、密码/身份无效、包损坏、认证、I/O 或登记失败返回 WorkflowError。
/// 注意事项：同一稳定 ID 不生成新身份；目标不覆盖由 unpack_registered 保证。发布后登记失败可能保留已验证容器；该函数不把所有失败都回滚。
pub(crate) fn commit_unpack(
    mut plan: UnpackPlan,
    password: &str,
) -> Result<UnpackedContainer, WorkflowError> {
    check_unpack_paths(&plan)?;
    let (metadata, _) = VeilUnpacker::new(&plan.package_path).read_metadata(password)?;
    plan.veil_id = metadata.veil_id.clone();
    crate::work::validate_veil_id(&plan.veil_id)?;
    plan.directory = plan.work_root.join(&plan.veil_id);
    check_unpack_paths(&plan)?;
    let count = crate::package::unpack_registered(
        &plan.package_path,
        &plan.directory,
        &plan.veil_id,
        &plan.name,
        password,
        &mut plan.config,
        &plan.work_root,
        &plan.link_path,
    )?;
    Ok(UnpackedContainer {
        name: plan.name,
        link_path: plan.link_path,
        directory: plan.directory,
        file_count: count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ServiceError, ServiceRequest, ServiceResponse, VeilService};

    #[test]
    /// 验证创建计划会校验专用空间目录，并在实际提交前重新检查目标条件。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn create_requests_validate_and_recheck_dedicated_directory() {
        let temporary = tempfile::TempDir::new().unwrap();
        let work = temporary.path().join("dedicated");
        std::fs::create_dir(&work).unwrap();
        let link = temporary.path().join("test.veil-link");
        let link_text = link.to_str().unwrap();
        let service = VeilService::default();
        let options = || CreateOptions {
            target: "test",
            link_output: Some(link_text),
            work_name: None,
            work_root: Some(&work),
            dedicated: true,
            portable: false,
        };
        let plan = match service
            .execute(ServiceRequest::PrepareCreate { options: options() })
            .unwrap()
        {
            ServiceResponse::CreatePrepared(plan) => plan,
            _ => panic!("预期返回创建计划"),
        };
        std::fs::write(work.join("existing"), b"keep").unwrap();
        match service.execute(ServiceRequest::CommitCreate {
            plan,
            password: "password",
        }) {
            Err(ServiceError::Workflow(WorkflowError::DedicatedNotEmpty(path))) => {
                assert_eq!(path.canonicalize().unwrap(), work.canonicalize().unwrap())
            }
            Err(error) => panic!("错误类型不符：{error:?}"),
            Ok(_) => panic!("非空专属工作区不应允许创建"),
        }
        assert_eq!(std::fs::read(work.join("existing")).unwrap(), b"keep");
        assert!(!link.exists());
        assert!(matches!(
            service.execute(ServiceRequest::PrepareCreate { options: options() }),
            Err(ServiceError::Workflow(WorkflowError::DedicatedNotEmpty(_)))
        ));
        std::fs::remove_file(work.join("existing")).unwrap();
        let plan = match service
            .execute(ServiceRequest::PrepareCreate { options: options() })
            .unwrap()
        {
            ServiceResponse::CreatePrepared(plan) => plan,
            _ => panic!("预期返回创建计划"),
        };
        std::fs::write(&link, b"keep link").unwrap();
        assert!(
            matches!(service.execute(ServiceRequest::CommitCreate { plan, password: "password" }), Err(ServiceError::Workflow(WorkflowError::LinkExists(path))) if path == link)
        );
        assert_eq!(std::fs::read(&link).unwrap(), b"keep link");
    }

    #[test]
    /// 验证解包准备与提交之间目标目录发生变化时，服务会在写入前再次拒绝。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn unpack_request_rechecks_dedicated_directory_before_writing() {
        let temporary = tempfile::TempDir::new().unwrap();
        let work = temporary.path().join("dedicated");
        std::fs::create_dir(&work).unwrap();
        let package = temporary.path().join("package.veil");
        std::fs::write(&package, b"unchanged").unwrap();
        let link = temporary.path().join("test.veil-link");
        let id = generate_veil_id();
        let plan = UnpackPlan {
            config: AppConfig::load_from(&temporary.path().join("app.db")).unwrap(),
            package_path: package,
            directory: work.join(&id),
            veil_id: id,
            name: "test".into(),
            link_path: link.clone(),
            work_root: work.clone(),
            dedicated: true,
        };
        std::fs::write(work.join("existing"), b"keep").unwrap();
        let result = VeilService::default().execute(ServiceRequest::CommitUnpack {
            plan,
            password: "password",
        });
        assert!(
            matches!(result, Err(ServiceError::Workflow(WorkflowError::UnpackDirectoryExists(path))) if path == work)
        );
        assert_eq!(std::fs::read(work.join("existing")).unwrap(), b"keep");
        assert!(!link.exists());
    }

    #[test]
    /// 验证服务层独立执行输入校验，不依赖 CLI 层预先过滤非法请求。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn service_rejects_invalid_inputs_without_cli() {
        let service = VeilService::default();
        assert!(matches!(
            service.execute(ServiceRequest::PrepareCreate {
                options: CreateOptions {
                    target: "test",
                    link_output: None,
                    work_name: Some("default"),
                    work_root: Some(Path::new("work")),
                    dedicated: false,
                    portable: false,
                }
            }),
            Err(ServiceError::Workflow(
                WorkflowError::ConflictingWorkOptions
            ))
        ));
        assert!(matches!(
            service.execute(ServiceRequest::PrepareCreate {
                options: CreateOptions {
                    target: "test",
                    link_output: Some("invalid"),
                    work_name: None,
                    work_root: None,
                    dedicated: false,
                    portable: false,
                }
            }),
            Err(ServiceError::Workflow(WorkflowError::InvalidLinkExtension(
                _
            )))
        ));
        assert!(matches!(
            service.execute(ServiceRequest::ValidateNewPassword { password: "" }),
            Err(ServiceError::Workflow(WorkflowError::PasswordEmpty))
        ));
        let temporary = tempfile::TempDir::new().unwrap();
        let package = temporary.path().join("existing.veil");
        std::fs::write(&package, b"keep").unwrap();
        assert!(
            matches!(service.execute(ServiceRequest::PreparePack { name: "test", output: package.to_str() }), Err(ServiceError::Workflow(WorkflowError::PackOutputExists(path))) if path == package)
        );
        assert_eq!(std::fs::read(package).unwrap(), b"keep");
    }
}
