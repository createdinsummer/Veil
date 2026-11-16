//! CLI 统一错误码、模板参数和退出码。
//!
//! 命令层只负责产生带稳定错误码的结构化错误；面向用户的文本、参数替换和退出码
//! 由本模块统一渲染。已知 I/O、格式、加密等核心错误映射为稳定分类并保留来源；
//! 直接经From<anyhow::Error>转换的错误均进入Unexpected，不在该转换中重新识别内部类型；
//! 其他已知来源通过专门From映射，不应把所有底层失败都称为意外错误。
//!
//! 核心清单：
//! - `ErrorCode`：稳定错误码枚举（1xxx 参数、2xxx 密码、3xxx 文件、4xxx 配置、
//!   5xxx 初始化、6xxx 链接、7xxx 打包解包、8xxx 帮助、9xxx 底层错误）；
//! - `ErrorSpec`：每个错误码对应的机器名、i18n 消息键与进程退出码；
//! - `CliError`：错误码 + 命名模板参数，负责渲染用户可见文本；
//! - `CommandError`：命令层统一错误类型（业务错误 / 带来源错误 / 意外错误）；
//! - `cli_error!` / `cli_bail!`：快速构造与直接返回业务错误的宏。

use crate::i18n;
use std::collections::BTreeMap;
use std::fmt;

/// CLI 错误码的稳定数值区间。
///
/// 1xxx 参数错误，2xxx 密码错误，3xxx 文件操作错误，5xxx 初始化错误，
/// 6xxx 链接错误，7xxx 打包解包错误，8xxx 帮助错误。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    // ---- 1000-1999：命令行参数错误（默认退出码 2）----
    UnknownArgument = 1001,
    ArgumentConflict = 1002,
    ArgumentConflictSingle = 1003,
    InvalidArguments = 1004,
    MissingVeil = 1005,
    MissingInputPath = 1006,
    MissingDeletePath = 1007,
    MissingSourceDestination = 1008,
    MissingOutputPath = 1009,
    MissingCheckPath = 1010,

    // ---- 2000-2999：密码错误（默认退出码 1）----
    PasswordMismatch = 2001,
    PasswordEmpty = 2002,

    // ---- 3000-3999：文件添加错误 / 4000-4999：配置错误 ----
    AddSourceNotFound = 3001,
    AddNotFile = 3002,
    AddInvalidDestination = 3003,
    AddDirectoryReadFailed = 3004,
    ConfigInvalidLevel = 4001,

    // ---- 5000-5999：init 初始化错误 ----
    InitConflictingWorkOptions = 5001,
    InitDedicatedRequiresWorkPath = 5002,
    InitPortableConflictsWork = 5003,
    InitInvalidLinkName = 5004,
    InitInvalidVeilName = 5005,
    InitInvalidLinkExtension = 5006,
    InitVeilExists = 5008,
    InitDedicatedPathNotEmpty = 5009,
    InitLinkRegistrationFailed = 5010,
    InitWorkCreateFailed = 5011,

    // ---- 6000-6999：链接错误 ----
    LinkOutputExists = 6001,
    LinkMissingVeilId = 6002,
    LinkInvalidExtension = 6003,

    // ---- 7000-7999：导出、打包和解包错误 ----
    ExportPathRequired = 7001,
    PackOutputExists = 7002,
    UnpackVeilNotFound = 7003,
    UnpackNameExtractFailed = 7004,
    UnpackMissingVeilId = 7006,
    UnpackDirectoryExists = 7007,
    UnpackInvalidLinkExtension = 7010,

    // ---- 8000-8999：帮助命令错误 ----
    HelpUnknownTopic = 8001,
    // ---- 9000-9999：底层核心错误和未分类异常 ----
    Unexpected = 9000,
    Io = 9001,
    Format = 9005,
    Work = 9006,
    Config = 9007,
    VeilNotFound = 9008,
    InvalidFormat = 9009,
    Serialization = 9010,
    Encryption = 9011,
    Decryption = 9012,
    KeyDerivation = 9013,
    FileNotFound = 9014,
    FileAlreadyExists = 9015,
    VolumeUnavailable = 9016,
    VeilIdConflict = 9017,
    VeilNameAmbiguous = 9018,
}

