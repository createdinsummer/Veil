//! CLI 统一错误码、模板参数和退出码。
//!
//! 命令层只负责产生带稳定错误码的结构化错误；面向用户的文本、参数替换和退出码
//! 由本模块统一渲染。底层 I/O、格式和加密错误保留为意外错误，避免把实现细节
//! 伪装成可稳定处理的业务错误。

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
    UnknownArgument = 1001,
    ArgumentConflict = 1002,
    ArgumentConflictSingle = 1003,
    InvalidArguments = 1004,
    MissingContainer = 1005,
    MissingInputPath = 1006,
    MissingDeletePath = 1007,
    MissingSourceDestination = 1008,
    MissingOutputPath = 1009,
    MissingCheckPath = 1010,

    PasswordMismatch = 2001,
    PasswordEmpty = 2002,

    AddSourceNotFound = 3001,
    AddNotFile = 3002,
    AddInvalidDestination = 3003,
    AddDirectoryReadFailed = 3004,
    ConfigInvalidLevel = 4001,

    InitConflictingWorkspaceOptions = 5001,
    InitDedicatedRequiresWorkspacePath = 5002,
    InitPortableConflictsWorkspace = 5003,
    InitInvalidLinkName = 5004,
    InitInvalidContainerName = 5005,
    InitInvalidLinkExtension = 5006,
    InitWorkspaceNotFound = 5007,
    InitContainerExists = 5008,
    InitDedicatedPathNotEmpty = 5009,
    InitLinkRegistrationFailed = 5010,
    InitWorkspaceCreateFailed = 5011,

    LinkOutputExists = 6001,
    LinkMissingVeilId = 6002,
    LinkInvalidExtension = 6003,

    ExportPathRequired = 7001,
    PackOutputExists = 7002,
    UnpackContainerNotFound = 7003,
    UnpackNameExtractFailed = 7004,
    UnpackWorkspaceNotFound = 7005,
    UnpackMissingVeilId = 7006,
    UnpackDirectoryExists = 7007,
    UnpackInvalidLinkExtension = 7010,

    HelpUnknownTopic = 8001,
    Unexpected = 9000,
    Io = 9001,
    Encrypt = 9002,
    Decrypt = 9003,
    Serialize = 9004,
    Format = 9005,
    Workspace = 9006,
    Config = 9007,
    ContainerNotFound = 9008,
    InvalidFormat = 9009,
    Serialization = 9010,
    Encryption = 9011,
    Decryption = 9012,
    KeyDerivation = 9013,
    FileNotFound = 9014,
    FileAlreadyExists = 9015,
    VolumeUnavailable = 9016,
}

/// 错误码对应的模板和进程退出码。
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
    /// 返回错误码的完整定义。
    pub const fn spec(self) -> ErrorSpec {
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
            Self::MissingContainer => {
                Self::define(value, "MISSING_CONTAINER", "error.require_container", 2)
            }
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
            Self::InitConflictingWorkspaceOptions => Self::define(
                value,
                "INIT_CONFLICTING_WORKSPACE_OPTIONS",
                "init.conflicting_workspace_options",
                1,
            ),
            Self::InitDedicatedRequiresWorkspacePath => Self::define(
                value,
                "INIT_DEDICATED_REQUIRES_WORKSPACE_PATH",
                "init.dedicated_requires_workspace_path",
                1,
            ),
            Self::InitPortableConflictsWorkspace => Self::define(
                value,
                "INIT_PORTABLE_CONFLICTS_WORKSPACE",
                "init.portable_conflicts_workspace",
                1,
            ),
            Self::InitInvalidLinkName => {
                Self::define(value, "INIT_INVALID_LINK_NAME", "init.invalid_link_name", 1)
            }
            Self::InitInvalidContainerName => Self::define(
                value,
                "INIT_INVALID_CONTAINER_NAME",
                "init.invalid_container_name",
                1,
            ),
            Self::InitInvalidLinkExtension => Self::define(
                value,
                "INIT_INVALID_LINK_EXTENSION",
                "init.invalid_link_extension",
                1,
            ),
            Self::InitWorkspaceNotFound => Self::define(
                value,
                "INIT_WORKSPACE_NOT_FOUND",
                "init.workspace_not_found",
                1,
            ),
            Self::InitContainerExists => {
                Self::define(value, "INIT_CONTAINER_EXISTS", "init.exists", 1)
            }
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
            Self::InitWorkspaceCreateFailed => Self::define(
                value,
                "INIT_WORKSPACE_CREATE_FAILED",
                "init.workspace_create_failed",
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
            Self::UnpackContainerNotFound => Self::define(
                value,
                "UNPACK_CONTAINER_NOT_FOUND",
                "unpack.container_not_found",
                1,
            ),
            Self::UnpackNameExtractFailed => Self::define(
                value,
                "UNPACK_NAME_EXTRACT_FAILED",
                "unpack.name_extract_failed",
                1,
            ),
            Self::UnpackWorkspaceNotFound => Self::define(
                value,
                "UNPACK_WORKSPACE_NOT_FOUND",
                "unpack.workspace_not_found",
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
            Self::Encrypt => Self::define(value, "ENCRYPT", "error.core_encrypt", 1),
            Self::Decrypt => Self::define(value, "DECRYPT", "error.core_decrypt", 1),
            Self::Serialize => Self::define(value, "SERIALIZE", "error.core_serialize", 1),
            Self::Format => Self::define(value, "FORMAT", "error.core_format", 1),
            Self::Workspace => Self::define(value, "WORKSPACE", "error.core_workspace", 1),
            Self::Config => Self::define(value, "CONFIG", "error.core_config", 1),
            Self::ContainerNotFound => Self::define(
                value,
                "CONTAINER_NOT_FOUND",
                "error.core_container_not_found",
                1,
            ),
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
        }
    }

    /// 构造一条错误码定义。
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
#[derive(Clone)]
pub struct CliError {
    code: ErrorCode,
    params: BTreeMap<&'static str, String>,
}

impl CliError {
    /// 创建指定错误码的业务错误。
    pub fn coded(code: ErrorCode) -> Self {
        Self {
            code,
            params: BTreeMap::new(),
        }
    }

    /// 返回错误码。
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    /// 按当前语言渲染用户消息。
    pub fn render(&self) -> String {
        let message = i18n::render(self.code.spec().template_key, &self.params);
        render_with_code(self.code.spec().value, &message)
    }
}

/// 命令层统一错误类型。
pub enum CommandError {
    /// 具有稳定错误码和模板参数的预期错误。
    Coded(CliError),
    /// 无稳定业务分类的底层错误。
    Unexpected(anyhow::Error),
}

/// CLI 统一结果类型。
pub type Result<T> = std::result::Result<T, CommandError>;

impl CommandError {
    /// 创建业务错误。
    pub fn coded(code: ErrorCode) -> Self {
        Self::Coded(CliError::coded(code))
    }

    /// 添加模板参数。
    pub fn param(mut self, name: &'static str, value: impl ToString) -> Self {
        if let Self::Coded(error) = &mut self {
            error.params.insert(name, value.to_string());
        }
        self
    }

    /// 返回稳定错误码；意外错误没有公开错误码。
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::Coded(error) => error.code(),
            Self::Unexpected(_) => ErrorCode::Unexpected,
        }
    }

    /// 返回进程退出码。
    pub fn exit_code(&self) -> u8 {
        self.code().spec().exit_code
    }

    /// 渲染用户可见错误文本。
    pub fn render(&self) -> String {
        match self {
            Self::Coded(error) => error.render(),
            Self::Unexpected(error) => {
                render_with_code(ErrorCode::Unexpected.spec().value, &error.to_string())
            }
        }
    }
}

