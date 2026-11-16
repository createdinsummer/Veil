//! Veil 应用服务：统一容器操作、工作区、会话流程与执行状态查询。
//!
//! 本 crate 是 Veil 的业务编排层，位于底层能力 veil-core 之上：桌面端、
//! CLI 与移动端都通过它访问容器业务，不直接操作数据库或密文文件。它把
//! 「本机应用库（app.db）＋加密容器库（veil.db）＋密文文件」三层存储组合
//! 成完整业务链路，职责分为四块：
//!
//! - 存储访问（storage 模块）：本机库的空间登记、用户/系统设置与展示缓存，
//!   以及容器库的目录树、内容版本链、文件密钥与租约表，全部由 service
//!   独占访问，连接与磁盘加密格式由 core 统一提供；
//! - 并发控制（storage::locks）：以数据库租约表实现的文件级读写锁，
//!   写租约的申请/续租/释放/交接通过数据库事务校验持锁凭证；部分 ReadOnly
//!   标记不登记持久租约，也不保证稳定快照。提交写入时同时检查基础
//!   版本与锁凭证，防止过期或已失效的写入覆盖新版本；
//! - 业务编排：容器创建/删除/改密、文件增删改查、导入导出与打包
//!   （operations、lifecycle、import、package 等模块）；
//! - 执行调度（execution 模块）：把任务分发给本机执行单元，并通过带随机
//!   访问凭证的本机回环端点跨进程查询任务存活状态，供租约回收判定持有者
//!   是否已退出。
//!
//! 对外公开的核心类型：VeilService（会话协议入口）、ContainerOperation 与
//! ContainerOperationResult（容器操作请求/结果）、ExecutionService 与
//! ExecutionEndpoint（执行身份与状态查询）。

// 批量提交策略：把本机用户设置（导入合并数量/间隔）换算成 core 队列的推送阈值。
mod batch_policy;
// 命令传输：适配命令层导入导出，处理路径收集、冲突策略、进度及执行单元身份。
mod command_transfer;
// 命令业务入口：封装容器选择、文件操作、传输和配置流程；终端参数解析仍由 CLI 完成。
pub mod commands;
// 比较策略：按版本/元数据、内容哈希或实际字节判断容器文件与外部来源是否相同。
pub mod comparison;
// 应用配置：链接文件（.veil-link）与工作目录约定，创建/登记流程的基础。
pub mod config;
// 单次文件传输共享的目录缓存：容器节点与文件系统目录的进程内缓存。
mod directory_cache;
// 已提交删除的物理回收队列：不访问数据库、不持有密码的后台删文件线程。
mod deletion_queue;
// 执行服务：执行身份登记、跨进程存活查询与任务调度封装。
pub mod execution;
// 导出流水线：把容器内容整理成导出任务清单的分阶段流水线。
mod export_pipeline;
// 提示系统：按提示级别与「是否已展示」的一次性本机状态决定是否提示。
pub mod hints;
// 导入收集：把本地文件/目录展开成容器内目标路径的添加请求。
pub mod import;
// 导入流水线：分批传输、冲突确认、目录加锁与合并提交的编排。
mod import_pipeline;
// 生命周期：容器创建、登记、删除及移动的事务编排。
pub mod lifecycle;
// 链接：.veil-link 便携入口的读写与恢复。
pub mod link;
// 操作定义：面向调用方的容器操作请求与结果，统一转交 VeilManager 执行。
pub mod operations;
// 打包：第 2 版 .veil 加密包的流式导出与解包验证。
pub mod package;
// 会话：客户端请求/响应协议与 VeilService 统一入口。
pub mod session;
// 空间：空间目录与容器位置的业务管理。
pub mod spaces;
// 存储：本机应用库（app.db）与加密容器库（veil.db）的数据访问层。
pub mod storage;
// 传输冲突：文件传输中同名冲突的有界确认队列。
mod transfer_conflicts;
// 容器操作实现：文件增删改查、导入提交、改密等核心业务逻辑。
pub mod veil_ops;
// 工作目录：容器目录与链接文件在磁盘上的布局约定。
pub mod work;
// 工作流：把命令分解为执行单元步骤的高级编排。
pub mod workflow;

// 常用入口类型的便捷再导出；其他操作和存储类型仍可通过公开模块访问，并非只允许使用这几组。
pub use execution::{ExecutionEndpoint, ExecutionServer, ExecutionService};
pub use operations::{ContainerOperation, ContainerOperationResult};
pub use session::{
    ContainerView, FileView, ServiceError, ServiceRequest, ServiceResponse, VeilService,
};

// 这些模块是服务实现依赖的核心能力，不包含界面适配逻辑。
use veil_core::{error, file_ops, fsutil, jsonc, metadata, version};