/// 错误码对应的模板和进程退出码。
///
/// 每个错误码对应一条 `ErrorSpec`：`value` 用于输出前缀（如 `[1002]`），
/// `name` 是写入日志的稳定机器名，`template_key` 指向 i18n 消息表里的模板，
/// `exit_code` 决定进程退出状态。四条信息集中在这里，命令层与日志层引用同一来源。
pub struct ErrorSpec {
    /// 稳定错误码数值。
    pub value: u32,
    /// 稳定的机器可读名称。
    pub name: &'static str,
    /// i18n 消息键。
    pub template_key: &'static str,
    /// 进程退出码。
    pub exit_code: u8,
}

impl ErrorCode {
    /// 取得错误分类对应的稳定数字、机器名、消息键和进程退出码。
    /// 入参：self（ErrorCode）为已知枚举分类。
    /// 返回值：ErrorSpec，参数类1xxx退出2，其余按映射一般退出1。
    /// 异常：无错误返回。
    /// 注意事项：纯const映射，不写日志、不验证错误产生条件，也不用于说明文件是否已回滚。
    pub const fn spec(self) -> ErrorSpec {
        // 每个分支只做"错误码 → 规范"的静态映射，全部在编译期可展开。
        let value = self as u32;
        match self {
            Self::UnknownArgument => {
                Self::define(value, "UNKNOWN_ARGUMENT", "error.unknown_argument", 2)
            }
            Self::ArgumentConflict => {
                Self::define(value, "ARGUMENT_CONFLICT", "error.argument_conflict", 2)
            }
            Self::ArgumentConflictSingle => Self::define(
                value,
                "ARGUMENT_CONFLICT_SINGLE",
                "error.argument_conflict_single",
                2,
            ),
            Self::InvalidArguments => {
                Self::define(value, "INVALID_ARGUMENTS", "error.invalid_arguments", 2)
            }
            Self::MissingVeil => Self::define(value, "MISSING_CONTAINER", "error.require_veil", 2),
            Self::MissingInputPath => {
                Self::define(value, "MISSING_INPUT_PATH", "error.require_input_path", 2)
            }
            Self::MissingDeletePath => {
                Self::define(value, "MISSING_DELETE_PATH", "error.require_delete_path", 2)
            }
            Self::MissingSourceDestination => Self::define(
                value,
                "MISSING_SOURCE_DESTINATION",
                "error.require_src_dst",
                2,
            ),
            Self::MissingOutputPath => {
                Self::define(value, "MISSING_OUTPUT_PATH", "error.require_output_path", 2)
            }
            Self::MissingCheckPath => {
                Self::define(value, "MISSING_CHECK_PATH", "error.require_check_path", 2)
            }
            Self::PasswordMismatch => {
                Self::define(value, "PASSWORD_MISMATCH", "error.password_mismatch", 1)
            }
            Self::PasswordEmpty => Self::define(value, "PASSWORD_EMPTY", "error.password_empty", 1),
            Self::AddSourceNotFound => {
                Self::define(value, "ADD_SOURCE_NOT_FOUND", "add.source_not_found", 1)
            }
            Self::AddNotFile => Self::define(value, "ADD_NOT_FILE", "add.not_file", 1),
            Self::AddInvalidDestination => Self::define(
                value,
                "ADD_INVALID_DESTINATION",
                "add.invalid_destination",
                1,
            ),
            Self::AddDirectoryReadFailed => Self::define(
                value,
                "ADD_DIRECTORY_READ_FAILED",
                "add.directory_read_failed",
                1,
            ),
            Self::ConfigInvalidLevel => {
                Self::define(value, "CONFIG_INVALID_LEVEL", "config.invalid_level", 1)
            }
            Self::InitConflictingWorkOptions => Self::define(
                value,
                "INIT_CONFLICTING_WORKSPACE_OPTIONS",
                "init.conflicting_work_options",
                1,
            ),
            Self::InitDedicatedRequiresWorkPath => Self::define(
                value,
                "INIT_DEDICATED_REQUIRES_WORKSPACE_PATH",
                "init.dedicated_requires_work_root",
                1,
            ),
            Self::InitPortableConflictsWork => Self::define(
                value,
                "INIT_PORTABLE_CONFLICTS_WORKSPACE",
                "init.portable_conflicts_work",
                1,
            ),
            Self::InitInvalidLinkName => {
                Self::define(value, "INIT_INVALID_LINK_NAME", "init.invalid_link_name", 1)
            }
            Self::InitInvalidVeilName => Self::define(
                value,
                "INIT_INVALID_CONTAINER_NAME",
                "init.invalid_veil_name",
                1,
            ),
            Self::InitInvalidLinkExtension => Self::define(
                value,
                "INIT_INVALID_LINK_EXTENSION",
                "init.invalid_link_extension",
                1,
            ),
            Self::InitVeilExists => Self::define(value, "INIT_CONTAINER_EXISTS", "init.exists", 1),
            Self::InitDedicatedPathNotEmpty => Self::define(
                value,
                "INIT_DEDICATED_PATH_NOT_EMPTY",
                "init.dedicated_path_not_empty",
                1,
            ),
            Self::InitLinkRegistrationFailed => Self::define(
                value,
                "INIT_LINK_REGISTRATION_FAILED",
                "init.link_registration_failed",
                1,
            ),
            Self::InitWorkCreateFailed => Self::define(
                value,
                "INIT_WORKSPACE_CREATE_FAILED",
                "init.work_create_failed",
                1,
            ),
            Self::LinkOutputExists => {
                Self::define(value, "LINK_OUTPUT_EXISTS", "link.output_exists", 1)
            }
            Self::LinkMissingVeilId => {
                Self::define(value, "LINK_MISSING_VEIL_ID", "link.missing_veil_id", 1)
            }
            Self::LinkInvalidExtension => {
                Self::define(value, "LINK_INVALID_EXTENSION", "link.invalid_extension", 1)
            }
            Self::ExportPathRequired => {
                Self::define(value, "EXPORT_PATH_REQUIRED", "ex.specify_path", 1)
            }
            Self::PackOutputExists => {
                Self::define(value, "PACK_OUTPUT_EXISTS", "pack.output_exists", 1)
            }
            Self::UnpackVeilNotFound => Self::define(
                value,
                "UNPACK_CONTAINER_NOT_FOUND",
                "unpack.veil_not_found",
                1,
            ),
            Self::UnpackNameExtractFailed => Self::define(
                value,
                "UNPACK_NAME_EXTRACT_FAILED",
                "unpack.name_extract_failed",
                1,
            ),
            Self::UnpackMissingVeilId => {
                Self::define(value, "UNPACK_MISSING_VEIL_ID", "unpack.missing_veil_id", 1)
            }
            Self::UnpackDirectoryExists => Self::define(
                value,
                "UNPACK_DIRECTORY_EXISTS",
                "unpack.directory_exists",
                1,
            ),
            Self::UnpackInvalidLinkExtension => Self::define(
                value,
                "UNPACK_INVALID_LINK_EXTENSION",
                "init.invalid_link_extension",
                1,
            ),
            Self::HelpUnknownTopic => {
                Self::define(value, "HELP_UNKNOWN_TOPIC", "help.files.unknown_topic", 1)
            }
            Self::Unexpected => Self::define(value, "UNEXPECTED", "error.unexpected", 1),
            Self::Io => Self::define(value, "IO", "error.core_io", 1),
            Self::Format => Self::define(value, "FORMAT", "error.core_format", 1),
            Self::Work => Self::define(value, "WORKSPACE", "error.core_work", 1),
            Self::Config => Self::define(value, "CONFIG", "error.core_config", 1),
            Self::VeilNotFound => {
                Self::define(value, "CONTAINER_NOT_FOUND", "error.core_veil_not_found", 1)
            }
            Self::InvalidFormat => {
                Self::define(value, "INVALID_FORMAT", "error.core_invalid_format", 1)
            }
            Self::Serialization => {
                Self::define(value, "SERIALIZATION", "error.core_serialization", 1)
            }
            Self::Encryption => Self::define(value, "ENCRYPTION", "error.core_encryption", 1),
            Self::Decryption => Self::define(value, "DECRYPTION", "error.core_decryption", 1),
            Self::KeyDerivation => {
                Self::define(value, "KEY_DERIVATION", "error.core_key_derivation", 1)
            }
            Self::FileNotFound => {
                Self::define(value, "FILE_NOT_FOUND", "error.core_file_not_found", 1)
            }
            Self::FileAlreadyExists => Self::define(
                value,
                "FILE_ALREADY_EXISTS",
                "error.core_file_already_exists",
                1,
            ),
            Self::VolumeUnavailable => Self::define(
                value,
                "VOLUME_UNAVAILABLE",
                "error.core_volume_unavailable",
                1,
            ),
            Self::VeilIdConflict => Self::define(
                value,
                "CONTAINER_ID_CONFLICT",
                "error.core_veil_id_conflict",
                1,
            ),
            Self::VeilNameAmbiguous => Self::define(
                value,
                "CONTAINER_NAME_AMBIGUOUS",
                "error.core_veil_name_ambiguous",
                1,
            ),
        }
    }

