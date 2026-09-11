//! Veil 核心库。
//!
//! 本 crate 提供单文件容器与工作区容器共用的底层能力，包括配置与链接模型、
//! 元数据格式、密钥派生、加解密、目录索引以及流式读写。上层 CLI 只负责组织
//! 用户交互和组合这些能力，不直接实现磁盘格式或密码学细节。

pub mod config;
pub mod container;
pub mod container_format;
pub mod error;
pub mod format;
pub mod index;
pub mod kdf;
pub mod keys;
pub mod link;
pub mod metadata;
pub mod mime;
pub mod slice_reader;
pub mod temp;
pub mod volume;
pub mod workspace;
pub mod workspace_ops;
