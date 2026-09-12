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
    /// 文件系统或字节流读写失败。
    #[error("I/O 错误: {0}")]
    Io(#[from] std::io::Error),

    /// age 加密失败。
    #[error("age 加密失败: {0}")]
    Encrypt(#[from] age::EncryptError),

    /// age 解密失败，通常表示密码错误或密文损坏。
    #[error("age 解密失败（密码错误或数据被篡改？）: {0}")]
    Decrypt(#[from] age::DecryptError),

    /// 目录索引的 postcard 序列化或反序列化失败。
    #[error("索引序列化失败: {0}")]
    Serialize(#[from] postcard::Error),

    /// 容器字节格式不符合预期。
    #[error("容器格式错误: {0}")]
    Format(String),

    /// 工作区创建、读取或更新失败。
    #[error("工作区错误: {0}")]
    WorkspaceError(String),

    /// 全局配置读写或解析失败。
    #[error("配置错误: {0}")]
    ConfigError(String),

    /// 按名称、ID、链接或路径均无法定位容器。
    #[error("容器未找到: {0}")]
    ContainerNotFound(String),

    /// 同一稳定 `veil_id` 被注册给多个容器。
    #[error("容器 ID 冲突: {0}")]
    ContainerIdConflict(String),

    /// 文件格式或字段内容无效。
    #[error("无效的格式: {0}")]
    InvalidFormat(String),

    /// 除目录索引以外的数据序列化失败。
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

    /// 链接所依赖的磁盘卷当前不可用。
    #[error("卷不可用: {0}")]
    VolumeUnavailable(String),
}

/// `veil-core` 内部统一使用的结果类型别名。
pub type Result<T> = std::result::Result<T, VeilError>;