    /// 集中构造一条错误分类定义，供spec静态映射复用。
    /// 入参：value（u32）为显示数字；name（&'static str）为日志机器名；template_key为本地化键；exit_code（u8）为进程状态。
    /// 返回值：ErrorSpec，原样保存四项。
    /// 异常：无错误返回。
    /// 注意事项：不校验数字/键唯一或模板是否存在，维护者须保持映射一致。
    const fn define(
        value: u32,
        name: &'static str,
        template_key: &'static str,
        exit_code: u8,
    ) -> ErrorSpec {
        ErrorSpec {
            value,
            name,
            template_key,
            exit_code,
        }
    }
}

/// 带错误码和命名模板参数的业务错误。
///
/// 命令层产生 `CliError` 时只填错误码和少量命名参数（如 `path`、`arg`），
/// 真正的用户文案由 `render` 按当前语言组装，因此错误文本可以随时本地化。
#[derive(Clone)]
pub struct CliError {
    /// 稳定的错误分类码，决定消息模板与退出码。
    code: ErrorCode,
    /// 命名模板参数（键如 `path`/`arg`/`prior`，值为用户可见文本）；
    /// 用 BTreeMap 保存，保证多条参数渲染时顺序稳定可复现。
    params: BTreeMap<&'static str, String>,
    /// 上游要求抑制CLI重复记录时为true，不保证日志已持久化；render仍可显示正文。
    reported: bool,
}

