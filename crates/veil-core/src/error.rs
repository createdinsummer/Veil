//! `veil-core` 的统一错误类型与结果别名。
//!
//! 模块集中定义底层 I/O、容器格式、配置、工作区和加解密错误，避免各子模块自行
//! 发明错误类型，并让上层 CLI 能在保留原始错误上下文的同时统一处理。

use thiserror::Error;

/// `veil-core` 的统一错误类型。
///
/// 调用方可按错误变体区分 I/O、格式、配置、工作区和加解密失败；携带文本的错误
/// 用于补充由库内部判断出的上下文。各错误来源保持原始错误信息，便于上层记录日志。
#[derive(Debug, Error)]
pub enum VeilError {
    #[error("操作已中止")]
    Cancelled,
    #[error("数据库错误: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("数据库连接池错误: {0}")]
    DatabasePool(String),
    /// 文件系统或字节流读写失败。
    #[error("I/O 错误: {0}")]
    Io(#[from] std::io::Error),

    /// 无法确认提交结果或持久化状态，调用方应重新读取权威状态。
    #[error("提交结果或持久化状态不确定: {0}")]
    CommitUncertain(#[source] std::io::Error),

    /// 容器字节格式不符合预期。
    #[error("容器格式错误: {0}")]
    Format(String),

    /// 工作区创建、读取或更新失败。
    #[error("工作区错误: {0}")]
    WorkError(String),

    /// 申请锁时遇到的已有锁。
    #[error("锁冲突：级别 {level}，范围 {scope}")]
    LockConflict { level: String, scope: String },

    #[error("导入文件 {file} 失败: {source}")]
    ImportBatchFailed {
        file: String,
        #[source]
        source: Box<VeilError>,
    },

    /// 全局配置读写或解析失败。
    #[error("配置错误: {0}")]
    ConfigError(String),

    /// 按名称、ID、链接或路径均无法定位容器。
    #[error("容器未找到: {0}")]
    VeilNotFound(String),

    /// 同一稳定 `veil_id` 被注册给多个容器。
    #[error("容器 ID 冲突: {0}")]
    VeilIdConflict(String),

    /// 容器展示名称匹配到多个容器。
    #[error("容器名称不唯一: {0}")]
    VeilNameAmbiguous(String),

    /// 文件格式或字段内容无效。
    #[error("无效的格式: {0}")]
    InvalidFormat(String),

    /// 元数据、配置或链接的序列化失败。
    #[error("序列化错误: {0}")]
    SerializationError(String),

    /// 文件内容或元数据加密失败。
    #[error("加密错误: {0}")]
    EncryptionError(String),

    /// 文件内容或元数据解密失败。
    #[error("解密错误: {0}")]
    DecryptionError(String),

    /// 密码派生密钥失败。
    #[error("密钥派生错误: {0}")]
    KeyDerivationError(String),

    /// 容器元数据中不存在目标文件。
    #[error("文件未找到: {0}")]
    FileNotFound(String),

    /// 目标名称已被容器中的其他文件占用。
    #[error("文件已存在: {0}")]
    FileAlreadyExists(String),

    /// 文件已被其他操作修改，基础版本不再匹配。
    #[error("文件版本冲突: {0}")]
    VersionConflict(String),

    /// 链接所依赖的磁盘卷当前不可用。
    #[error("卷不可用: {0}")]
    VolumeUnavailable(String),
}

impl VeilError {
    /// 用于日志的稳定分类，不包含错误携带的数据。
    pub const fn log_code(&self) -> &'static str {
        match self {
            Self::Cancelled => "CANCELLED",
            Self::Database(..) => "DATABASE",
            Self::DatabasePool(..) => "DATABASE_POOL",
            Self::Io(..) => "IO",
            Self::CommitUncertain(..) => "COMMIT_UNCERTAIN",
            Self::Format(..) => "FORMAT",
            Self::WorkError(..) => "WORK_ERROR",
            Self::LockConflict { .. } => "LOCK_CONFLICT",
            Self::ImportBatchFailed { .. } => "IMPORT_BATCH_FAILED",
            Self::ConfigError(..) => "CONFIG_ERROR",
            Self::VeilNotFound(..) => "VEIL_NOT_FOUND",
            Self::VeilIdConflict(..) => "VEIL_ID_CONFLICT",
            Self::VeilNameAmbiguous(..) => "VEIL_NAME_AMBIGUOUS",
            Self::InvalidFormat(..) => "INVALID_FORMAT",
            Self::SerializationError(..) => "SERIALIZATION_ERROR",
            Self::EncryptionError(..) => "ENCRYPTION_ERROR",
            Self::DecryptionError(..) => "DECRYPTION_ERROR",
            Self::KeyDerivationError(..) => "KEY_DERIVATION_ERROR",
            Self::FileNotFound(..) => "FILE_NOT_FOUND",
            Self::FileAlreadyExists(..) => "FILE_ALREADY_EXISTS",
            Self::VersionConflict(..) => "VERSION_CONFLICT",
            Self::VolumeUnavailable(..) => "VOLUME_UNAVAILABLE",
        }
    }
}

/// `veil-core` 内部统一使用的结果类型别名。
pub type Result<T> = std::result::Result<T, VeilError>;
