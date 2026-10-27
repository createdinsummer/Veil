//! Veil 应用服务：统一容器操作、工作区和会话流程。

pub mod commands;
pub mod config;
pub mod hints;
pub mod import;
pub mod lifecycle;
pub mod link;
pub mod operations;
pub mod package;
pub mod session;
pub mod veil_ops;
pub mod work;
pub mod workflow;

pub use operations::{ContainerOperation, ContainerOperationResult};
pub use session::{
    ContainerView, FileView, ServiceError, ServiceRequest, ServiceResponse, VeilService,
};

// 这些模块是服务实现依赖的核心能力，不包含界面适配逻辑。
use veil_core::{error, file_ops, fsutil, jsonc, kdf, locking, metadata, version, volume};