impl CliError {
    /// 构造尚未上报、没有模板参数的业务错误。
    /// 入参：code（ErrorCode）为业务分类。
    /// 返回值：CliError，params为空BTreeMap，reported=false。
    /// 异常：无错误返回。
    /// 注意事项：只构造，不渲染、不记录日志。
    pub fn coded(code: ErrorCode) -> Self {
        Self {
            code,
            params: BTreeMap::new(),
            reported: false,
        }
    }

    /// 借用业务错误取得稳定分类。
    /// 入参：self（&CliError）为结构化错误。
    /// 返回值：ErrorCode副本。
    /// 异常：无错误返回。
    /// 注意事项：不读取模板或记录日志。
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    /// 按当前语言渲染业务模板参数和统一错误码前缀。
    /// 入参：self（&CliError）为错误分类及params映射。
    /// 返回值：String为用户可见文案。
    /// 异常：无Result错误返回。
    /// 注意事项：不写日志、不脱敏用户可见参数；本地化缺键回退键名，参数顺序按BTreeMap键序。
    pub fn render(&self) -> String {
        // 先查消息模板并替换参数，再加上统一错误码前缀。
        let message = i18n::render(self.code.spec().template_key, &self.params);
        render_with_code(self.code.spec().value, &message)
    }
}

/// 命令层统一错误类型。
///
/// 三种形态对应三种来源：`Coded` 是命令层自己产生的预期业务错误；`Sourced` 是
/// 包装了底层原因的预期错误（用户看到稳定文案，日志保留原始错误链）；`Unexpected`
/// 是没有稳定业务分类的意外错误，直接展示底层错误文本。
pub enum CommandError {
    /// 具有稳定错误码和模板参数的预期错误。
    Coded(CliError),
    /// 保留用户提示与原始错误，直到最终处理边界再记录。
    Sourced {
        /// 对外分类与模板参数，render和exit_code从这里取规则。
        display: CliError,
        /// 原始错误链，report按其类型分级并交日志入口；Debug也可能显示。
        source: anyhow::Error,
    },
    /// 无稳定业务分类的底层错误。
    Unexpected(anyhow::Error),
}

