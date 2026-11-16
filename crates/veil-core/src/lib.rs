//! Veil 核心库。
//!
//! 本 crate 提供目录模型与打包格式的底层能力，包括
//! 元数据格式、密钥派生、加解密、文件清单、流式读写以及执行单元登记与存活检查。上层 CLI 只负责组织
//! 用户交互和组合这些能力，不直接实现磁盘格式或密码学细节。
//!
//! # 业务定位
//!
//! veil-core 是 Veil（本地加密文件保险箱）的底层能力库：负责磁盘格式、密码学、
//! 文件系统、队列与日志等基础能力，不包含面向用户的业务流程。业务表、文件租约
//! （某段时间内独占操作权的凭证）与恢复流程由上层 veil-service 编排，
//! CLI / 桌面端 / 移动端只通过 service 组合调用，不自行实现磁盘格式或密码学细节。
//!
//! # 核心模块清单（按职责分组）
//!
//! - 加密与文件：crypto（单段 ChaCha20-Poly1305 原语，AEAD=加密同时防篡改）、
//!   kdf（KDF=密钥派生函数，从密码派生密钥材料）、file_ops（文件分块流式读写）。
//! - 数据模型：metadata（容器元数据快照）、volume（卷/容器身份模型）、version（文件版本链比较）。
//! - 基础设施：database（SQLite/SQLCipher 连接，SQLCipher=SQLite 的整库加密扩展）、
//!   queue（执行任务队列）、temp（临时文件生命周期）、locking（文件级锁等待选项）、
//!   progress（进度上报）、logging（日志与敏感信息脱敏）、mime（按扩展名猜测媒体类型）。
//! - 打包迁移：veil_package（.veil 打包文件格式：加密数据库一致快照+引用密文，供跨设备迁移）。
//! - 执行身份：execution（执行单元登记、任务作用域与存活检查）。
//!
//! # 依赖能力
//!
//! - 系统资源：文件系统、挂载卷枚举（macOS getfsstat / Linux /proc/self/mountinfo / Windows 盘符）、
//!   单调时钟（租约等待计时，不受系统时间调整影响）。
//! - 外部库：SQLite/SQLCipher（加密数据库）、OpenSSL（密码学后端）、serde（序列化）、
//!   自建 JSONL 日志通道（结构化字段与后台写盘，不依赖 tracing 后端）。
//!
//! # 整体业务目标
//!
//! 为"本地加密容器"提供统一、可验证的底层原语：容器库用 SQLCipher 整库加密，
//! 文件内容用独立密文流式存储，数据库引用是权威来源；本 crate 保证这些原语的
//! 行为一致，跨平台（桌面/移动）可复现。

/// 密码学与流式加密：密钥材料、AEAD 分块加密与密文文件格式。
pub mod crypto;
/// 统一错误类型：各模块失败都转换为 VeilError 向上传递。
pub mod error;
/// 执行身份：进程/线程/任务身份、登记簿与存活检查。
pub mod execution;
/// 文件分块流式读写：1 MiB 分块的 ChaCha20-Poly1305 密文文件操作。
pub mod file_ops;
/// 文件系统工具：目录扫描、原子写等底层辅助。
pub mod fsutil;
/// JSONC（带注释的 JSON）解析，用于历史配置等场景。
pub mod jsonc;
/// 密钥派生（KDF）：从密码派生密钥材料。
pub mod kdf;
/// 文件级锁等待选项；数据库租约协议本身由 veil-service 实现。
pub mod locking;
/// 日志系统：结构化日志与敏感信息（路径、密码）脱敏。
pub mod logging;
/// 容器元数据模型：目录树、文件信息、版本链等描述快照。
pub mod metadata;
/// MIME 类型：仅按文件扩展名映射，不读取内容，也不验证实际格式。
pub mod mime;
/// 进度上报：任务进度跟踪与回调。
pub mod progress;
/// 执行任务队列：任务调度与并发消费。
pub mod queue;
/// 临时文件生命周期：暂存文件的创建与清理。
pub mod temp;
/// .veil 打包文件格式：加密数据库快照+引用密文，供跨设备迁移。
pub mod veil_package;
/// 文件版本链：版本链校验与继承关系比较。
pub mod version;
/// 卷/容器身份模型：运行时枚举挂载卷、识别路径所属卷。
pub mod volume;

/// 数据库底层：普通 SQLite 应用库与 SQLCipher 容器库的统一连接能力。
pub mod database;
/// 数据库连接池（crate 私有）。
mod database_pool;
