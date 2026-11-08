//! Veil 应用服务：统一容器操作、工作区、会话流程与执行状态查询。

mod batch_policy;
mod command_transfer;
pub mod commands;
pub mod comparison;
pub mod config;
mod directory_cache;
mod deletion_queue;
pub mod execution;
mod export_pipeline;
pub mod hints;
pub mod import;
mod import_pipeline;
pub mod lifecycle;
pub mod link;
pub mod operations;
pub mod package;
pub mod session;
pub mod spaces;
pub mod storage;
mod transfer_conflicts;
pub mod veil_ops;
pub mod work;
pub mod workflow;

pub use execution::{ExecutionEndpoint, ExecutionServer, ExecutionService};
pub use operations::{ContainerOperation, ContainerOperationResult};
pub use session::{
    ContainerView, FileView, ServiceError, ServiceRequest, ServiceResponse, VeilService,
};

// 这些模块是服务实现依赖的核心能力，不包含界面适配逻辑。
use veil_core::{error, file_ops, fsutil, jsonc, metadata, version};