/// CLI 统一结果类型。
pub type Result<T> = std::result::Result<T, CommandError>;

impl CommandError {
    /// 构造命令边界使用的Coded业务错误。
    /// 入参：code（ErrorCode）为稳定分类。
    /// 返回值：CommandError::Coded，内部空参数且未上报。
    /// 异常：无错误返回。
    /// 注意事项：构造不记录日志；后续param补充模板值。
    pub fn coded(code: ErrorCode) -> Self {
        Self::Coded(CliError::coded(code))
    }

    /// 为可本地化错误补充一份命名模板参数。
    /// 入参：self按值消费；name（&'static str）为占位符名；value（impl ToString）为显示值。
    /// 返回值：更新后的CommandError；同名覆盖，Unexpected原样返回。
    /// 异常：无Result返回，用户自定义ToString可panic。
    /// 注意事项：值不自动脱敏，可能出现在用户提示和Debug；不会修改原始source或reported。
    pub fn param(mut self, name: &'static str, value: impl ToString) -> Self {
        // 两种带模板的形态都往展示错误里塞参数，意外错误静默忽略。
        if let Self::Coded(error) | Self::Sourced { display: error, .. } = &mut self {
            error.params.insert(name, value.to_string());
        }
        self
    }

    /// 将任一命令错误统一映射到稳定分类。
    /// 入参：self（&CommandError）为错误。
    /// 返回值：Coded/Sourced取展示分类；Unexpected返回ErrorCode::Unexpected（9000）。
    /// 异常：无错误返回。
    /// 注意事项：意外错误虽无细分业务分类，仍有固定9000码。
    pub const fn code(&self) -> ErrorCode {
        match self {
            // 带来源的错误对外展示包装后的稳定分类码。
            Self::Coded(error) => error.code(),
            Self::Sourced { display, .. } => display.code(),
            Self::Unexpected(_) => ErrorCode::Unexpected,
        }
    }

    /// 为命令失败选择进程退出状态。
    /// 入参：self为命令错误。
    /// 返回值：u8，来自code().spec().exit_code，参数错误2、其他映射通常1。
    /// 异常：无错误返回。
    /// 注意事项：只读取规则，不结束进程；调用方另行process::exit。
    pub fn exit_code(&self) -> u8 {
        self.code().spec().exit_code
    }

    /// 在需要时记录失败原因，并返回本地化用户文案。
    /// 入参：self为待报告错误，Coded/Sourced内reported决定是否略过本次日志。
    /// 返回值：String，同render用户显示结果。
    /// 异常：不通过Result返回日志失败。
    /// 注意事项：非纯渲染：不自动把reported置true，多次调用可重复日志。Coded只记固定标签；Sourced/Unexpected记录来源链，来源正文仍可能含用户路径，需底层脱敏。
    pub fn report(&self) -> String {
        // 服务边界已经记录过原始原因的错误，这里只渲染文本。
        if matches!(
            self,
            Self::Coded(CliError { reported: true, .. })
                | Self::Sourced {
                    display: CliError { reported: true, .. },
                    ..
                }
        ) {
            return self.render();
        }
        match self {
            // 意外错误没有业务分类，按 9000 码记录完整错误链。
            Self::Unexpected(error) => veil_core::logging::error_with_code(
                "cli",
                "command",
                self.code().spec().name,
                error.as_ref(),
            ),
            Self::Sourced { source, .. } => {
                use veil_core::{
                    error::VeilError,
                    logging::{Level, failure},
                };
                // 向下转型成核心错误类型，按"是否可预期"选择日志级别：
                // 用户取消、锁冲突、找不到对象等属于正常业务结果，不按故障记录。
                let level = match source.downcast_ref::<VeilError>() {
                    Some(
                        VeilError::Cancelled
                        | VeilError::LockConflict { .. }
                        | VeilError::VeilNotFound(_)
                        | VeilError::VeilNameAmbiguous(_)
                        | VeilError::FileNotFound(_)
                        | VeilError::FileAlreadyExists(_)
                        | VeilError::VersionConflict(_)
                        | VeilError::VolumeUnavailable(_),
                    ) => Level::Info,
                    // 格式损坏或解密失败可能暗示数据问题，按警告级别提示。
                    Some(VeilError::InvalidFormat(_) | VeilError::DecryptionError(_)) => {
                        Level::Warn
                    }
                    // 其余核心错误（I/O、加密、序列化等）视为真实故障。
                    _ => Level::Error,
                };
                failure(
                    level,
                    "cli",
                    "command",
                    self.code().spec().name,
                    source.as_ref(),
                    &[],
                );
            }
            // 参数校验和可预期冲突不进入默认生产日志。
            Self::Coded(_) => veil_core::logging::record(
                if matches!(
                    self.code(),
                    // 只有这三类"落盘/登记失败"才按错误级别记录，
                    // 其余参数校验类问题只按信息级别留痕。
                    ErrorCode::AddDirectoryReadFailed
                        | ErrorCode::InitLinkRegistrationFailed
                        | ErrorCode::InitWorkCreateFailed
                ) {
                    veil_core::logging::Level::Error
                } else {
                    veil_core::logging::Level::Info
                },
                "cli",
                "command",
                self.code().spec().name,
            ),
        }
        self.render()
    }