/// 为用户消息加上稳定错误码前缀。
fn render_with_code(code: u32, message: &str) -> String {
    i18n::t2(
        "error.display",
        "code",
        &code.to_string(),
        "message",
        message,
    )
}

impl From<CliError> for CommandError {
    fn from(error: CliError) -> Self {
        Self::Coded(error)
    }
}

impl From<anyhow::Error> for CommandError {
    fn from(error: anyhow::Error) -> Self {
        Self::Unexpected(error)
    }
}

impl From<std::io::Error> for CommandError {
    fn from(error: std::io::Error) -> Self {
        Self::coded(ErrorCode::Io).param("error", error)
    }
}

impl From<veil_core::error::VeilError> for CommandError {
    fn from(error: veil_core::error::VeilError) -> Self {
        use veil_core::error::VeilError;

        let (code, detail) = match error {
            VeilError::Io(error) => (ErrorCode::Io, error.to_string()),
            VeilError::Encrypt(error) => (ErrorCode::Encrypt, error.to_string()),
            VeilError::Decrypt(error) => (ErrorCode::Decrypt, error.to_string()),
            VeilError::Serialize(error) => (ErrorCode::Serialize, error.to_string()),
            VeilError::Format(error) => (ErrorCode::Format, error),
            VeilError::WorkspaceError(error) => (ErrorCode::Workspace, error),
            VeilError::ConfigError(error) => (ErrorCode::Config, error),
            VeilError::ContainerNotFound(error) => (ErrorCode::ContainerNotFound, error),
            VeilError::InvalidFormat(error) => (ErrorCode::InvalidFormat, error),
            VeilError::SerializationError(error) => (ErrorCode::Serialization, error),
            VeilError::EncryptionError(error) => (ErrorCode::Encryption, error),
            VeilError::DecryptionError(error) => (ErrorCode::Decryption, error),
            VeilError::KeyDerivationError(error) => (ErrorCode::KeyDerivation, error),
            VeilError::FileNotFound(error) => (ErrorCode::FileNotFound, error),
            VeilError::FileAlreadyExists(error) => (ErrorCode::FileAlreadyExists, error),
            VeilError::VolumeUnavailable(error) => (ErrorCode::VolumeUnavailable, error),
        };

        Self::coded(code).param("error", detail)
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.render())
    }
}

impl fmt::Debug for CliError {
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

impl fmt::Debug for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Coded(error) => error.fmt(formatter),
            Self::Unexpected(error) => formatter.debug_tuple("Unexpected").field(error).finish(),
        }
    }
}

/// 返回一个带错误码的 `CommandError`。
#[macro_export]
macro_rules! cli_error {
    ($code:ident) => {
        $crate::error::CommandError::coded($crate::error::ErrorCode::$code)
    };
    ($code:ident, $($name:literal => $value:expr),+ $(,)?) => {
        $crate::error::CommandError::coded($crate::error::ErrorCode::$code)
            $(.param($name, $value))+
    };
}

/// 直接返回一个带错误码的 `CommandError`。
#[macro_export]
macro_rules! cli_bail {
    ($($args:tt)*) => {
        return Err($crate::cli_error!($($args)*))
    };
}
