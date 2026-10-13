//! Veil 核心库。
//!
//! 本 crate 提供目录模型与打包格式的底层能力，包括配置与链接模型、
//! 元数据格式、密钥派生、加解密、文件清单以及流式读写。上层 CLI 只负责组织
//! 用户交互和组合这些能力，不直接实现磁盘格式或密码学细节。

pub mod config;
pub mod error;
pub mod file_ops;
pub(crate) mod fsutil;
pub mod kdf;
pub mod link;
pub mod metadata;
pub mod mime;
pub mod temp;
pub mod veil_ops;
pub mod veil_package;
pub mod volume;
pub mod work;