    /// 告诉CLI边界上游已处理诊断，本地不再重复写错误链。
    /// 入参：self按值消费。
    /// 返回值：Coded/Sourced将reported=true，Unexpected不变。
    /// 异常：无错误返回。
    /// 注意事项：不检查日志是否真写出、不实际记录；对Unexpected多次report仍可能重复记录。
    pub(crate) fn already_reported(mut self) -> Self {
        // 标记展示错误"已上报"；意外错误无模板，不需要标记。
        if let Self::Coded(display) | Self::Sourced { display, .. } = &mut self {
            display.reported = true;
        }
        self
    }

    /// 仅生成用户错误提示，不附带日志写入。
    /// 入参：self为任一CommandError。
    /// 返回值：String，Coded/Sourced渲染模板，Unexpected用原错误文本加9000前缀。
    /// 异常：无Result错误返回。
    /// 注意事项：模板参数可能包含底层详情，不能宣称Sourced展示完全隐藏来源文本；与report相比此入口不写日志。
    pub fn render(&self) -> String {
        match self {
            // 使用包装模板；其中error参数仍可能保存来源详情，不直接打印整条链。
            Self::Coded(error) => error.render(),
            Self::Sourced { display, .. } => display.render(),
            // 意外错误直接展示底层文本，并加上 9000 错误码前缀。
            Self::Unexpected(error) => {
                render_with_code(ErrorCode::Unexpected.spec().value, &error.to_string())
            }
        }
    }
}

/// 给错误消息附加当前语言的数字前缀。
/// 入参：code（u32）为稳定数字；message（&str）为已渲染正文。
/// 返回值：String，通过error.display模板替换code和message。
/// 异常：无错误返回。
/// 注意事项：不转义正文、不写日志；前缀格式由本地化资源决定。
fn render_with_code(code: u32, message: &str) -> String {
    i18n::t2(
        "error.display",
        "code",
        &code.to_string(),
        "message",
        message,
    )
}

/// 业务错误直接作为预期错误包装，保留其错误码与模板参数。
impl From<CliError> for CommandError {
    /// 功能：将已有稳定错误码的业务失败交给命令统一渲染
    /// 入参：error: CliError，本次操作失败原因，转换时取得所有权。
    /// 返回值：CommandError::Coded，保留原业务错误与参数。
    /// 异常：不通过 Result 再次返回错误，不执行失败业务操作。
    /// 注意事项：不重新分类，不丢失 reported 状态。
    fn from(error: CliError) -> Self {
        Self::Coded(error)
    }
}

