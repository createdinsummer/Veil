//! 错误类型定义

use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("容器错误: {0}")]
    Container(#[from] veil_core::error::VeilError),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("文件不存在: {0}")]
    NotFound(String),

    #[error("不是文件: {0}")]
    NotFile(String),

    #[error("不是目录: {0}")]
    NotDirectory(String),

    #[error("只读文件系统")]
    ReadOnly,
}

pub type Result<T> = std::result::Result<T, Error>;
