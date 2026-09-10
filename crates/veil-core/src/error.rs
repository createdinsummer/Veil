use thiserror::Error;

/// veil-core 库层的统一错误类型。
/// 用 thiserror 派生 `Error` trait，比手写 impl Display/Error 省很多样板。
#[derive(Debug, Error)]
pub enum VeilError {
    // #[from] 让「?」能把 std::io::Error 自动转成 VeilError::Io
    // #[from]:最关键的一招。它自动生成「从别的错误类型转成 VeilError」的实现,
    // 于是你在函数里写 some_io_op()?,? 会自动把 io::Error 转成 VeilError::Io 再返回。省掉一堆手工 map_err。
    //#[derive(Error)](thiserror):自动帮你的枚举实现标准 Error trait,#[error("...")] 定义每个变体的报错文案。
    #[error("I/O 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("age 加密失败: {0}")]
    Encrypt(#[from] age::EncryptError),

    #[error("age 解密失败（密码错误或数据被篡改？）: {0}")]
    Decrypt(#[from] age::DecryptError),

    #[error("索引序列化失败: {0}")]
    Serialize(#[from] postcard::Error),

    // 没有现成错误类型可转的、我们自己判定的格式错误
    #[error("容器格式错误: {0}")]
    Format(String),

    // 工作区相关错误
    #[error("工作区错误: {0}")]
    WorkspaceError(String),

    #[error("配置错误: {0}")]
    ConfigError(String),

    #[error("容器未找到: {0}")]
    ContainerNotFound(String),

    #[error("无效的格式: {0}")]
    InvalidFormat(String),

    #[error("序列化错误: {0}")]
    SerializationError(String),

    #[error("加密错误: {0}")]
    EncryptionError(String),

    #[error("解密错误: {0}")]
    DecryptionError(String),

    #[error("密钥派生错误: {0}")]
    KeyDerivationError(String),

    #[error("文件未找到: {0}")]
    FileNotFound(String),

    #[error("文件已存在: {0}")]
    FileAlreadyExists(String),

    #[error("卷不可用: {0}")]
    VolumeUnavailable(String),
}

/// 库内部统一用这个 Result 别名，少写一遍错误类型。
/// // type Result<T>:类型别名,让签名从 Result<Foo, VeilError> 简写成 Result<Foo>。
pub type Result<T> = std::result::Result<T, VeilError>;