/// 无稳定业务分类的 anyhow 错误按意外错误处理（9000）。
impl From<anyhow::Error> for CommandError {
    /// 功能：包装没有稳定业务分类的意外错误
    /// 入参：error: anyhow::Error，本次操作失败原因，转换时取得所有权。
    /// 返回值：CommandError::Unexpected，保留原始错误链。
    /// 异常：不通过 Result 再次返回错误，不执行失败业务操作。
    /// 注意事项：最终展示使用通用意外错误分类，不在转换阶段记录日志。
    fn from(error: anyhow::Error) -> Self {
        Self::Unexpected(error)
    }
}

/// I/O 错误保留原始错误链作为来源，同时给出稳定 Io 错误码（9001）。
impl From<std::io::Error> for CommandError {
    /// 功能：把文件或设备访问失败转为稳定 I/O 错误分类
    /// 入参：error: std::io::Error，本次操作失败原因，转换时取得所有权。
    /// 返回值：CommandError::Sourced，包含 Io 展示错误与原始来源。
    /// 异常：不通过 Result 再次返回错误，不执行失败业务操作。
    /// 注意事项：error 模板参数保存系统描述，原始来源另行保留。
    fn from(error: std::io::Error) -> Self {
        let mut display = CliError::coded(ErrorCode::Io);
        // 原始错误文本放进模板参数，用户能看到具体失败原因。
        display.params.insert("error", error.to_string());
        Self::Sourced {
            display,
            source: error.into(),
        }
    }
}

/// 把核心层错误映射为带稳定错误码的命令错误，原始错误保留为来源。
///
/// 每个核心错误变体都映射到一个 9xxx 稳定码（如加密 → 9011、解密 → 9012），
/// 个别业务语义（取消、锁冲突、版本冲突）归入通用工作区错误并附中文说明。
impl From<veil_core::error::VeilError> for CommandError {
    /// 功能：把核心层失败映射为 CLI 可展示的稳定错误码
    /// 入参：error: veil_core::error::VeilError，本次操作失败原因，转换时取得所有权。
    /// 返回值：带映射后展示分类与原始来源的 CommandError。
    /// 异常：不通过 Result 再次返回错误，不执行失败业务操作。
    /// 注意事项：取消、锁冲突与版本冲突按业务规则分类；本函数不会重试失败操作。
    fn from(error: veil_core::error::VeilError) -> Self {
        use veil_core::error::VeilError;

        // 映射表：返回（稳定错误码，放进模板参数的用户可见详情文本）。
        let (code, detail) = match &error {
            // 以下三个场景补充更贴合业务的中文说明，不直接透传英文错误文本。
            VeilError::Cancelled => (ErrorCode::Work, "操作已中止".into()),
            VeilError::LockConflict { level, scope } => (
                ErrorCode::Work,
                format!("锁冲突：级别 {level}，范围 {scope}"),
            ),
            VeilError::ImportBatchFailed { file, source } => {
                (ErrorCode::Work, format!("导入文件 {file} 失败: {source}"))
            }
            // 数据库与配置类错误归入 9xxx 配置错误。
            VeilError::Database(error) => (ErrorCode::Config, error.to_string()),
            VeilError::Io(error) => (ErrorCode::Io, error.to_string()),
            VeilError::CommitUncertain(error) => (
                ErrorCode::Io,
                format!("提交结果或持久化状态不确定: {error}"),
            ),
            VeilError::Format(error) => (ErrorCode::Format, error.clone()),
            VeilError::WorkError(error) => (ErrorCode::Work, error.clone()),
            VeilError::ConfigError(error) | VeilError::DatabasePool(error) => {
                (ErrorCode::Config, error.clone())
            }
            // 容器、格式与序列化错误直接透传核心分类。
            VeilError::VeilNotFound(error) => (ErrorCode::VeilNotFound, error.clone()),
            VeilError::InvalidFormat(error) => (ErrorCode::InvalidFormat, error.clone()),
            VeilError::SerializationError(error) => (ErrorCode::Serialization, error.clone()),
            VeilError::EncryptionError(error) => (ErrorCode::Encryption, error.clone()),
            VeilError::DecryptionError(error) => (ErrorCode::Decryption, error.clone()),
            VeilError::KeyDerivationError(error) => (ErrorCode::KeyDerivation, error.clone()),
            VeilError::FileNotFound(error) => (ErrorCode::FileNotFound, error.clone()),
            VeilError::FileAlreadyExists(error) => (ErrorCode::FileAlreadyExists, error.clone()),
            VeilError::VersionConflict(error) => {
                (ErrorCode::Work, format!("文件版本冲突: {error}"))
            }
            VeilError::VolumeUnavailable(error) => (ErrorCode::VolumeUnavailable, error.clone()),
            VeilError::VeilIdConflict(error) => (ErrorCode::VeilIdConflict, error.clone()),
            VeilError::VeilNameAmbiguous(error) => (ErrorCode::VeilNameAmbiguous, error.clone()),
        };

        // 组装带来源的显示错误：用户看到稳定文案，日志保留完整原始错误链。
        let mut display = CliError::coded(code);
        display.params.insert("error", detail);
        Self::Sourced {
            display,
            source: error.into(),
        }
    }
}

