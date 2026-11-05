//! Veil 核心库。
//!
//! 本 crate 提供目录模型与打包格式的底层能力，包括配置与链接模型、
//! 元数据格式、密钥派生、加解密、文件清单、流式读写以及执行单元登记与存活检查。上层 CLI 只负责组织
//! 用户交互和组合这些能力，不直接实现磁盘格式或密码学细节。

pub mod crypto;
pub mod error;
pub mod execution;
pub mod file_ops;
pub mod fsutil;
pub mod jsonc;
pub mod kdf;
pub mod locking;
pub mod metadata;
pub mod mime;
pub mod progress;
pub mod temp;
pub mod veil_package;
pub mod version;
pub mod volume;

pub mod database;
