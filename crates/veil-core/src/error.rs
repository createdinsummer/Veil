//! `veil-core` 的统一错误类型与结果别名。
//!
//! 模块集中定义底层 I/O、容器格式、配置、工作区和加解密错误，避免各子模块自行
//! 发明错误类型，并让上层 CLI 能在保留原始错误上下文的同时统一处理。
//!
//! 所属业务模块：veil-core 的公共错误层，被 database、file_ops、kdf、crypto、
//! execution 等多个子模块引用；部分底层接口仍直接返回标准 I/O 等错误类型。
//!
//! 文件内核心清单：
//! - `VeilError`：统一错误枚举，覆盖取消、数据库、I/O、格式、锁冲突、加解密等
//!   22 类业务失败；携带文本的错误用于补充库内部判断出的上下文。
//! - `VeilError::log_code`：把错误映射为不含业务数据的稳定分类码，供日志聚合。
//! - `Result<T>`：库内统一结果别名，错误固定为 `VeilError`。
//!
//! 依赖能力：
//! - `thiserror`：通过派生宏生成 `std::error::Error` 实现与展示文案，`#[from]`
//!   自动把 rusqlite/std::io 等底层错误转换为本枚举，`#[source]` 保留错误链。
//!
//! 整体业务目标：上层（veil-service、CLI、桌面端）能用同一套错误类型区分业务失败
//! 场景（文件已存在、版本冲突、锁冲突、密码错误），并按稳定分类码聚合日志。

use thiserror::Error;

/// `veil-core` 的统一错误类型。
///
/// 调用方可按错误变体区分 I/O、格式、配置、工作区和加解密失败；携带文本的错误
/// 用于补充由库内部判断出的上下文。各错误来源保持原始错误信息，便于上层记录日志。
#[derive(Debug, Error)]
pub enum VeilError {
    /// 操作被调用方或进度检查主动中止；锁等待的超时/取消也可能走 WorkError，须按实际调用边界区分。
    #[error("操作已中止")]
    Cancelled,
    /// SQLite/SQLCipher 数据库操作失败（执行 SQL、事务、备份等底层错误）。
    #[error("数据库错误: {0}")]
    Database(#[from] rusqlite::Error),
    /// 数据库连接池获取连接失败或池自身状态错误；携带文本描述具体原因。
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
    ///
    /// 字段：
    /// - `level`：已有占用者的诊断级别，当前租约实现通常输出 SR/SW/EW，不是数据库 mode 字段的原文。
    /// - `scope`：冲突发生的范围（如具体文件节点 ID），说明哪份资源正被占用。
    #[error("锁冲突：级别 {level}，范围 {scope}")]
    LockConflict { level: String, scope: String },

    /// 批量导入时某个文件失败，携带失败文件名与原始错误链。
    #[error("导入文件 {file} 失败: {source}")]
    ImportBatchFailed {
        /// 导入失败的文件路径或名称，用于向用户指出是哪一个文件出错。
        file: String,
        /// 该文件失败的具体原因；保留错误链以维持原始上下文。
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
    /// 功能：把底层失败归为稳定日志类别，便于聚合而不泄露错误携带的业务内容。
    /// 入参：self: &VeilError 为已构造的失败原因，只读取变体，不展开其文字或来源。
    /// 返回值：&'static str 静态分类码；同一错误变体返回同一码，多个不同详细原因可能被归为同一码。
    /// 异常：穷尽匹配枚举，不返回错误或读取外部资源。
    /// 注意事项：此码用于日志分类，不等于 CLI 数值错误码或进程退出码；新增变体时编译器要求补齐映射。
    pub const fn log_code(&self) -> &'static str {
        // 按变体一一映射为稳定分类码；不输出错误携带的数据，避免日志泄露内容
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
///
/// 业务含义：`Ok(T)` 表示操作成功并携带产出；`Err(VeilError)` 携带上文定义的统一
/// 错误。采用此别名的接口统一返回该错误；其他接口可能返回各自的底层错误类型。
pub type Result<T> = std::result::Result<T, VeilError>;