/// 以稳定错误码渲染错误文本（`Display` 与 `Debug` 共用同一渲染结果）。
impl fmt::Display for CommandError {
    /// 功能：向终端输出统一渲染后的错误文案
    /// 入参：self 为待展示错误；formatter: &mut fmt::Formatter 为调用方输出器。
    /// 返回值：写入 render() 结果成功时为 Ok(())。
    /// 异常：输出端写入失败返回 fmt::Error。
    /// 注意事项：使用当前本地化模板；格式化不改变原错误及退出码。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.render())
    }
}

/// 调试输出包含错误码数值、机器名和模板参数，便于排障；参数可能含用户文案，仍须脱敏。
impl fmt::Debug for CliError {
    /// 功能：为排障输出稳定错误码、机器名及模板参数
    /// 入参：self 为待展示错误；formatter: &mut fmt::Formatter 为调用方输出器。
    /// 返回值：成功写出 code/name/params 后为 Ok(())。
    /// 异常：输出端写入失败返回 fmt::Error。
    /// 注意事项：params 可能包含用户输入，不能把此格式直接视为脱敏结果。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let spec = self.code.spec();
        formatter
            .debug_struct("CliError")
            .field("code", &spec.value)
            .field("name", &spec.name)
            .field("params", &self.params)
            .finish()
    }
}

/// 调试输出区分三种形态：业务错误沿用其调试格式，带来源错误展示包装与原始错误。
impl fmt::Debug for CommandError {
    /// 功能：区分业务错误、带来源错误与意外错误的调试信息
    /// 入参：self 为待展示错误；formatter: &mut fmt::Formatter 为调用方输出器。
    /// 返回值：成功写出对应变体内容后为 Ok(())。
    /// 异常：输出端写入失败返回 fmt::Error。
    /// 注意事项：来源错误可能含路径或其他业务文本；日志仍应经过统一脱敏边界。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Coded(error) => error.fmt(formatter),
            Self::Sourced { display, source } => formatter
                .debug_struct("Sourced")
                .field("display", display)
                .field("source", source)
                .finish(),
            Self::Unexpected(error) => formatter.debug_tuple("Unexpected").field(error).finish(),
        }
    }
}

/// 返回一个带错误码的 `CommandError`。
///
/// 第一个分支只填错误码；第二个分支按 `"参数名" => 值` 依次追加模板参数，
/// 供错误文案里的 `{参数名}` 占位符替换。
#[macro_export]
macro_rules! cli_error {
    // 仅错误码的用法：cli_error!(MissingVeil)
    ($code:ident) => {
        $crate::error::CommandError::coded($crate::error::ErrorCode::$code)
    };
    // 带参数的用法：cli_error!(ArgumentConflict, "arg" => a, "prior" => b)
    ($code:ident, $($name:literal => $value:expr),+ $(,)?) => {
        $crate::error::CommandError::coded($crate::error::ErrorCode::$code)
            $(.param($name, $value))+
    };
}

/// 直接返回一个带错误码的 `CommandError`。
///
/// 等价于 `return Err(cli_error!(...))`，把"构造并返回"压缩成一步。
#[macro_export]
macro_rules! cli_bail {
    ($($args:tt)*) => {
        return Err($crate::cli_error!($($args)*))
    };
}
