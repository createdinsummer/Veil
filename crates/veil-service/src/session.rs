//! 面向调用方的容器会话请求入口。
//!
//! 所属业务模块：veil-service 的统一会话（session）层。桌面端、CLI、移动端都通过
//! VeilService::execute 发送 ServiceRequest（容器创建/解锁/列表、文件导入导出、
//! 设置、链接、打包解包等），得到 ServiceResponse；本文件把 veil-core 的底层能力
//! 组合成完整业务，并集中管理「会话」——客户端一次交互期间持有的解锁状态
//! （容器目录 + 容器 ID + 密码）。锁定移除主会话；在途快照、连接池或脱敏上下文可能仍持敏感副本。
//!
//! 文件内核心清单：
//! - `VeilService`：服务门面，持有会话、共享进度槽、串行互斥量、导入/导出冲突
//!   队列、容器生命周期锁、物理删除队列与垃圾清理队列（九个元组字段）；
//! - `ServiceRequest` / `ServiceResponse` / `ServiceError`：请求、响应与错误协议；
//! - `Session`：解锁状态快照（path / veil_id / password）；
//! - `GarbageCollectionQueue`：定时批量触发密文清理的后台队列（削峰/异步）；
//! - `import_sources_with_interval`：多线程并行导入流水线（工作线程加密、
//!   主线程按时间/数量阈值批量提交）；
//! - `ContainerView` / `FileView`：容器列表展示快照。
//!
//! 依赖能力：veil-core 的 Progress（进度）、BoundedQueue（有界队列）、执行身份
//! 服务；storage 的空间/租约访问；import/export 流水线与冲突队列；deletion_queue
//! 物理删除队列。
//!
//! 整体业务目标：同一套服务协议服务所有客户端，保证并发安全——会话互斥、
//! 传输进度槽协调、容器生命周期锁（删除与使用互斥）、按模式兼容的文件级租约，
//! 并把长流程（导入/导出/清理）放到后台线程与队列中执行。

use crate::veil_ops::VeilManager;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
use veil_core::metadata::{MetaData, generate_veil_id};
use zeroize::Zeroizing;

/// 接收来自命令行、桌面端或移动端的业务请求。
///
/// 字段按位置分工（元组结构体，Clone 后各客户端/线程共享同一底层状态）：
/// 0. 当前解锁会话（容器目录、ID、密码）；None 表示未解锁；
/// 1. 共享传输进度对象（导入/导出共用；忙时其他请求仍可执行，不能把它当成全传输互斥锁）；
/// 2. 串行互斥量：锁定/切换容器、移动容器等会改变会话或进度槽的操作都先拿它，
///    保证「会话变更」串行化；
/// 3. 会话对应目录的独立RwLock快照（与字段0成对维护，供不持会话Mutex的读取）；
/// 4. 流式导入冲突队列注册表：流式导入进行中时挂在这里，供轮询/决策请求访问；
/// 5. 流式导出冲突队列注册表（同上，导出专用）；
/// 6. 当前会话持有的容器生命周期共享锁句柄（防容器被删除流程并发移除）；
/// 7. 物理删除队列：删除文件后把失去引用的密文文件排入，后台统一删除；
/// 8. 垃圾清理队列：批量触发 veil.db 中的失效密文清理（后台线程消费）。
#[derive(Clone)]
pub struct VeilService(
    Arc<Mutex<Option<Session>>>,
    veil_core::progress::Progress,
    Arc<Mutex<()>>,
    Arc<RwLock<Option<PathBuf>>>,
    Arc<Mutex<Option<Arc<crate::import_pipeline::ImportConflictQueue>>>>,
    Arc<Mutex<Option<Arc<crate::transfer_conflicts::ConflictQueue<crate::export_pipeline::Work>>>>>,
    Arc<Mutex<Option<Vec<std::fs::File>>>>,
    Arc<crate::deletion_queue::PhysicalDeletionQueue>,
    Arc<GarbageCollectionQueue>,
);

impl Default for VeilService {
    /// 创建未解锁的服务状态，并启动物理删除及垃圾清理后台消费者。
    /// 入参：无。
    /// 返回值：VeilService，进度/会话/冲突登记为空，Clone后共享同一组状态。
    /// 异常：后台线程创建失败可panic，没有Result返回。
    /// 注意事项：构造会启动线程但尚不扫描容器；垃圾清理线程持有session的Arc，生命周期不应仅按服务Clone数量推断。
    fn default() -> Self {
        // 会话 Arc 同时交给清理队列的消费者，供其读取当前解锁密码做清理。
        let session = Arc::new(Mutex::new(None));
        Self(
            Arc::clone(&session),
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            Arc::new(GarbageCollectionQueue::new(session)),
        )
    }
}

impl Drop for VeilService {
    /// 在会话Arc计数恰为1时，尝试让本进程容器连接缓存失效。
    /// 入参：self（&mut VeilService）为被释放的服务句柄。
    /// 返回值：无；仅条件满足且状态锁可用、会话存在时调用invalidate。
    /// 异常：会话Mutex中毒时跳过本段，后台队列的后续析构行为由各自类型负责。
    /// 注意事项：垃圾清理消费者也持session Arc，因此“最后一个服务Clone”不必使计数为1；invalidate不通知其他进程，也不强制中止已借出连接。
    fn drop(&mut self) {
        // 仅当会话 Arc 恰好剩一个引用才进入；后台消费者也可能持有引用，不能按服务 Clone 数量推断。
        if Arc::strong_count(&self.0) == 1 {
            if let Ok(session) = self.0.lock() {
                if let Some(session) = session.as_ref() {
                    veil_core::database::invalidate(
                        &session.path.join(veil_core::database::CONTAINER_DB),
                    );
                }
            }
        }
    }
}

/// 流式导入请求的冲突队列登记守卫，借用服务保存当前队列的 Mutex 槽位。
/// 导入期间让客户端找到待审核冲突；作用域正常结束或 panic 展开析构时清空槽位。
/// 自身不持有队列消息，不取消传输、不回滚文件；进程直接退出时不保证执行析构。
struct StreamingImportRegistration<'a>(
    &'a Mutex<Option<Arc<crate::import_pipeline::ImportConflictQueue>>>,
);
impl Drop for StreamingImportRegistration<'_> {
    /// 撤销本次流式导入的冲突队列登记，让后续轮询不再读取旧任务。
    /// 入参：self 为 StreamingImportRegistration，借用服务冲突槽Mutex。
    /// 返回值：无；把槽位设为None。
    /// 异常：中毒时通过into_inner恢复，原panic不在此捕获。
    /// 注意事项：只清除登记，不主动取消任务或回滚文件；在请求作用域结束时运行。
    fn drop(&mut self) {
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) = None;
    }
}

/// 会话层导入/导出冲突的用户决策动作：跳过或覆盖（中止通过整体取消表达）。
/// 序列化为小写蛇形（"skip"/"overwrite"），与客户端 JSON 协议一致。
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionImportAction {
    /// 跳过该目标文件（保留占用者）。
    Skip,
    /// 覆盖目标文件（提交时仍会校验版本）。
    Overwrite,
}

/// 一条冲突的最终决议：目标路径 + 动作。客户端把整批决议一次性提交回来。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionImportResolution {
    /// 冲突批次中的目标路径。
    pub target: String,
    /// 用户对该目标的决定。
    pub action: SessionImportAction,
}

/// 导入任务的去重键：相同容器 + 相同来源路径集合 + 相同目标目录视为重复任务。
/// 注意只比较「用户选择的路径」，不比较文件内容——内容变化不改变任务身份。
#[derive(Clone, Hash, PartialEq, Eq)]
struct ImportKey {
    /// 容器目录（会话位置，天然区分不同容器）。
    container: PathBuf,
    /// 用户选择的来源路径集合（绝对路径、排序去重后比较）。
    source_paths: Vec<PathBuf>,
    /// 容器内目标目录（规范化后）。
    destination: String,
}

/// 取得本进程所有服务实例共用的导入去重登记簿。
/// 入参：无。
/// 返回值：`Arc<Mutex<HashSet<ImportKey>>>`，集合每项为一个尚未撤销登记的来源集合/目标/容器键。
/// 异常：无Result错误返回。
/// 注意事项：仅进程内共享，不阻止其他进程相同任务；集合只存路径身份，不比较来源内容。
fn import_registry() -> Arc<Mutex<HashSet<ImportKey>>> {
    static REGISTRY: OnceLock<Arc<Mutex<HashSet<ImportKey>>>> = OnceLock::new();
    Arc::clone(REGISTRY.get_or_init(|| Arc::new(Mutex::new(HashSet::new()))))
}

/// 导入任务登记守卫（RAII）：插入登记簿成功后持有，请求结束析构时移除自身键，
/// 保证重复判断在任务真正结束时才解除。
struct ImportGuard {
    /// 全局登记簿的引用。
    active: Arc<Mutex<HashSet<ImportKey>>>,
    /// 本任务自己的键（析构时移除）。
    key: ImportKey,
}
impl Drop for ImportGuard {
    /// 在导入请求退出时移除其去重键，允许后续重试。
    /// 入参：self（&mut ImportGuard）保存全局登记簿及本请求ImportKey。
    /// 返回值：无，移除对应HashSet条目。
    /// 异常：Mutex中毒时into_inner恢复，不返回错误。
    /// 注意事项：登记取消不撤销已经提交的导入；先前工作应在此守卫作用域结束前收敛。
    fn drop(&mut self) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        active.remove(&self.key);
    }
}

/// 容器会话请求协议；密码不应公开记录，成功解锁后会由内存会话持有。
///
/// 这是所有客户端的统一请求协议：每个变体对应一个业务动作，参数即业务入参。
/// 带 password 的请求在 execute 入口会把密码登记为敏感信息（日志脱敏），
/// 创建/打开成功时密码复制到会话内存，后台任务还可持快照；不写入明文本机应用库。
pub enum ServiceRequest<'a> {
    /// 查询某执行身份（进程/线程/任务）的存活状态；用于锁回收时确认持有者是否结束。
    CheckExecution {
        /// 被查询的执行身份。
        owner: &'a crate::execution::ExecutionOwner,
        /// 对方进程的本机查询端点（查询其他进程的线程/任务时提供）。
        endpoint: Option<&'a crate::execution::ExecutionEndpoint>,
        /// 查询超时时间。
        timeout: std::time::Duration,
    },
    /// 在指定存储位置创建新容器并直接解锁，返回容器视图。
    CreateContainer {
        /// 存放容器的父目录（空间目录）。
        parent: String,
        /// 容器展示名称（1-100 字符）。
        name: String,
        /// 新容器密码（同时用于数据库与后续文件操作）。
        password: String,
    },
    /// 用目录路径与密码解锁已有容器（验证密码、刷新登记并建立会话）。
    OpenContainer {
        /// 容器目录路径。
        path: String,
        /// 容器密码。
        password: String,
    },
    /// 刷新当前会话容器的展示视图（重新读取文件列表），不改变会话状态。
    RefreshContainer,
    /// 锁定主会话并释放其持有的密码包装/生命周期锁；在途任务快照可能仍持密码，之后新请求需重新打开。
    LockContainer,
    /// 列出本机全部空间及其中容器（设置页/首页数据源）。
    ListSpaces,
    /// 列出最近访问的容器记录（本机 recent_div 缓存）。
    ListRecents,
    /// 读取用户偏好项（如 hint_level、layout）。
    GetUserSetting {
        /// 偏好项名称。
        key: &'a str,
    },
    /// 写入用户偏好项。
    SetUserSetting {
        /// 偏好项名称。
        key: &'a str,
        /// JSON 值（类型与范围由存储层校验）。
        value: &'a serde_json::Value,
    },
    /// 设置页展示用的系统配置；服务只放行白名单键，见 `AppStore::system_setting_view`。
    GetSystemSetting {
        /// 系统设置项名称（白名单校验）。
        key: &'a str,
    },
    /// 空间改名（不改变空间 ID 与容器归属）。
    RenameSpace {
        /// 空间稳定 ID。
        id: &'a str,
        /// 新展示名称。
        name: &'a str,
    },
    /// 设置默认空间（system_setting.default_space_id）。
    SetDefaultSpace {
        /// 要设为默认的空间 ID。
        id: &'a str,
    },
    /// 物理删除空间：删除空间目录及全部容器，并清除登记与缓存。
    DeleteSpace {
        /// 空间 ID。
        id: String,
    },
    /// 仅移除不可用空间的本机记录，保留磁盘上的全部文件。
    DeleteSpaceRecord {
        /// 空间 ID。
        id: String,
    },
    /// 删除容器：持排他生命周期锁后删除登记、缓存并递归删除整个容器目录；
    /// 容器正在使用时直接拒绝。
    DeleteContainer {
        /// 容器 ID。
        id: String,
    },
    /// 取消容器在本机的登记（不删除磁盘数据）。
    UnregisterContainer {
        /// 容器 ID。
        id: &'a str,
    },
    /// 移动（独立复制）容器到另一空间：复制 DB 与密文、登记目标、保留源。
    MoveContainer {
        /// 源容器 ID。
        id: &'a str,
        /// 目标空间 ID。
        target_space: &'a str,
        /// 源容器密码（复制时需要打开源库）。
        password: &'a str,
    },
    /// 新建空间（可选绑定本机目录）。
    CreateSpace {
        /// 空间展示名称。
        name: String,
        /// 可选自定义目录；None由存储层在应用根下创建默认空间目录，并非保持未绑定。
        path: Option<PathBuf>,
    },
    /// 在指定空间内创建容器并解锁。
    CreateInSpace {
        /// 空间 ID 或名称。
        space: String,
        /// 容器展示名称。
        name: String,
        /// 新容器密码。
        password: String,
    },
    /// 批量导入（同步等待全部完成）：把来源文件/目录导入当前会话容器。
    ImportSessionFiles {
        /// 来源路径列表（文件或文件夹，可多个）。
        sources: Vec<String>,
        /// 容器内目标目录（空串表示根目录）。
        directory: String,
    },
    /// 流式导入：后台并行处理，目标占用产生冲突时通过轮询/决策请求交互。
    StreamSessionImport {
        /// 来源路径列表。
        sources: Vec<String>,
        /// 容器内目标目录。
        directory: String,
    },
    /// 在当前会话容器内新建文件夹。
    CreateSessionDirectory {
        /// 父目录路径（空串表示根目录）。
        directory: String,
        /// 新文件夹名称。
        name: String,
    },
    /// 轮询流式导入的待决策冲突批次（无新批次时返回 None）。
    PollSessionImportConflicts,
    /// 提交一批导入冲突的逐文件决策。
    ResolveSessionImportConflicts {
        /// 冲突批次 ID（由轮询返回）。
        id: String,
        /// 每条冲突的最终决定。
        resolutions: Vec<SessionImportResolution>,
    },
    /// 预览容器内文件：读取前 16 MiB 明文用于界面展示。
    PreviewSessionFile {
        /// 容器内文件路径。
        name: String,
    },
    /// 导出单个文件到指定位置（不覆盖已有文件）。
    ExportSessionFile {
        /// 容器内文件路径。
        name: String,
        /// 本机目标路径。
        destination: String,
    },
    /// 流式导出：把容器内文件/目录批量解密到本机，冲突轮询决策。
    StreamSessionExport {
        /// 要导出的容器内路径列表（可混文件与目录）。
        names: Vec<String>,
        /// 本机目标目录。
        destination: String,
    },
    /// 轮询流式导出的待决策冲突批次。
    PollSessionExportConflicts,
    /// 提交一批导出冲突的逐文件决策。
    ResolveSessionExportConflicts {
        /// 冲突批次 ID。
        id: String,
        /// 每条冲突的最终决定。
        resolutions: Vec<SessionImportResolution>,
    },
    /// 把当前会话容器打包为加密 .veil 包。
    ExportSessionPackage {
        /// 输出包文件路径（已存在则失败）。
        destination: String,
    },
    /// 在指定空间导入 .veil 包并解锁（新设备恢复场景）。
    ImportPackageInSpace {
        /// 包文件路径。
        path: String,
        /// 目标空间 ID 或名称。
        space: String,
        /// 包密码（也是导入后容器的新密码）。
        password: String,
    },
    /// 删除当前会话容器内的文件或目录（数据库先行，密文稍后清理）。
    RemoveSessionPath {
        /// 容器内路径。
        name: String,
    },
    /// 对「未通过会话解锁」的容器执行底层操作：直接携带目录与容器 ID。
    OperateContainer {
        /// 容器目录路径。
        directory: &'a Path,
        /// 容器稳定 ID。
        veil_id: &'a str,
        /// 要执行的底层操作（添加/删除/导出等）。
        operation: crate::operations::ContainerOperation<'a>,
    },
    /// 两阶段操作的第一步：按选择器定位容器，返回可跨请求携带的句柄。
    PrepareContainer {
        /// 容器选择器（ID/名称/目录/链接路径）。
        selector: &'a str,
    },
    /// 两阶段操作的第二步：对已准备好的容器句柄执行操作。
    OperatePrepared {
        /// 上一步返回的容器句柄。
        container: &'a crate::commands::ContainerHandle,
        /// 要执行的底层操作。
        operation: crate::operations::ContainerOperation<'a>,
    },
    /// 导入准备：解析容器并收集来源路径对应的导入任务（禁止覆盖，冲突待确认）。
    PrepareImport {
        /// 容器选择器。
        selector: &'a str,
        /// 容器外来源路径（文件或文件夹）。
        source: &'a Path,
        /// 可选容器内目标目录。
        destination: Option<&'a str>,
    },
    /// 导入提交：真正执行加密写入与数据库提交。
    CommitImport {
        /// 准备阶段产出的导入计划（可能已通过冲突确认）。
        plan: crate::commands::ImportPlan,
        /// 容器密码。
        password: &'a str,
    },
    /// 打包准备：解析容器并确定输出路径（存在即失败）。
    PrepareContainerPack {
        /// 容器选择器。
        selector: &'a str,
        /// 可选输出路径。
        output: Option<&'a str>,
    },
    /// 打包提交：执行加密打包并统计结果。
    CommitContainerPack {
        /// 打包计划。
        plan: crate::commands::PackPlan,
        /// 容器密码。
        password: &'a str,
    },
    /// 把选择器解析成容器事实（ID/名称/目录/链接信息），不改变任何状态。
    ResolveContainer {
        /// 容器选择器。
        selector: &'a str,
    },
    /// 读取配置摘要（空间列表、默认空间、容器数、链接数）。
    ReadConfigSummary,
    /// 用目录与密码把容器登记到本机（新设备重新登记/首次登记场景）。
    RegisterContainer {
        /// 容器目录路径。
        directory: &'a Path,
        /// 容器密码（用于解锁读取真实身份）。
        password: &'a str,
    },
    /// 返回本机应用数据库 app.db 的完整路径。
    ConfigPath,
    /// 按文件扩展名猜测 MIME 类型（预览展示用）。
    GuessMime {
        /// 文件路径。
        path: &'a str,
    },
    /// 计算生效的提示级别（可被覆盖值 override 影响）。
    EffectiveHintLevel {
        /// 可选的临时覆盖值（如 CLI 参数）。
        override_value: Option<&'a str>,
    },
    /// 判断某类提示是否应当展示（结合提示状态记录与覆盖值）。
    ShouldShowHint {
        /// 提示类别。
        kind: crate::hints::HintKind,
        /// 可选覆盖值。
        override_value: Option<&'a str>,
    },
    /// 设置提示级别并持久化到 user_setting。
    SetHintLevel {
        /// "full" / "brief" / "off"。
        level: &'a str,
    },
    /// 链接创建第一步：解析目标容器、确定输出路径，产出计划。
    PrepareLink {
        /// 容器选择器。
        target: &'a str,
        /// 可选输出路径。
        output: Option<&'a Path>,
    },
    /// 链接创建第二步：写入链接文件并登记缓存，返回输出路径。
    CommitLink {
        /// 链接计划。
        plan: crate::link::LinkPlan,
    },
    /// 仅收集导入任务清单（不执行），供客户端预展示。
    CollectImport {
        /// 来源路径。
        source: &'a Path,
        /// 可选目标目录。
        destination: Option<&'a str>,
    },
    /// 打包准备（按容器名/选择器），仅确定输出路径并校验不存在。
    PreparePack {
        /// 容器选择器或名称。
        name: &'a str,
        /// 可选输出路径。
        output: Option<&'a str>,
    },
    /// 校验新密码非空（改密前的输入校验）。
    ValidateNewPassword {
        /// 候选新密码。
        password: &'a str,
    },
    /// 创建流程第一步：解析创建选项（位置/空间/名称），产出创建计划。
    PrepareCreate {
        /// 创建选项（空间、名称、目录等）。
        options: crate::workflow::CreateOptions<'a>,
    },
    /// 创建流程第二步：执行容器初始化并登记。
    CommitCreate {
        /// 创建计划。
        plan: crate::workflow::CreatePlan,
        /// 新容器密码。
        password: &'a str,
    },
    /// 解包流程第一步：解析包与目标空间，产出解包计划。
    PrepareUnpack {
        /// 解包选项。
        options: crate::workflow::UnpackOptions<'a>,
    },
    /// 解包流程第二步：验证密码与内容认证后发布并登记容器。
    CommitUnpack {
        /// 解包计划。
        plan: crate::workflow::UnpackPlan,
        /// 包密码。
        password: &'a str,
    },
}

impl ServiceRequest<'_> {
    /// 生成固定请求类型标签，供日志归类而不暴露参数。
    /// 入参：self（&ServiceRequest）为任意请求变体。
    /// 返回值：&'static str，与请求变体对应的固定名称。
    /// 异常：无错误返回。
    /// 注意事项：不格式化密码、路径或业务载荷；这只是操作标签，不表示请求已经成功。
    fn log_operation(&self) -> &'static str {
        match self {
            Self::CheckExecution { .. } => "CheckExecution",
            Self::CreateContainer { .. } => "CreateContainer",
            Self::OpenContainer { .. } => "OpenContainer",
            Self::RefreshContainer => "RefreshContainer",
            Self::LockContainer => "LockContainer",
            Self::ListSpaces => "ListSpaces",
            Self::ListRecents => "ListRecents",
            Self::GetUserSetting { .. } => "GetUserSetting",
            Self::SetUserSetting { .. } => "SetUserSetting",
            Self::GetSystemSetting { .. } => "GetSystemSetting",
            Self::RenameSpace { .. } => "RenameSpace",
            Self::SetDefaultSpace { .. } => "SetDefaultSpace",
            Self::DeleteSpace { .. } => "DeleteSpace",
            Self::DeleteSpaceRecord { .. } => "DeleteSpaceRecord",
            Self::DeleteContainer { .. } => "DeleteContainer",
            Self::UnregisterContainer { .. } => "UnregisterContainer",
            Self::MoveContainer { .. } => "MoveContainer",
            Self::CreateSpace { .. } => "CreateSpace",
            Self::CreateInSpace { .. } => "CreateInSpace",
            Self::ImportSessionFiles { .. } => "ImportSessionFiles",
            Self::StreamSessionImport { .. } => "StreamSessionImport",
            Self::CreateSessionDirectory { .. } => "CreateSessionDirectory",
            Self::PollSessionImportConflicts => "PollSessionImportConflicts",
            Self::ResolveSessionImportConflicts { .. } => "ResolveSessionImportConflicts",
            Self::PreviewSessionFile { .. } => "PreviewSessionFile",
            Self::ExportSessionFile { .. } => "ExportSessionFile",
            Self::StreamSessionExport { .. } => "StreamSessionExport",
            Self::PollSessionExportConflicts => "PollSessionExportConflicts",
            Self::ResolveSessionExportConflicts { .. } => "ResolveSessionExportConflicts",
            Self::ExportSessionPackage { .. } => "ExportSessionPackage",
            Self::ImportPackageInSpace { .. } => "ImportPackageInSpace",
            Self::RemoveSessionPath { .. } => "RemoveSessionPath",
            Self::OperateContainer { .. } => "OperateContainer",
            Self::PrepareContainer { .. } => "PrepareContainer",
            Self::OperatePrepared { .. } => "OperatePrepared",
            Self::PrepareImport { .. } => "PrepareImport",
            Self::CommitImport { .. } => "CommitImport",
            Self::PrepareContainerPack { .. } => "PrepareContainerPack",
            Self::CommitContainerPack { .. } => "CommitContainerPack",
            Self::ResolveContainer { .. } => "ResolveContainer",
            Self::ReadConfigSummary => "ReadConfigSummary",
            Self::RegisterContainer { .. } => "RegisterContainer",
            Self::ConfigPath => "ConfigPath",
            Self::GuessMime { .. } => "GuessMime",
            Self::EffectiveHintLevel { .. } => "EffectiveHintLevel",
            Self::ShouldShowHint { .. } => "ShouldShowHint",
            Self::SetHintLevel { .. } => "SetHintLevel",
            Self::PrepareLink { .. } => "PrepareLink",
            Self::CommitLink { .. } => "CommitLink",
            Self::CollectImport { .. } => "CollectImport",
            Self::PreparePack { .. } => "PreparePack",
            Self::ValidateNewPassword { .. } => "ValidateNewPassword",
            Self::PrepareCreate { .. } => "PrepareCreate",
            Self::CommitCreate { .. } => "CommitCreate",
            Self::PrepareUnpack { .. } => "PrepareUnpack",
            Self::CommitUnpack { .. } => "CommitUnpack",
        }
    }
}

/// 业务请求的结果。
///
/// 每个变体对应一类请求的成功产物：容器视图、空间列表、预览字节、两阶段计划的
/// 句柄、冲突批次等。客户端按变体类型分发展示。
pub enum ServiceResponse {
    /// 执行身份存活状态查询结果。
    ExecutionStatus(crate::execution::Liveness),
    /// 容器展示视图（创建/解锁/刷新/导入后返回的最新列表）。
    Container(ContainerView),
    /// 容器已锁定（LockContainer 成功）。
    Locked,
    /// 空间列表视图。
    Spaces(Vec<crate::spaces::SpaceView>),
    /// 最近访问容器记录列表。
    Recents(Vec<crate::storage::app::RecentContainer>),
    /// 设置项 JSON 值。
    Setting(serde_json::Value),
    /// 预览内容（稳定状态读取：无并发写时）。
    Preview(Vec<u8>),
    /// 预览内容（不稳定状态：有其他进程持有共享写锁，内容可能随后变化）。
    PreviewUnstable(Vec<u8>),
    /// 导出完成但目标存在并发写入风险（内容已写出，供客户端提示）。
    ExportUnstable,
    /// 通用「已完成」应答（设置、删除、决策提交等）。
    Done,
    /// 底层容器操作的结果。
    Operation(crate::operations::ContainerOperationResult),
    /// 两阶段容器操作的已准备句柄。
    ContainerPrepared(crate::commands::ContainerHandle),
    /// 已准备的导入计划。
    ImportPrepared(crate::commands::ImportPlan),
    /// 导入结果（目标路径与密文名）。
    Imported(crate::commands::ImportResult),
    /// 已准备的打包计划。
    ContainerPackPrepared(crate::commands::PackPlan),
    /// 打包结果（文件数、大小等）。
    ContainerPacked(crate::commands::PackResult),
    /// 容器解析结果（ID/名称/目录/链接）。
    Resolved(crate::config::ResolvedVeil),
    /// 配置摘要。
    ConfigSummary(crate::config::ConfigSummary),
    /// 生效的提示级别。
    EffectiveHintLevel(crate::config::HintLevel),
    /// 是否应展示某提示。
    ShowHint(bool),
    /// 提示级别设置结果。
    HintLevelSet(crate::config::HintLevel),
    /// 本机应用数据库路径。
    ConfigPath(PathBuf),
    /// 猜测出的 MIME 类型（不支持时 None）。
    Mime(Option<String>),
    /// 创建流程准备产物。
    CreatePrepared(crate::workflow::CreatePlan),
    /// 创建完成的容器信息。
    Created(crate::workflow::CreatedContainer),
    /// 解包流程准备产物。
    UnpackPrepared(crate::workflow::UnpackPlan),
    /// 解包完成并登记的容器信息。
    Unpacked(crate::workflow::UnpackedContainer),
    /// 链接创建计划。
    LinkPrepared(crate::link::LinkPlan),
    /// 链接文件输出路径（创建成功）。
    LinkCreated(PathBuf),
    /// 导入任务清单（仅收集不执行）。
    ImportFiles(Vec<crate::veil_ops::AddFileSpec>),
    /// 待决策的冲突批次；None 表示当前没有待决策冲突（任务可能已结束）。
    ImportConflicts(Option<crate::import_pipeline::ConflictBatch>),
    /// 打包输出路径（准备成功）。
    PackPrepared(PathBuf),
    /// 新密码校验通过。
    PasswordValid,
}

/// 服务层错误；调用方可以按类型决定如何展示或处理。
///
/// 各变体覆盖不同层次：参数/状态类（InvalidInput、NotOpen 等）、传输类
/// （ImportFileFailed 携带部分完成的进度，便于客户端提示「处理后可重试」）、
/// 底层透传类（Core/Workflow/Import 用 `#[from]` 自动转换底层错误）。
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// 参数校验失败（面向用户的中文提示）。
    #[error("{0}")]
    InvalidInput(&'static str),
    /// 服务内部状态不可用（互斥量中毒等内部异常）。
    #[error("容器状态不可用")]
    StateUnavailable,
    /// 相同来源路径和目标目录的导入任务正在进行（防重复提交）。
    #[error("相同来源路径和目标目录的导入任务正在进行，请勿重复提交")]
    DuplicateImport,
    /// 请求需要已解锁会话但当前未解锁。
    #[error("请先打开容器")]
    NotOpen,
    /// 带业务上下文的 I/O 错误（操作名 + 底层错误）。
    #[error("{operation}：{source}")]
    Io {
        /// 正在执行的操作描述。
        operation: &'static str,
        /// 底层 I/O 错误。
        #[source]
        source: std::io::Error,
    },
    /// veil-core 底层错误（解密失败、版本冲突、锁冲突、取消等）。
    #[error("{0}")]
    Core(#[from] veil_core::error::VeilError),
    /// 创建/解包等工作流错误。
    #[error("{0}")]
    Workflow(#[from] crate::workflow::WorkflowError),
    /// 导入参数/路径错误。
    #[error("{0}")]
    Import(#[from] crate::import::ImportError),
    /// 导入过程中某个文件失败：附文件名与已完成/跳过进度，
    /// 已提交的文件保留，用户处理后重试即可续传。
    #[error(
        "导入失败：{name}；已完成 {completed} 个文件（其中跳过 {skipped} 个）：{source}。处理问题后可重新选择相同文件继续导入"
    )]
    ImportFileFailed {
        /// 失败的文件名。
        name: String,
        /// 已完成处理的文件数，包含成功提交与跳过；不是纯写入成功数。
        completed: u64,
        /// 其中被跳过的文件数。
        skipped: u64,
        /// 底层失败原因。
        #[source]
        source: veil_core::error::VeilError,
    },
    /// 用户取消导入：附已完成/跳过进度。
    #[error("导入已中止，已完成 {completed} 个文件（其中跳过 {skipped} 个）")]
    ImportCancelled { completed: u64, skipped: u64 },
    /// 容器创建失败：附残留目录路径（未自动删除，供清理/恢复）。
    #[error("容器创建未完成：{source}。目录保留在 {path}")]
    CreateFailed {
        /// 残留的容器目录。
        path: PathBuf,
        /// 失败原因。
        #[source]
        source: veil_core::error::VeilError,
    },
    /// 服务内部响应类型与请求不匹配（防御性错误，正常流程不应出现）。
    #[error("服务响应类型不符合请求")]
    UnexpectedResponse,
}

impl ServiceError {
    /// 按用户预期失败与内部故障选择本次请求的失败日志级别。
    /// 入参：self（&ServiceError）为失败；operation（&str）为固定操作标签，OpenContainer的解密错误特殊记Warn。
    /// 返回值：`Option<Level>`，None表示入口不记录此失败日志；其余返回Warn或Error。
    /// 异常：无错误返回。
    /// 注意事项：仅决定execute入口日志，底层模块可能另有日志；ImportBatchFailed会递归取内部原因，取消不记失败。
    fn log_level(&self, operation: &str) -> Option<veil_core::logging::Level> {
        use veil_core::logging::Level;
        match self {
            // 用户层面的预期结果，不记失败日志。
            Self::InvalidInput(_)
            | Self::DuplicateImport
            | Self::NotOpen
            | Self::ImportCancelled { .. }
            | Self::Import(
                crate::import::ImportError::Cancelled
                | crate::import::ImportError::InvalidDestination(_)
                | crate::import::ImportError::NotFile(_),
            ) => None,
            // 工作流错误按内部成分分级：I/O 与创建/登记失败必记 Error。
            Self::Workflow(error) => match error {
                crate::workflow::WorkflowError::Core(error) => core_log_level(error),
                crate::workflow::WorkflowError::Io(_)
                | crate::workflow::WorkflowError::CreateFailed { .. }
                | crate::workflow::WorkflowError::LinkRegistrationFailed { .. } => {
                    Some(Level::Error)
                }
                _ => None,
            },
            // 打开容器时密码错误是常见用户输入问题，只记 Warn。
            Self::Core(veil_core::error::VeilError::DecryptionError(_))
                if operation == "OpenContainer" =>
            {
                Some(Level::Warn)
            }
            // 其余 core 错误按核心分级表处理。
            Self::Core(error) | Self::ImportFileFailed { source: error, .. } => {
                core_log_level(error)
            }
            Self::Import(crate::import::ImportError::SourceNotFound(_)) => Some(Level::Warn),
            _ => Some(Level::Error),
        }
    }

    /// 把服务错误映射为跨客户端可聚合的固定错误码。
    /// 入参：self（&ServiceError）为服务失败。
    /// 返回值：&'static str，Workflow和Import委托各自映射，Core统一为CORE。
    /// 异常：无错误返回。
    /// 注意事项：错误码不包含敏感正文，也不替代详细原因或日志级别。
    fn log_code(&self) -> &'static str {
        match self {
            Self::InvalidInput(..) => "INVALID_INPUT",
            Self::StateUnavailable => "STATE_UNAVAILABLE",
            Self::DuplicateImport => "DUPLICATE_IMPORT",
            Self::NotOpen => "NOT_OPEN",
            Self::Io { .. } => "IO",
            Self::Core(..) => "CORE",
            Self::Workflow(error) => error.log_code(),
            Self::Import(error) => error.log_code(),
            Self::ImportFileFailed { .. } => "IMPORT_FILE_FAILED",
            Self::ImportCancelled { .. } => "IMPORT_CANCELLED",
            Self::CreateFailed { .. } => "CREATE_FAILED",
            Self::UnexpectedResponse => "UNEXPECTED_RESPONSE",
        }
    }
}

/// 将核心业务错误归类为预期竞争、取消或内部故障。
/// 入参：error（&VeilError）为底层错误。
/// 返回值：取消为None；常见路径/版本/锁/容器/卷冲突为Warn；其余Error；批失败递归判内部原因。
/// 异常：无错误返回。
/// 注意事项：仅供日志分级，不改变对客户端返回的错误或决定自动重试。
fn core_log_level(error: &veil_core::error::VeilError) -> Option<veil_core::logging::Level> {
    use veil_core::{error::VeilError, logging::Level};
    match error {
        VeilError::Cancelled => None,
        VeilError::ImportBatchFailed { source, .. } => core_log_level(source),
        VeilError::FileAlreadyExists(_)
        | VeilError::VersionConflict(_)
        | VeilError::LockConflict { .. }
        | VeilError::FileNotFound(_)
        | VeilError::VeilNotFound(_)
        | VeilError::VeilNameAmbiguous(_)
        | VeilError::VolumeUnavailable(_) => Some(Level::Warn),
        _ => Some(Level::Error),
    }
}

/// 解锁会话的快照：一次成功解锁后内存中保留的三个事实。
/// password 用 Zeroizing 包装——字符串被丢弃前先清零，降低内存泄密风险。
#[derive(Clone)]
struct Session {
    /// 容器目录路径（veil.db 与密文所在）。
    path: PathBuf,
    /// 容器稳定 ID。
    veil_id: String,
    /// 会话持有的密码副本，释放此包装时零化；Session.clone产生独立副本，在途任务可能晚于主会话释放。
    password: Zeroizing<String>,
}

/// 垃圾清理信号队列容量：最多积压 64 个清理请求（超出丢弃，清理是低优先级任务）。
const GARBAGE_QUEUE_CAPACITY: usize = 64;
/// 每批最多合并处理的清理信号数。
const GARBAGE_BATCH_COUNT: usize = 16;
/// 批次间隔：信号不足时最多等 30 秒凑一批。
const GARBAGE_BATCH_INTERVAL: Duration = Duration::from_secs(30);

/// 只保存清理信号；密码仅由消费者从当前已解锁会话中短暂读取。
///
/// 队列作用：削峰/异步——删除文件后触发一次清理信号；后台线程按批次（数量或
/// 时间阈值）合并执行 veil.db 中失效密文的清理扫描，避免每次删除都同步扫描。
/// 信号本身不带密码，消费者执行前才从会话里短暂取用。
struct GarbageCollectionQueue {
    /// 信号发送端（try_send 非阻塞，满了就丢弃本轮清理机会）。
    sender: veil_core::queue::Sender<()>,
    /// 后台消费线程句柄（Drop 时关闭队列并 join）。
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl GarbageCollectionQueue {
    /// 建立清理信号队列和消费者，合并信号后对当时会话扫描垃圾密文。
    /// 入参：session（`Arc<Mutex<Option<Session>>>`）为当前解锁状态；消费者保留此Arc，读取时复制Session和密码。
    /// 返回值：GarbageCollectionQueue，容量64信号、每批最多16、首信号等待阈值30秒。
    /// 异常：线程创建失败可能panic；消费错误不经此构造接口返回。
    /// 注意事项：队列消息只是()信号，不绑定发送时的容器；每批读取当时会话。获得生命周期锁失败会结束本批且不自投下一轮；成功进入清理后尝试再投一条，不保证固定周期或永不漏轮。
    fn new(session: Arc<Mutex<Option<Session>>>) -> Self {
        let (sender, receiver) =
            veil_core::queue::BoundedQueue::new(GARBAGE_QUEUE_CAPACITY).split();
        // recurring 用于清理完成后自投喂下一轮信号，实现周期扫描。
        let recurring = sender.clone();
        let worker = std::thread::spawn(move || {
            let _ = receiver.listen_batches(
                // 批量策略：攒够 16 条或等满 30 秒就出一批。
                veil_core::queue::BatchPolicy {
                    count: GARBAGE_BATCH_COUNT,
                    interval: GARBAGE_BATCH_INTERVAL,
                },
                |_| 1,
                |state| {
                    Ok::<_, ()>(veil_core::queue::ListenControl::new(
                        GARBAGE_BATCH_COUNT,
                        state.closed,
                    ))
                },
                // 批次回调：每条信号只表示「该扫描一次」。
                |_| {
                    // 消费时短暂读取会话快照（密码只在本次 cleanup 使用）。
                    let session = session.lock().ok().and_then(|session| session.clone());
                    if let Some(session) = session {
                        // 清理前先拿容器生命周期锁：正在被删除的容器不应再清理。
                        let Ok(_container_locks) =
                            VeilService::space_lock_for_container(&session.veil_id)
                        else {
                            return Ok(());
                        };
                        let manager = VeilManager::for_veil(session.path, session.veil_id);
                        // 执行失效密文清理（结果不影响会话，失败静默跳过）。
                        let _ = manager.cleanup(session.password.as_str());
                        // 会话保持解锁时，周期性发送下一轮低优先级扫描。
                        let _ = recurring.try_send(());
                    }
                    Ok(())
                },
            );
        });
        Self {
            sender,
            worker: Mutex::new(Some(worker)),
        }
    }

    /// 尽力触发一次后台清理，不让业务请求等扫描完成。
    /// 入参：self（&GarbageCollectionQueue）为清理发送端。
    /// 返回值：无，try_send结果被忽略。
    /// 异常：满队列、关闭和投递失败不返回调用者。
    /// 注意事项：不等待容量但仍可能竞争队列Mutex；若没有解锁会话，本次消费不会扫描，也不自投下一轮信号。
    fn schedule(&self) {
        let _ = self.sender.try_send(());
    }
}

impl Drop for GarbageCollectionQueue {
    /// 关闭清理队列，并在可取到线程句柄时等待消费者退出。
    /// 入参：self（&mut GarbageCollectionQueue）为最后释放的队列对象。
    /// 返回值：无。
    /// 异常：workerMutex中毒时跳过join；消费者panic的join结果忽略。
    /// 注意事项：可能等待正在进行的清理及已排信号处理；关闭后自投信号失败，不是无限续订。
    fn drop(&mut self) {
        self.sender.close();
        if let Ok(worker) = self.worker.get_mut() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
}

/// 尝试为单个文件准备加密并入提交队列，同名时以哈希判断是否跳过。
/// 入参：manager/password 为目标容器；file（&AddFileSpec）为本地来源及内部目标；directory_guard 为可复用目录守卫/范围；transaction_gate（&`Mutex<()>`）协调准备与提交；batch 为共享缓存/完成队列/持锁身份。
/// 返回值：true表示比较认为相同且再次观察目标存在而跳过；false表示准备结果已入队，尚未保证文件引用提交。
/// 异常：首次非同名错误传播；同名且内容不同/不可判定或比较时源目标缺失映射FileAlreadyExists；其他比较/投递错误传播。
/// 注意事项：目标比较与再次存在检查不在同一事务，仍需后续提交版本保护；同名路径若在比较后消失会再次尝试写入，失败不自动回队。
fn import_one_file(
    manager: &VeilManager,
    file: &crate::veil_ops::AddFileSpec,
    password: &str,
    directory_guard: Option<(
        &crate::storage::locks::LeaseGuard,
        &crate::veil_ops::LockedDirectory,
    )>,
    transaction_gate: &Mutex<()>,
    batch: &crate::veil_ops::ImportBatchContext,
) -> veil_core::error::Result<bool> {
    // 第一次尝试：流式加密写暂存 + 入提交批次（此处不真正提交）。
    let result = manager.add_files_for_import(
        std::slice::from_ref(file),
        password,
        directory_guard,
        transaction_gate,
        batch,
    );
    match result {
        Ok(_) => Ok(false),
        // 目标已被占用：进入内容比较流程（进度进入 verifying 阶段）。
        Err(veil_core::error::VeilError::FileAlreadyExists(_)) => {
            veil_core::progress::verifying();
            use crate::comparison::{
                ComparisonChecks, ConflictOutcome, FileInput, check_file_conflict,
                check_path_conflict,
            };
            // 只按哈希判断「内容是否相同」（省去逐字节比较）。
            let result = check_file_conflict(
                manager,
                &file.target,
                password,
                FileInput::External {
                    path: &file.source,
                    version: None,
                },
                ComparisonChecks {
                    hash: true,
                    ..Default::default()
                },
                directory_guard,
            );
            match result {
                Ok(ConflictOutcome::NoConflict) => {
                    // 内容相同：目标仍在 ⇒ 跳过；目标已被移除 ⇒ 重试写入。
                    if check_path_conflict(manager, &file.target, password, directory_guard)? {
                        Ok(true)
                    } else {
                        // 检测期间目标被移除时重新导入，不能误记为已完成。
                        manager.add_files_for_import(
                            std::slice::from_ref(file),
                            password,
                            directory_guard,
                            transaction_gate,
                            batch,
                        )?;
                        Ok(false)
                    }
                }
                // 内容不同（或文件已消失）：把冲突还原成 FileAlreadyExists 交给上层。
                Ok(_) | Err(veil_core::error::VeilError::FileNotFound(_)) => Err(
                    veil_core::error::VeilError::FileAlreadyExists(file.target.clone()),
                ),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

/// 容器展示快照（序列化为 camelCase JSON 交给客户端）：
/// 名称、ID、目录路径、创建时间、文件列表与目录列表。这是「某一时刻的
/// 一致展示副本」，不定义磁盘元数据格式，也不跨请求缓存目录树。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerView {
    /// 容器展示名称。
    pub name: String,
    /// 容器稳定 ID。
    pub id: String,
    /// 容器目录路径。
    pub path: String,
    /// 容器原始创建时间。
    pub created_at: String,
    /// 文件列表（按名称排序的展示快照）。
    pub files: Vec<FileView>,
    /// 目录展示条目集合；每项DirectoryEntry含节点ID和逻辑路径，保留元数据输入顺序。
    pub directories: Vec<veil_core::metadata::DirectoryEntry>,
}

/// 单个文件的展示视图：身份、名称、明文大小、版本链长度、加密时间、
/// 分类与预览 MIME（按扩展名推断）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileView {
    /// 文件稳定节点 ID（改名/移动/重加密不变）。
    pub id: String,
    /// 容器内部完整逻辑路径，不只是最后一级文件名。
    pub name: String,
    /// 明文大小（字节）。
    pub size: u64,
    /// 内容版本数（版本链长度）。
    pub version: usize,
    /// 当前密文生成时间。
    pub encrypted_at: String,
    /// 展示分类：photo/video/document/archive/other。
    pub category: &'static str,
    /// 支持内置预览时的 MIME 类型；不支持为 None。
    pub preview_mime: Option<&'static str>,
}

impl VeilService {
    /// 在执行前登记本次导入的来源集合与目标，拒绝同进程重复请求。
    /// 入参：self 提供会话目录快照；sources（&[String]）每项本地来源；directory（&str）为空表示容器根，否则规范化。
    /// 返回值：ImportGuard，持有本次唯一ImportKey，释放时移除。
    /// 异常：未解锁为NotOpen；路径解析、目标非法或状态锁中毒返回相应错误；键已存在为DuplicateImport。
    /// 注意事项：来源只std::path::absolute后排序去重，不canonicalize符号链接，不完全消除“..”；内容变化不改变任务键。容器路径从独立RwLock读，后续必须再核对会话未切换。
    fn register_import(
        &self,
        sources: &[String],
        directory: &str,
    ) -> Result<ImportGuard, ServiceError> {
        // 目标目录规范化（空串保持空串，表示根目录）。
        let destination = if directory.is_empty() {
            String::new()
        } else {
            crate::veil_ops::normalize_veil_dir(directory)?
        };
        // 会话容器目录来自字段 3 的快照（只读锁快速读取）。
        let container = self
            .3
            .read()
            .map_err(|_| ServiceError::StateUnavailable)?
            .clone()
            .ok_or(ServiceError::NotOpen)?;
        // 来源逐一转为绝对写法；不解析符号链接，含“..”或别名的同一来源仍可能得到不同键。
        let mut source_paths = Vec::with_capacity(sources.len());
        for source in sources {
            source_paths.push(
                std::path::absolute(source).map_err(|source| ServiceError::Io {
                    operation: "无法解析导入来源路径",
                    source,
                })?,
            );
        }
        // 排序 + 去重：顺序无关、重复选择不算新任务。
        source_paths.sort();
        source_paths.dedup();
        let key = ImportKey {
            container,
            source_paths,
            destination,
        };
        let registry = import_registry();
        let mut active = registry
            .lock()
            .map_err(|_| ServiceError::StateUnavailable)?;
        // insert 返回 false 说明同键任务已在运行。
        if !active.insert(key.clone()) {
            return Err(ServiceError::DuplicateImport);
        }
        drop(active);
        Ok(ImportGuard {
            active: registry,
            key,
        })
    }

    /// 启动本进程执行身份查询入口，供其他进程核对租约持有者。
    /// 入参：self 为服务句柄，本方法不需要已解锁会话。
    /// 返回值：成功返回ExecutionServer，包含回环端点与凭证。
    /// 异常：执行运行时初始化、监听/随机凭证/线程创建失败返回io::Error。
    /// 注意事项：调用方须持有返回句柄保持服务；不是服务全局自动单例，释放句柄将同步停止对应监听线程。
    pub fn listen_execution(&self) -> std::io::Result<crate::execution::ExecutionServer> {
        crate::execution::ExecutionService::global()?.listen()
    }

    /// 接收统一服务请求，附加诊断上下文后执行，并按规则记录失败。
    /// 入参：request（ServiceRequest）按值消费，变体数据描述业务参数；借用字段须在同步调用期间有效。
    /// 返回值：成功返回对应ServiceResponse，失败返回原ServiceError。
    /// 异常：配置/会话/容器/文件/工作流错误由内部传播，部分底层panic不捕获。
    /// 注意事项：同步入口，长传输会等待内部作用域线程完成，外部客户端应另行调度；创建/打开会话可在内存保留密码，不只是本次请求借用。日志仅为选中的传输变体附进度上下文。
    pub fn execute(&self, request: ServiceRequest<'_>) -> Result<ServiceResponse, ServiceError> {
        let _request = veil_core::logging::request_scope();
        let _secrets = veil_core::logging::sensitive_scope(&[]);
        // 逐变体登记密码为敏感值：日志输出时会被替换成占位符。
        match &request {
            ServiceRequest::CreateContainer { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            ServiceRequest::OpenContainer { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            ServiceRequest::MoveContainer { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            ServiceRequest::CreateInSpace { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            ServiceRequest::ImportPackageInSpace { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            ServiceRequest::CommitImport { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            ServiceRequest::CommitContainerPack { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            ServiceRequest::RegisterContainer { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            ServiceRequest::ValidateNewPassword { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            ServiceRequest::CommitCreate { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            ServiceRequest::CommitUnpack { password, .. } => {
                veil_core::logging::remember_sensitive(password)
            }
            _ => {}
        }
        // 记录开始时间用于耗时日志；is_transfer 标记传输类请求（失败日志要带进度）。
        let started = std::time::Instant::now();
        let is_transfer = matches!(
            &request,
            ServiceRequest::ImportSessionFiles { .. }
                | ServiceRequest::StreamSessionImport { .. }
                | ServiceRequest::StreamSessionExport { .. }
                | ServiceRequest::ExportSessionFile { .. }
                | ServiceRequest::ExportSessionPackage { .. }
                | ServiceRequest::ImportPackageInSpace { .. }
        );
        let operation = request.log_operation();
        let result = self.execute_inner(request);
        // 失败且达到日志级别阈值时记录结构化失败日志。
        if let Err(error) = &result {
            if let Some(level) = error.log_level(operation) {
                use veil_core::logging::{self, Field};
                // 基础上下文：耗时与结果标记。
                let mut context = vec![
                    ("elapsed_ms", Field::Duration(started.elapsed())),
                    ("outcome", Field::Label("failed")),
                ];
                // 传输类请求附加进度上下文，便于定位失败阶段。
                if is_transfer {
                    let progress = self.transfer_progress();
                    context.extend([
                        ("phase", Field::Label(progress.phase)),
                        ("completed_files", Field::Count(progress.completed_files)),
                        ("total_files", Field::Count(progress.total_files)),
                        ("parallel_workers", Field::Count(progress.parallel_workers)),
                    ]);
                }
                logging::failure(
                    level,
                    "service",
                    operation,
                    error.log_code(),
                    error,
                    &context,
                );
            }
        }
        result
    }

    /// 为请求建立去重与可选进度作用域，再登记执行身份并分派业务。
    /// 入参：request 为统一请求。
    /// 返回值：原样返回execute_registered结果；持有进度槽时按成功、失败或取消结束进度。
    /// 异常：去重、状态锁、执行上下文初始化或业务失败返回ServiceError；panic向外传播。
    /// 注意事项：只有会话普通/流式导入做全局去重；进度槽忙时仍继续执行本次请求，通常不绑定新槽，不能理解为禁止并行传输。进度和取消行为仍取决于各流程使用的共享Progress。
    fn execute_inner(&self, request: ServiceRequest<'_>) -> Result<ServiceResponse, ServiceError> {
        // 导入类请求先在全局登记簿登记（去重），守卫在函数结束时自动撤销。
        let _import = match &request {
            ServiceRequest::ImportSessionFiles { sources, directory }
            | ServiceRequest::StreamSessionImport { sources, directory } => {
                Some(self.register_import(sources, directory)?)
            }
            _ => None,
        };
        // 传输类请求的操作名（进度槽位标识）。
        let operation = match &request {
            ServiceRequest::ImportSessionFiles { .. }
            | ServiceRequest::StreamSessionImport { .. } => Some("import"),
            ServiceRequest::ExportSessionFile { .. }
            | ServiceRequest::StreamSessionExport { .. } => Some("export"),
            ServiceRequest::ImportPackageInSpace { .. } => Some("import_package"),
            ServiceRequest::ExportSessionPackage { .. } => Some("export_package"),
            _ => None,
        };
        // 只在开始前串行占用共享进度槽：已有传输在跑时不覆盖它的进度，本次请求静默执行。
        // 这里不能像以前那样把 self.2 一直持有到传输结束，否则导入期间预览/打开文件会一直排队。
        let _progress = match operation {
            Some(operation) => self.begin_transfer_progress(operation)?,
            None => None,
        };
        // 进入本进程的执行身份服务并登记当前任务：持锁操作的归属查询依赖该登记簿。
        let execution =
            crate::execution::ExecutionService::global().map_err(|source| ServiceError::Io {
                operation: "无法初始化执行上下文",
                source,
            })?;
        let _scope = execution.enter_task("VeilService::execute");
        // 真正分发：import_container 是登记键里的容器路径，供内部校验会话未切换。
        let result = self.execute_registered(
            request,
            _import.as_ref().map(|guard| guard.key.container.as_path()),
        );
        // 传输结束收尾：取消类错误标记 cancelled，否则标记成功/失败。
        if _progress.is_some() {
            if matches!(&result, Err(ServiceError::ImportCancelled { .. }))
                || (matches!(operation, Some("import" | "export"))
                    && matches!(
                        &result,
                        Err(ServiceError::Core(veil_core::error::VeilError::Cancelled))
                    ))
            {
                self.1.finish_cancelled();
            } else {
                self.1.finish(result.is_ok());
            }
        }
        result
    }

    /// 读取客户端用于展示的共享传输状态。
    /// 入参：self 为服务。
    /// 返回值：Snapshot副本，包含操作、阶段、计数和取消状态。
    /// 异常：无Result返回。
    /// 注意事项：读取进度内部Mutex，可能等待；只代表共享槽任务，不是本服务全部并发请求的汇总。
    pub fn transfer_progress(&self) -> veil_core::progress::Snapshot {
        self.1.snapshot()
    }

    /// 短时串行检查并尝试占用客户端共享进度槽。
    /// 入参：operation（&'static str）为固定传输类型标签。
    /// 返回值：已识别传输操作处于指定活动阶段时返回None；否则Some(Scope)开始新进度并绑定当前线程。
    /// 异常：串行Mutex中毒返回StateUnavailable。
    /// 注意事项：串行锁仅保护检查/占用，不持到传输结束；None不拒绝业务、不提供独立取消令牌，也不能保证各流程不再显式attach同一个Progress。
    fn begin_transfer_progress(
        &self,
        operation: &'static str,
    ) -> Result<Option<veil_core::progress::Scope>, ServiceError> {
        // 串行检查 + 登记：两个传输同时开始时只有第一个能拿到进度槽。
        let _serial = self.2.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let snapshot = self.1.snapshot();
        // 活动阶段集合：只有这些阶段内的传输才算「正在运行」。
        let active = matches!(
            snapshot.operation,
            "import" | "export" | "export_package" | "import_package"
        ) && matches!(
            snapshot.phase,
            "preparing"
                | "processing"
                | "verifying"
                | "snapshot"
                | "packing"
                | "unpacking"
                | "finalizing"
        );
        if active {
            return Ok(None);
        }
        Ok(Some(self.1.begin(operation)))
    }

    /// 向共享进度槽请求协作取消当前可取消传输。
    /// 入参：self 为服务。
    /// 返回值：首次接受取消为true；无可取消任务或已请求取消为false。
    /// 异常：无Result返回。
    /// 注意事项：不强制停止线程或系统I/O，需各流程检查标志；已发布/提交成果不回滚，也不代表所有无进度请求均被取消。
    pub fn cancel_transfer(&self) -> bool {
        self.1.request_cancel()
    }

    /// 分派全部服务请求，协调本机会话、空间/容器操作与两阶段计划。
    /// 入参：request（ServiceRequest）为按值消费的请求；import_container（`Option<&Path>`）为导入登记时的目标目录快照，会话导入必须有值。
    /// 返回值：返回与请求对应的ServiceResponse；例如列表/计划/容器视图/冲突批次或Done。
    /// 异常：状态、校验、工作流、导入和底层VeilError转换为ServiceError传播；嵌套线程panic不捕获。
    /// 注意事项：各分支资源不同，不能统一视为无会话操作。删除先按需清本服务会话，再执行删除；创建/解包完成后打开失败可能仍已创建数据。流式导入/导出同步等待管线，轮询/决策需另一请求并发调用。
    fn execute_registered(
        &self,
        request: ServiceRequest<'_>,
        import_container: Option<&Path>,
    ) -> Result<ServiceResponse, ServiceError> {
        match request {
            ServiceRequest::GetUserSetting { key } => Ok(ServiceResponse::Setting(
                crate::storage::app::AppStore::global()?.user_setting(key)?,
            )),
            ServiceRequest::SetUserSetting { key, value } => {
                crate::storage::app::AppStore::global()?.set_user_setting(key, value)?;
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::GetSystemSetting { key } => Ok(ServiceResponse::Setting(
                crate::storage::app::AppStore::global()?.system_setting_view(key)?,
            )),
            ServiceRequest::RenameSpace { id, name } => {
                crate::storage::app::AppStore::global()?.rename_space(id, name)?;
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::SetDefaultSpace { id } => {
                crate::storage::app::AppStore::global()?.set_default_space(id)?;
                Ok(ServiceResponse::Done)
            }
            // 先锁定本服务中受影响主会话以释放自身使用锁；其他任务仍使用时，下层删除仍可拒绝。
            ServiceRequest::DeleteSpace { id } => {
                self.lock_if_space_open(&id)?;
                crate::spaces::delete_space(&id)?;
                Ok(ServiceResponse::Spaces(crate::spaces::list_spaces()?))
            }
            ServiceRequest::DeleteSpaceRecord { id } => {
                self.lock_if_space_open(&id)?;
                crate::spaces::delete_space_record(&id)?;
                Ok(ServiceResponse::Spaces(crate::spaces::list_spaces()?))
            }
            ServiceRequest::DeleteContainer { id } => {
                self.lock_if_open(&id)?;
                crate::spaces::delete_container(&id)?;
                Ok(ServiceResponse::Spaces(crate::spaces::list_spaces()?))
            }
            ServiceRequest::UnregisterContainer { id } => {
                crate::storage::app::AppStore::global()?.unregister(id)?;
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::MoveContainer {
                id,
                target_space,
                password,
            } => {
                let _session_change = self.2.lock().map_err(|_| ServiceError::StateUnavailable)?;
                crate::spaces::move_container(id, target_space, password)?;
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::ListRecents => Ok(ServiceResponse::Recents(
                crate::storage::app::AppStore::global()?.records("recent_div", 6)?,
            )),
            ServiceRequest::CheckExecution {
                owner,
                endpoint,
                timeout,
            } => {
                let execution = crate::execution::ExecutionService::global().map_err(|source| {
                    ServiceError::Io {
                        operation: "无法初始化执行上下文",
                        source,
                    }
                })?;
                Ok(ServiceResponse::ExecutionStatus(
                    execution.check_owner(owner, endpoint, timeout),
                ))
            }
            ServiceRequest::CreateContainer {
                parent,
                name,
                password,
            } => self
                .create(parent, name, password)
                .map(ServiceResponse::Container),
            ServiceRequest::OpenContainer { path, password } => {
                self.open(path, password).map(ServiceResponse::Container)
            }
            ServiceRequest::RefreshContainer => self.refresh().map(ServiceResponse::Container),
            ServiceRequest::LockContainer => {
                self.lock()?;
                Ok(ServiceResponse::Locked)
            }
            ServiceRequest::ListSpaces => {
                Ok(ServiceResponse::Spaces(crate::spaces::list_spaces()?))
            }
            ServiceRequest::CreateSpace { name, path } => {
                crate::spaces::create_space(&name, path.as_deref())?;
                Ok(ServiceResponse::Spaces(crate::spaces::list_spaces()?))
            }
            ServiceRequest::CreateInSpace {
                space,
                name,
                password,
            } => {
                let _space_lock = Self::space_lock_for_selector(&space)?;
                let password = Zeroizing::new(password);
                let created = crate::spaces::create_container(&space, &name, password.as_str())?;
                self.open(
                    created.directory.to_string_lossy().into_owned(),
                    password.to_string(),
                )
                .map(ServiceResponse::Container)
            }
            ServiceRequest::ImportSessionFiles { sources, directory } => self
                .import_sources(
                    sources,
                    directory,
                    import_container.ok_or(ServiceError::NotOpen)?,
                )
                .map(ServiceResponse::Container),
            ServiceRequest::StreamSessionImport { sources, directory } => {
                let expected = import_container.ok_or(ServiceError::NotOpen)?;
                let session = self
                    .0
                    .lock()
                    .map_err(|_| ServiceError::StateUnavailable)?
                    .clone()
                    .ok_or(ServiceError::NotOpen)?;
                let _container_locks = Self::space_lock_for_container(&session.veil_id)?;
                if session.path != expected {
                    return Err(ServiceError::InvalidInput("容器已切换，请重新导入"));
                }
                let control = Arc::new(crate::import_pipeline::ImportConflictQueue::default());
                *self.4.lock().map_err(|_| ServiceError::StateUnavailable)? =
                    Some(Arc::clone(&control));
                let _registration = StreamingImportRegistration(&self.4);
                let mut options = crate::storage::locks::default_options();
                options.cancellation = Some(self.1.cancellation_flag());
                let manager =
                    VeilManager::for_veil(session.path, session.veil_id).with_lock_options(options);
                let result = crate::import_pipeline::run(
                    &manager,
                    session.password.as_str(),
                    sources,
                    directory,
                    &self.1,
                    &control,
                );
                result.map(ServiceResponse::Container)
            }
            ServiceRequest::CreateSessionDirectory { directory, name } => self
                .create_session_directory(&directory, &name)
                .map(ServiceResponse::Container),
            ServiceRequest::PollSessionImportConflicts => {
                let control = self
                    .4
                    .lock()
                    .map_err(|_| ServiceError::StateUnavailable)?
                    .clone();
                Ok(ServiceResponse::ImportConflicts(control.and_then(
                    |control| control.poll(self.1.snapshot().total_files),
                )))
            }
            ServiceRequest::ResolveSessionImportConflicts { id, resolutions } => {
                let control = self
                    .4
                    .lock()
                    .map_err(|_| ServiceError::StateUnavailable)?
                    .clone()
                    .ok_or(ServiceError::InvalidInput("导入任务已结束"))?;
                control.resolve(&id, resolutions)?;
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::PreviewSessionFile { name } => {
                self.preview_file(&name).map(|(bytes, unstable)| {
                    if unstable {
                        ServiceResponse::PreviewUnstable(bytes)
                    } else {
                        ServiceResponse::Preview(bytes)
                    }
                })
            }
            ServiceRequest::ExportSessionFile { name, destination } => {
                let unstable = self.export_file(&name, &destination)?;
                Ok(if unstable {
                    ServiceResponse::ExportUnstable
                } else {
                    ServiceResponse::Done
                })
            }
            ServiceRequest::StreamSessionExport { names, destination } => {
                let session = self
                    .0
                    .lock()
                    .map_err(|_| ServiceError::StateUnavailable)?
                    .clone()
                    .ok_or(ServiceError::NotOpen)?;
                let _container_locks = Self::space_lock_for_container(&session.veil_id)?;
                let control = Arc::new(crate::transfer_conflicts::ConflictQueue::default());
                *self.5.lock().map_err(|_| ServiceError::StateUnavailable)? =
                    Some(Arc::clone(&control));
                /// 本次流式导出的冲突槽登记守卫；仅存槽位引用，退出时摘除队列，不撤销已发布成果。
                struct Registration<'a>(
                    &'a Mutex<
                        Option<
                            Arc<
                                crate::transfer_conflicts::ConflictQueue<
                                    crate::export_pipeline::Work,
                                >,
                            >,
                        >,
                    >,
                );
                impl Drop for Registration<'_> {
                    /// 移除本次流式导出登记的审核队列。
                    /// 入参：self（&mut Registration）借用服务导出冲突槽Mutex。
                    /// 返回值：无，槽位置None。
                    /// 异常：锁中毒时取回状态，不返回错误。
                    /// 注意事项：请求作用域结束即清槽，不回滚已发布文件或取消其他请求。
                    fn drop(&mut self) {
                        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    }
                }
                // 登记仅覆盖本请求作用域；完成/错误退出都会移除槽位。相同服务的并发导出仍共用这个槽。
                let _registration = Registration(&self.5);
                let mut options = crate::storage::locks::default_options();
                options.cancellation = Some(self.1.cancellation_flag());
                let manager =
                    VeilManager::for_veil(session.path, session.veil_id).with_lock_options(options);
                crate::export_pipeline::run(
                    &manager,
                    &session.password,
                    names,
                    Path::new(&destination),
                    &self.1,
                    &control,
                    true,
                )?;
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::PollSessionExportConflicts => {
                let control = self
                    .5
                    .lock()
                    .map_err(|_| ServiceError::StateUnavailable)?
                    .clone();
                Ok(ServiceResponse::ImportConflicts(
                    control.and_then(|c| c.poll(self.1.snapshot().total_files)),
                ))
            }
            ServiceRequest::ResolveSessionExportConflicts { id, resolutions } => {
                let control = self
                    .5
                    .lock()
                    .map_err(|_| ServiceError::StateUnavailable)?
                    .clone()
                    .ok_or(ServiceError::InvalidInput("导出任务已结束"))?;
                control.resolve(&id, resolutions)?;
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::ExportSessionPackage { destination } => {
                let session = self.session_snapshot()?;
                let manager = VeilManager::for_veil(session.path.clone(), session.veil_id.clone());
                crate::package::VeilPacker::new(destination)
                    .without_overwrite()
                    .pack_container(&manager, session.password.as_str())?;
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::ImportPackageInSpace {
                path,
                space,
                password,
            } => {
                let _space_lock = Self::space_lock_for_selector(&space)?;
                crate::spaces::ensure_space_available(&space)?;
                let password = Zeroizing::new(password);
                let link = crate::config::AppConfig::config_path()?
                    .parent()
                    .ok_or(ServiceError::InvalidInput("应用目录无效"))?
                    .join("links")
                    .join(format!("{}.veil-link", crate::storage::id()));
                let plan = crate::workflow::prepare_unpack(crate::workflow::UnpackOptions {
                    package_path: &path,
                    name: None,
                    work_name: Some(&space),
                    link_output: link.to_str(),
                })?;
                // 此调用成功已还原并登记容器；下面open失败不会撤销已完成的还原。
                let unpacked = crate::workflow::commit_unpack(plan, password.as_str())?;
                self.open(
                    unpacked.directory.to_string_lossy().into_owned(),
                    password.to_string(),
                )
                .map(ServiceResponse::Container)
            }
            ServiceRequest::RemoveSessionPath { name } => {
                self.remove_path(&name).map(ServiceResponse::Container)
            }
            ServiceRequest::OperateContainer {
                directory,
                veil_id,
                operation,
            } => {
                let _space_lock = Self::space_lock_for_container(veil_id)?;
                let manager = VeilManager::for_veil(directory.to_path_buf(), veil_id);
                Ok(ServiceResponse::Operation(crate::operations::execute(
                    &manager, operation,
                )?))
            }
            ServiceRequest::PrepareContainer { selector } => Ok(
                ServiceResponse::ContainerPrepared(crate::commands::prepare_container(selector)?),
            ),
            ServiceRequest::OperatePrepared {
                container,
                operation,
            } => {
                let _space_lock = Self::space_lock_for_container(container.id())?;
                Ok(ServiceResponse::Operation(crate::commands::operate(
                    container, operation,
                )?))
            }
            ServiceRequest::PrepareImport {
                selector,
                source,
                destination,
            } => Ok(ServiceResponse::ImportPrepared(
                crate::commands::prepare_import(selector, source, destination)?,
            )),
            ServiceRequest::CommitImport { plan, password } => {
                let _space_lock = Self::space_lock_for_container(plan.container_id())?;
                Ok(ServiceResponse::Imported(crate::commands::commit_import(
                    plan, password,
                )?))
            }
            ServiceRequest::PrepareContainerPack { selector, output } => {
                Ok(ServiceResponse::ContainerPackPrepared(
                    crate::commands::prepare_pack(selector, output)?,
                ))
            }
            ServiceRequest::CommitContainerPack { plan, password } => {
                let _space_lock = Self::space_lock_for_container(plan.container_id())?;
                Ok(ServiceResponse::ContainerPacked(
                    crate::commands::commit_pack(plan, password)?,
                ))
            }
            ServiceRequest::ResolveContainer { selector } => {
                let mut config = crate::config::AppConfig::load()?;
                Ok(ServiceResponse::Resolved(config.resolve_veil(selector)?))
            }
            ServiceRequest::ReadConfigSummary => {
                Ok(ServiceResponse::ConfigSummary(crate::config::summary()?))
            }
            ServiceRequest::RegisterContainer {
                directory,
                password,
            } => {
                let metadata = VeilManager::new(directory.into()).read_meta(password)?;
                crate::config::AppConfig::load()?.register_directory(
                    directory,
                    &metadata.veil_id,
                    &metadata.veil_name,
                    &metadata.created_at,
                )?;
                Ok(ServiceResponse::Done)
            }
            ServiceRequest::ConfigPath => Ok(ServiceResponse::ConfigPath(
                crate::config::AppConfig::config_path()?,
            )),
            ServiceRequest::GuessMime { path } => {
                Ok(ServiceResponse::Mime(veil_core::mime::guess_mime(path)))
            }
            ServiceRequest::EffectiveHintLevel { override_value } => Ok(
                ServiceResponse::EffectiveHintLevel(crate::hints::effective_level(override_value)),
            ),
            ServiceRequest::ShouldShowHint {
                kind,
                override_value,
            } => Ok(ServiceResponse::ShowHint(crate::hints::should_show(
                kind,
                override_value,
            ))),
            ServiceRequest::SetHintLevel { level } => {
                let level = crate::config::HintLevel::parse(level)
                    .ok_or(crate::workflow::WorkflowError::InvalidHintLevel)?;
                crate::storage::app::AppStore::global()?
                    .set_user_setting("hint_level", &serde_json::json!(level.as_str()))?;
                Ok(ServiceResponse::HintLevelSet(level))
            }
            ServiceRequest::PrepareLink { target, output } => Ok(ServiceResponse::LinkPrepared(
                crate::link::prepare_link(target, output)?,
            )),
            ServiceRequest::CommitLink { plan } => Ok(ServiceResponse::LinkCreated(
                crate::link::commit_link(plan)?,
            )),
            ServiceRequest::CollectImport {
                source,
                destination,
            } => Ok(ServiceResponse::ImportFiles(
                crate::import::collect_add_specs(source, destination)?,
            )),
            ServiceRequest::PreparePack { name, output } => {
                let path = output
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(format!("{name}.vault.veil")));
                if path.exists() {
                    return Err(crate::workflow::WorkflowError::PackOutputExists(path).into());
                }
                Ok(ServiceResponse::PackPrepared(path))
            }
            // 只校验非空，不校验强度、不尝试打开任何容器，也不保存新密码。
            ServiceRequest::ValidateNewPassword { password } => {
                if password.is_empty() {
                    return Err(crate::workflow::WorkflowError::PasswordEmpty.into());
                }
                Ok(ServiceResponse::PasswordValid)
            }
            ServiceRequest::PrepareCreate { options } => Ok(ServiceResponse::CreatePrepared(
                crate::workflow::prepare_create(options)?,
            )),
            ServiceRequest::CommitCreate { plan, password } => Ok(ServiceResponse::Created(
                crate::workflow::commit_create(plan, password)?,
            )),
            ServiceRequest::PrepareUnpack { options } => Ok(ServiceResponse::UnpackPrepared(
                crate::workflow::prepare_unpack(options)?,
            )),
            ServiceRequest::CommitUnpack { plan, password } => Ok(ServiceResponse::Unpacked(
                crate::workflow::commit_unpack(plan, password)?,
            )),
        }
    }

    /// 在现有父目录下创建新身份容器、登记到空间并切换主会话。
    /// 入参：parent（String）为已存在目录；name（String）trim后须1至100字符；password（String）为非空密码，转入Zeroizing并在成功时留存Session。
    /// 返回值：成功返回新容器ContainerView。
    /// 异常：参数无效、父目录解析/目录创建、容器初始化/登记/生命周期或状态锁失败返回ServiceError；初始化错误带残留path。
    /// 注意事项：持串行和会话Mutex完成磁盘/数据库流程，其他会话访问可能等待。目录创建后不自动清理所有失败；路径快照、生命周期句柄、Session分步更新，状态锁失败可部分完成，不保证原会话绝无变化。
    fn create(
        &self,
        parent: String,
        name: String,
        password: String,
    ) -> Result<ContainerView, ServiceError> {
        let _session_change = self.2.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let password = Zeroizing::new(password);
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 100 {
            return Err(ServiceError::InvalidInput(
                "请输入 1 至 100 个字符的容器名称",
            ));
        }
        if password.is_empty() {
            return Err(ServiceError::InvalidInput("密码不能为空"));
        }
        let parent = PathBuf::from(parent);
        if !parent.is_dir() {
            return Err(ServiceError::InvalidInput("请选择存在的存储位置"));
        }
        let parent = std::fs::canonicalize(parent).map_err(|source| ServiceError::Io {
            operation: "无法访问保存位置",
            source,
        })?;
        let id = generate_veil_id();
        let _container_lock =
            crate::storage::app::AppStore::global()?.acquire_container_shared_lock(&id)?;
        let path = parent.join(&id);
        let mut session = self.0.lock().map_err(|_| ServiceError::StateUnavailable)?;
        std::fs::create_dir(&path).map_err(|source| ServiceError::Io {
            operation: "无法创建容器目录",
            source,
        })?;
        let manager = VeilManager::for_veil(path.clone(), id.clone());
        let metadata = manager
            .init_veil_with_id(&id, name, password.as_str())
            .map_err(|source| ServiceError::CreateFailed {
                path: path.clone(),
                source,
            })?;
        crate::config::AppConfig::load()?.register_directory(
            &path,
            &metadata.veil_id,
            &metadata.veil_name,
            &metadata.created_at,
        )?;
        let container_locks = Self::space_lock_for_container(&metadata.veil_id)?;
        let view = container_view(&path, &metadata);
        *self.3.write().map_err(|_| ServiceError::StateUnavailable)? = Some(path.clone());
        if let Some(previous) = session.as_ref() {
            veil_core::database::invalidate(&previous.path.join(veil_core::database::CONTAINER_DB));
        }
        *self.6.lock().map_err(|_| ServiceError::StateUnavailable)? = container_locks;
        *session = Some(Session {
            path,
            veil_id: id,
            password,
        });
        self.8.schedule();
        Ok(view)
    }

    /// 解锁并登记已有容器，取得生命周期共享锁后切换主会话。
    /// 入参：path（String）为存在的容器目录，canonicalize后保存；password（String）须非空，成功后由Session持有受零化包装副本。
    /// 返回值：ContainerView为持生命周期锁后重新读取的元数据视图。
    /// 异常：无效目录/密码、注册/读库/生命周期/状态锁失败返回ServiceError。
    /// 注意事项：串行锁与会话Mutex覆盖较长的开库和登记操作。先登记再取得锁并重读；失败可能已有本机登记/缓存变化。替换旧密码不清除其他任务快照或连接池副本。
    fn open(&self, path: String, password: String) -> Result<ContainerView, ServiceError> {
        let _session_change = self.2.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let password = Zeroizing::new(password);
        if password.is_empty() {
            return Err(ServiceError::InvalidInput("请输入容器密码"));
        }
        let path = PathBuf::from(path);
        if !path.is_dir() {
            return Err(ServiceError::InvalidInput("请选择有效的容器目录"));
        }
        let path = std::fs::canonicalize(path).map_err(|source| ServiceError::Io {
            operation: "无法访问容器目录",
            source,
        })?;
        let mut session = self.0.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let metadata = VeilManager::new(path.clone()).read_meta(password.as_str())?;
        crate::config::AppConfig::load()?.register_directory(
            &path,
            &metadata.veil_id,
            &metadata.veil_name,
            &metadata.created_at,
        )?;
        let space_lock = Self::space_lock_for_container(&metadata.veil_id)?;
        let metadata = VeilManager::new(path.clone()).read_meta(password.as_str())?;
        let view = container_view(&path, &metadata);
        *self.3.write().map_err(|_| ServiceError::StateUnavailable)? = Some(path.clone());
        if let Some(previous) = session.as_ref() {
            veil_core::database::invalidate(&previous.path.join(veil_core::database::CONTAINER_DB));
        }
        *self.6.lock().map_err(|_| ServiceError::StateUnavailable)? = space_lock;
        *session = Some(Session {
            path,
            veil_id: metadata.veil_id.clone(),
            password,
        });
        self.8.schedule();
        Ok(view)
    }

    /// 重新读取当前主会话容器的展示快照。
    /// 入参：self 为服务，要求当前会话已打开。
    /// 返回值：当前容器ContainerView。
    /// 异常：未打开为NotOpen，Mutex中毒为StateUnavailable，密码/身份/元数据读取错误传播。
    /// 注意事项：整个读取期间持会话Mutex，锁定或切换会等待；不修改Session身份，但底层open_db可更新展示缓存。
    fn refresh(&self) -> Result<ContainerView, ServiceError> {
        let session = self.0.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let session = session.as_ref().ok_or(ServiceError::NotOpen)?;
        let manager = VeilManager::for_veil(session.path.clone(), session.veil_id.clone());
        let metadata = manager.read_meta(session.password.as_str())?;
        Ok(container_view(&session.path, &metadata))
    }

    /// 读取本机批量提交设置后执行普通会话导入。
    /// 入参：sources（`Vec<String>`）每项本地文件/目录；directory（String）为内部导入根，空为根；expected_container（&Path）为登记时容器目录。
    /// 返回值：成功返回导入后ContainerView。
    /// 异常：设置读取、输入/会话/文件/事务/取消或最终视图读取失败返回ServiceError。
    /// 注意事项：普通导入先收集全部文件，与流式扫描队列入口不同；提交间隔和数量取自应用设置，已提交文件不随后续失败回滚。
    fn import_sources(
        &self,
        sources: Vec<String>,
        directory: String,
        expected_container: &Path,
    ) -> Result<ContainerView, ServiceError> {
        let settings = crate::storage::app::AppStore::global()?;
        let commit_interval =
            std::time::Duration::from_secs(settings.import_commit_interval_seconds()?);
        let commit_count = settings.import_commit_count()?;
        self.import_sources_with_interval(
            sources,
            directory,
            expected_container,
            commit_interval,
            commit_count,
        )
    }

    /// 收集全部来源后并行准备密文，由调用线程按数量/时间分批提交。
    /// 入参：sources 非空来源路径列表；directory 为内部目标目录，空为根；expected_container 为必须匹配的Session目录快照；commit_interval（Duration）为本批首项后等待阈值；commit_count（usize）须大于0，为本地待提交队列项数量阈值。
    /// 返回值：处理收敛且最后元数据读取成功返回ContainerView；同名内容相同的文件可自动跳过。
    /// 异常：输入/重复或互相包含目标、会话变化/锁/密码/文件/事务错误返回ServiceError；取消及文件失败携带完成/跳过数；子线程panic传播。
    /// 注意事项：不提供交互覆盖队列；文件清单全量驻内存，完成通道容量8，阈值触发还受调度/数据库影响。commit_count=0未在此验证，调用方须保证有效以免无法推进；已提交批次保留，取消仅在cancel_requested且未全完成时报告。
    fn import_sources_with_interval(
        &self,
        sources: Vec<String>,
        directory: String,
        expected_container: &Path,
        commit_interval: std::time::Duration,
        commit_count: usize,
    ) -> Result<ContainerView, ServiceError> {
        if sources.is_empty() {
            return Err(ServiceError::InvalidInput("请选择文件或文件夹"));
        }
        let directory = if directory.is_empty() {
            String::new()
        } else {
            crate::veil_ops::normalize_veil_dir(&directory)?
        };
        let session = self
            .0
            .lock()
            .map_err(|_| ServiceError::StateUnavailable)?
            .clone()
            .ok_or(ServiceError::NotOpen)?;
        let _container_locks = Self::space_lock_for_container(&session.veil_id)?;
        if session.path != expected_container {
            return Err(ServiceError::InvalidInput("容器已切换，请重新导入"));
        }
        let manager = VeilManager::for_veil(session.path.clone(), session.veil_id.clone());
        // 全量待导入规格：每项一个本地文件及内部目标；收集完成后按索引分配给工作线程。
        let mut files = Vec::new();
        // 每个来源目录在容器里的映射根；批量持共享写租约，文件来源不增加目录范围。
        let mut directory_paths = Vec::new();
        // 本次所有文件目标逻辑路径集合，用于拒绝同名映射和文件/后代路径冲突。
        let mut targets = std::collections::HashSet::new();
        for source in sources {
            if veil_core::progress::check_cancelled().is_err() {
                return Err(ServiceError::ImportCancelled {
                    completed: 0,
                    skipped: 0,
                });
            }
            let path = Path::new(&source);
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or(ServiceError::InvalidInput("文件名称无效"))?;
            let is_directory = path.is_dir();
            let destination = if is_directory {
                if directory.is_empty() {
                    name.to_owned()
                } else {
                    format!("{directory}/{name}")
                }
            } else if directory.is_empty() {
                String::new()
            } else {
                format!("{directory}/")
            };
            if is_directory {
                directory_paths.push(destination.clone());
            }
            let collected = crate::import::collect_add_specs(
                path,
                if destination.is_empty() {
                    None
                } else {
                    Some(&destination)
                },
            )
            .map_err(|error| match error {
                crate::import::ImportError::Cancelled => ServiceError::ImportCancelled {
                    completed: 0,
                    skipped: 0,
                },
                other => ServiceError::Import(other),
            })?;
            for spec in collected {
                if !targets.insert(spec.target.clone()) {
                    return Err(ServiceError::InvalidInput("选择的文件包含相同目标路径"));
                }
                files.push(spec.without_overwrite());
            }
        }
        if files.is_empty() {
            return Err(ServiceError::InvalidInput("选择的文件夹没有可添加的文件"));
        }
        // 明文总字节估计；任一来源metadata不可得或累加溢出即改None，文件处理时仍会再次读取来源。
        let mut total = Some(0u64);
        for file in &files {
            if veil_core::progress::check_cancelled().is_err() {
                return Err(ServiceError::ImportCancelled {
                    completed: 0,
                    skipped: 0,
                });
            }
            total = total.and_then(|total| total.checked_add(file.source.metadata().ok()?.len()));
        }
        for target in &targets {
            if veil_core::progress::check_cancelled().is_err() {
                return Err(ServiceError::ImportCancelled {
                    completed: 0,
                    skipped: 0,
                });
            }
            for (index, _) in target.match_indices('/') {
                if targets.contains(&target[..index]) {
                    return Err(ServiceError::InvalidInput("选择的文件包含冲突的目标路径"));
                }
            }
        }
        directory_paths.sort();
        directory_paths.dedup();
        let (directory_guard, locked_directories) = if directory_paths.is_empty() {
            (None, Vec::new())
        } else {
            match manager.lock_import_directories(
                &directory_paths,
                session.password.as_str(),
                self.1.cancellation_flag(),
            ) {
                Ok((guard, directories)) => (Some(Arc::new(guard)), directories),
                Err(_) if veil_core::progress::check_cancelled().is_err() => {
                    return Err(ServiceError::ImportCancelled {
                        completed: 0,
                        skipped: 0,
                    });
                }
                Err(error) => return Err(error.into()),
            }
        };
        veil_core::progress::start(total);
        veil_core::progress::set_file_count(files.len());
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let worker_count = veil_core::progress::worker_count(files.len());
        veil_core::progress::set_parallel_workers(worker_count);
        // 下一个待领取files索引；fetch_add确保同一请求只由一个工作线程领取。
        let next = AtomicUsize::new(0);
        let stopped = AtomicBool::new(false);
        let failure = Mutex::new(None);
        let transaction_gate = Mutex::new(());
        let owner = crate::execution::ExecutionService::global()
            .map_err(|source| ServiceError::Io {
                operation: "无法初始化导入执行上下文",
                source,
            })?
            .runtime()
            .current_context()
            .owner()
            .clone();
        /// 后台工作线程的退出守卫：负责减少运行数，并在 panic 时通知任务停止。
        struct WorkerEnd<'a> {
            /// 当前并行任务仍在运行的工作线程数量。
            remaining: Arc<AtomicUsize>,
            /// 共享停止标记；线程异常退出时置为 true。
            stopped: &'a AtomicBool,
        }
        impl Drop for WorkerEnd<'_> {
            /// 在普通导入工作线程退出时减少运行数，panic时同时通知管线停止。
            /// 入参：self（&mut WorkerEnd）含共享remaining原子计数和stopped引用。
            /// 返回值：无，每个工作线程退出一次就将remaining减1。
            /// 异常：不捕获原panic，不返回错误。
            /// 注意事项：Release写入与主线程Acquire读取配合；计数为0只表示加密工作线程结束，待提交队列可能尚未清空。
            fn drop(&mut self) {
                if std::thread::panicking() {
                    self.stopped.store(true, Ordering::Release);
                }
                self.remaining.fetch_sub(1, Ordering::Release);
            }
        }
        let logging_context = veil_core::logging::capture_context();
        std::thread::scope(|scope| {
            let logging_context = &logging_context;
            // 队列及待提交批次均限制数量，避免长间隔导入时无限累积租约。
            const QUEUE_LIMIT: usize = 8;
            let (sender, receiver) = veil_core::queue::BoundedQueue::new(QUEUE_LIMIT).split();
            let remaining_workers = Arc::new(AtomicUsize::new(worker_count));
            let directories = Arc::new(crate::directory_cache::DirectoryCache::default());
            for _ in 0..worker_count {
                let progress = self.1.clone();
                let manager = &manager;
                let files = &files;
                let next = &next;
                let stopped = &stopped;
                let failure = &failure;
                let transaction_gate = &transaction_gate;
                let password = session.password.as_str();
                let directory_guard = directory_guard.as_ref();
                let locked_directories = &locked_directories;
                let remaining_workers = Arc::clone(&remaining_workers);
                let context = crate::veil_ops::ImportBatchContext {
                    directories: directories.clone(),
                    sender: sender.clone(),
                    directory_guard: directory_guard.cloned(),
                    owner: owner.clone(),
                };
                scope.spawn(move || {
                    let _logging = logging_context.attach();
                    let _end = WorkerEnd {
                        remaining: remaining_workers,
                        stopped,
                    };
                    let _attachment = progress.attach();
                    loop {
                        if stopped.load(Ordering::Acquire)
                            || veil_core::progress::check_cancelled().is_err()
                        {
                            break;
                        }
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(file) = files.get(index) else { break };
                        veil_core::progress::begin_file(&file.target);
                        let directory = locked_directories
                            .iter()
                            .filter(|directory| directory.covers(&file.target))
                            .max_by_key(|directory| directory.path_len());
                        let scoped_guard = directory_guard.map(Arc::as_ref).zip(directory);
                        match import_one_file(
                            manager,
                            file,
                            password,
                            scoped_guard,
                            transaction_gate,
                            &context,
                        ) {
                            Ok(true) => veil_core::progress::file_done(true),
                            Ok(false) => {}
                            Err(veil_core::error::VeilError::Cancelled) => {
                                stopped.store(true, Ordering::Release);
                                break;
                            }
                            Err(error) => {
                                stopped.store(true, Ordering::Release);
                                let mut failure = failure.lock().unwrap_or_else(|e| e.into_inner());
                                if failure.is_none() {
                                    *failure = Some((file.target.clone(), error));
                                }
                                break;
                            }
                        }
                    }
                });
            }
            drop(sender);
            // 当前尚未提交的QueuedImport集合；此普通流程每个队列项由import_one_file准备一份文件。
            let mut pending = Vec::new();
            // 第一项进入当前pending时设置截止时刻；后续新项不延后它，提交/取消后重置。
            let mut deadline = None;
            loop {
                let remaining = deadline
                    .map(|at: std::time::Instant| {
                        at.saturating_duration_since(std::time::Instant::now())
                    })
                    .unwrap_or(std::time::Duration::from_millis(20));
                let wait = remaining.min(std::time::Duration::from_millis(20));
                let mut disconnected = false;
                if pending.len() < commit_count {
                    match receiver.recv_batch_timeout(commit_count - pending.len(), wait) {
                        Ok(items) => {
                            if pending.is_empty() {
                                deadline = Some(std::time::Instant::now() + commit_interval);
                            }
                            pending.extend(items);
                        }
                        Err(veil_core::queue::RecvError::Closed) => disconnected = true,
                        Err(
                            veil_core::queue::RecvError::Timeout
                            | veil_core::queue::RecvError::Empty,
                        ) => {}
                        Err(veil_core::queue::RecvError::Cancelled) => disconnected = true,
                    }
                } else if !wait.is_zero() {
                    std::thread::sleep(wait);
                }
                if !pending.is_empty() {
                    if remaining_workers.load(Ordering::Acquire) == 0 {
                        if let Ok(items) = receiver.try_recv_batch(commit_count - pending.len()) {
                            pending.extend(items);
                        }
                    }
                    let stop = stopped.load(Ordering::Acquire)
                        || veil_core::progress::check_cancelled().is_err();
                    if stop
                        || disconnected
                        || pending.len() >= commit_count
                        || remaining_workers.load(Ordering::Acquire) == 0
                        || deadline.is_some_and(|at| std::time::Instant::now() >= at)
                    {
                        // 数量/时间/断连/线程结束/停止任一条件达到，取走整批处理；队列中的其他项留待下一轮。
                        let batch = std::mem::take(&mut pending);
                        deadline = None;
                        if stop {
                            VeilManager::cancel_import_batch(batch);
                        } else {
                            let first_target = batch[0].target().to_owned();
                            let _gate = transaction_gate
                                .lock()
                                .unwrap_or_else(|error| error.into_inner());
                            if let Err(error) = manager.commit_import_batch(
                                batch,
                                session.password.as_str(),
                                &stopped,
                            ) {
                                stopped.store(true, Ordering::Release);
                                if !matches!(error, veil_core::error::VeilError::Cancelled) {
                                    let name = match &error {
                                        veil_core::error::VeilError::ImportBatchFailed {
                                            file,
                                            ..
                                        } => file.clone(),
                                        _ => first_target,
                                    };
                                    let mut failure =
                                        failure.lock().unwrap_or_else(|error| error.into_inner());
                                    if failure.is_none() {
                                        *failure = Some((name, error));
                                    }
                                }
                            }
                        }
                    }
                }
                if disconnected {
                    break;
                }
            }
        });
        if let Some(guard) = directory_guard.as_ref() {
            if let Ok(credentials) = guard.credentials() {
                for credential in credentials {
                    let _ = std::fs::remove_dir(
                        manager.veil_dir.join(".staging").join(credential.lock_id),
                    );
                }
            }
        }
        let progress = self.1.snapshot();
        let completed = progress.completed_files;
        let skipped = progress.skipped_files;
        if let Some((name, source)) = failure.into_inner().unwrap_or_else(|e| e.into_inner()) {
            drop(directory_guard);
            return Err(ServiceError::ImportFileFailed {
                name,
                completed,
                skipped,
                source,
            });
        }
        if progress.cancel_requested && completed < files.len() as u64 {
            return Err(ServiceError::ImportCancelled { completed, skipped });
        }
        veil_core::progress::finalizing();
        Ok(container_view(
            &session.path,
            &manager.read_meta(session.password.as_str())?,
        ))
    }

    /// 按扩展名白名单及大小上限读取会话文件明文，供客户端预览。
    /// 入参：name（&str）为容器文件逻辑路径。
    /// 返回值：(`Vec<u8>`, bool)，后项为unstable（普通只读标记），不是截断；当前底层ReadOnly成功通常为true。
    /// 异常：扩展名不支持、未解锁、超过16MiB、认证/读取/密码等错误返回ServiceError。
    /// 注意事项：超过限制直接拒绝，绝不截断；取得Session副本后不持会话Mutex解密，锁定不强制终止此读取。明文Vec不自动零化。
    fn preview_file(&self, name: &str) -> Result<(Vec<u8>, bool), ServiceError> {
        if file_kind(name).1.is_none() {
            return Err(ServiceError::InvalidInput("暂不支持预览此文件类型"));
        }
        let session = self.session_snapshot()?;
        let manager = VeilManager::for_veil(session.path.clone(), session.veil_id.clone());
        Ok(manager.read_file_bytes_with_status(
            name,
            session.password.as_str(),
            16 * 1024 * 1024,
        )?)
    }

    /// 把一个会话文件发布到精确外部目标，拒绝覆盖。
    /// 入参：name 为内部文件路径；destination（&str）为非空本地文件路径。
    /// 返回值：bool为底层unstable模式提示，当前申请SharedWrite，正常成功为false；不是截断或导出条数。
    /// 异常：目标空、未打开、占用、不支持发布、密码/锁/解密/文件/数据库错误传播。
    /// 注意事项：用Session快照完成操作，不持续持会话Mutex；单文件底层入口没有流式导出完整的父链/容器内部目标检查，调用方应提供合适位置。发布后同步失败可能已生成文件。
    fn export_file(&self, name: &str, destination: &str) -> Result<bool, ServiceError> {
        if destination.is_empty() {
            return Err(ServiceError::InvalidInput("请选择导出位置"));
        }
        let session = self.session_snapshot()?;
        let manager = VeilManager::for_veil(session.path.clone(), session.veil_id.clone());
        Ok(manager.extract_file_without_overwrite_with_status(
            name,
            Path::new(destination),
            session.password.as_str(),
        )?)
    }

    /// 逻辑删除会话节点后投递失引用密文，并返回刷新视图。
    /// 入参：name（&str）为容器内部文件或目录路径。
    /// 返回值：删除后重新读取的ContainerView。
    /// 异常：未解锁、状态/生命周期/路径/租约/数据库错误或最后视图读取失败返回ServiceError。
    /// 注意事项：持Session Mutex直到删除与刷新结束；回收异步且队列满不反馈，最后读取失败也不恢复已删除引用。
    fn remove_path(&self, name: &str) -> Result<ContainerView, ServiceError> {
        let session = self.0.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let session = session.as_ref().ok_or(ServiceError::NotOpen)?;
        let _space_lock = Self::space_lock_for_container(&session.veil_id)?;
        let manager = VeilManager::for_veil(session.path.clone(), session.veil_id.clone());
        let (_, ciphers) = manager.remove_path_deferred(name, session.password.as_str())?;
        self.7.enqueue(ciphers);
        self.8.schedule();
        Ok(container_view(
            &session.path,
            &manager.read_meta(session.password.as_str())?,
        ))
    }

    /// 在当前会话已有父目录下创建指定名称的子目录。
    /// 入参：directory（&str）为父逻辑路径，空为根；name（&str）须非空白且不是“.”/“..”，不含“/”、反斜杠或NUL，按原文保存。
    /// 返回值：创建后ContainerView；同路径已有目录可成功复用。
    /// 异常：名称/父类型无效、未解锁、生命周期/资源冲突、数据库或刷新失败返回ServiceError。
    /// 注意事项：先查询父类型再创建，实际创建事务重检结构；持会话Mutex阻塞其他主会话操作。无100字符或一般控制字符校验，返回错误可能目录已建成。
    fn create_session_directory(
        &self,
        directory: &str,
        name: &str,
    ) -> Result<ContainerView, ServiceError> {
        if name.trim().is_empty()
            || name == "."
            || name == ".."
            || name.contains(['/', '\\', '\0'])
        {
            return Err(ServiceError::InvalidInput("文件夹名称无效"));
        }
        let directory = if directory.is_empty() {
            String::new()
        } else {
            crate::veil_ops::normalize_veil_dir(directory)?
        };
        let session = self.0.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let session = session.as_ref().ok_or(ServiceError::NotOpen)?;
        let _space_lock = Self::space_lock_for_container(&session.veil_id)?;
        let manager = VeilManager::for_veil(session.path.clone(), session.veil_id.clone());
        if !matches!(
            manager.path_kind(&directory, session.password.as_str())?,
            crate::veil_ops::ContainerPathKind::Directory(_)
        ) {
            return Err(ServiceError::InvalidInput("目标位置不是有效文件夹"));
        }
        let path = if directory.is_empty() {
            name.to_owned()
        } else {
            format!("{directory}/{name}")
        };
        manager.create_directory(&path, session.password.as_str())?;
        Ok(container_view(
            &session.path,
            &manager.read_meta(session.password.as_str())?,
        ))
    }

    /// 复制当前解锁会话供长流程使用，取完立即释放会话Mutex。
    /// 入参：self 为服务，要求主会话存在。
    /// 返回值：Session独立副本，含路径、容器ID和Zeroizing密码字符串副本。
    /// 异常：未打开返回NotOpen；Mutex中毒为StateUnavailable。
    /// 注意事项：复制密码延长敏感数据寿命，之后锁定主会话不撤回此副本；本方法不额外取得生命周期锁，各调用路径另行协调。
    fn session_snapshot(&self) -> Result<Session, ServiceError> {
        self.0
            .lock()
            .map_err(|_| ServiceError::StateUnavailable)?
            .clone()
            .ok_or(ServiceError::NotOpen)
    }

    /// 串行清除主会话目录、密码及生命周期锁，并使本地数据库缓存失效。
    /// 入参：self 为服务，不要求已打开。
    /// 返回值：各状态更新完成返回Ok(())；未打开时也成功。
    /// 异常：串行/会话/路径/生命周期状态锁中毒返回StateUnavailable，前面更新可能已发生。
    /// 注意事项：顺序为清路径快照→invalidate→移除Session→清生命周期句柄，不是跨Mutex原子事务；不主动取消后台请求，不清除在途密码副本或中止借出连接。
    fn lock(&self) -> Result<(), ServiceError> {
        let _session_change = self.2.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let mut session = self.0.lock().map_err(|_| ServiceError::StateUnavailable)?;
        *self.3.write().map_err(|_| ServiceError::StateUnavailable)? = None;
        if let Some(current) = session.as_ref() {
            veil_core::database::invalidate(&current.path.join(veil_core::database::CONTAINER_DB));
        }
        // 清主会话会释放此密码副本，在途Session克隆仍各自存活。
        *session = None;
        *self.6.lock().map_err(|_| ServiceError::StateUnavailable)? = None;
        Ok(())
    }

    /// 为已登记容器取得空间共享锁和容器生命周期共享锁，避免删除期间使用。
    /// 入参：identity（&str）为容器稳定ID。
    /// 返回值：已登记返回Some(`Vec<File>`)，每项为必须持有的OS锁句柄；未登记返回None。
    /// 异常：movement字段存在（含null）、应用库/记录解析或锁获取失败返回ServiceError。
    /// 注意事项：读取移动状态/登记与取得锁非单事务；未登记None不自动保护外部目录。共享锁不等于业务文件租约，也不阻止独立复制。
    fn space_lock_for_container(
        identity: &str,
    ) -> Result<Option<Vec<std::fs::File>>, ServiceError> {
        let store = crate::storage::app::AppStore::global()?;
        if store
            .record::<serde_json::Value>("div_state", identity)?
            .is_some_and(|state| state.get("movement").is_some())
        {
            return Err(ServiceError::Core(crate::storage::invalid(
                "容器正在移动，请稍后重试",
            )));
        }
        let Some(registration) = store
            .registrations()?
            .into_iter()
            .find(|registration| registration.div_id == identity)
        else {
            return Ok(None);
        };
        let mut locks = store.acquire_space_shared_lock(&registration.space_id)?;
        locks.push(store.acquire_container_shared_lock(identity)?);
        Ok(Some(locks))
    }

    /// 按空间ID或名称选择首个匹配项并持其共享生命周期锁。
    /// 入参：selector（&str）为精确空间ID或名称。
    /// 返回值：`Vec<File>`，持有至业务结束，释放句柄归还OS锁。
    /// 异常：无匹配为InvalidInput；应用库或锁操作失败传播。
    /// 注意事项：使用find，不检测多命中/ID优先；后续业务可再做唯一性校验，不能仅凭此函数认为选择唯一。
    fn space_lock_for_selector(selector: &str) -> Result<Vec<std::fs::File>, ServiceError> {
        let store = crate::storage::app::AppStore::global()?;
        let space_id = store
            .spaces()?
            .into_iter()
            .find(|space| space.id == selector || space.name == selector)
            .map(|space| space.id)
            .ok_or(ServiceError::InvalidInput("空间不存在"))?;
        Ok(store.acquire_space_shared_lock(&space_id)?)
    }

    /// 删除容器前仅清除本服务中同身份的主会话。
    /// 入参：identity（&str）为要比较的容器ID。
    /// 返回值：不匹配无操作也返回Ok(())；匹配则清会话、路径和生命周期句柄。
    /// 异常：串行或状态锁中毒为StateUnavailable，可能已部分清理。
    /// 注意事项：不会锁定其他服务实例或取消在途任务；若其他使用者仍持生命周期锁，后续物理删除仍可能失败。
    fn lock_if_open(&self, identity: &str) -> Result<(), ServiceError> {
        let _session_change = self.2.lock().map_err(|_| ServiceError::StateUnavailable)?;
        let mut session = self.0.lock().map_err(|_| ServiceError::StateUnavailable)?;
        if session
            .as_ref()
            .is_some_and(|current| current.veil_id == identity)
        {
            if let Some(current) = session.as_ref() {
                veil_core::database::invalidate(
                    &current.path.join(veil_core::database::CONTAINER_DB),
                );
            }
            *session = None;
            *self.3.write().map_err(|_| ServiceError::StateUnavailable)? = None;
            *self.6.lock().map_err(|_| ServiceError::StateUnavailable)? = None;
        }
        Ok(())
    }

    /// 若待删空间路径与当前容器所属空间重叠，则尝试锁定此主会话。
    /// 入参：space_identity（&str）为目标空间ID。
    /// 返回值：空间未绑定/不存在、不重叠或相应会话清理成功均为Ok(())。
    /// 异常：应用库查询、记录或状态锁错误传播。
    /// 注意事项：按登记路径前缀双向比较，不canonicalize，不直接检查Session实际目录；读取身份到lock_if_open之间可切换，后者会重比ID。没有持续锁覆盖整个空间删除。
    fn lock_if_space_open(&self, space_identity: &str) -> Result<(), ServiceError> {
        let store = crate::storage::app::AppStore::global()?;
        let spaces = store.spaces()?;
        let Some(target_path) = spaces
            .iter()
            .find(|space| space.id == space_identity)
            .and_then(|space| space.local_path.as_deref())
        else {
            return Ok(());
        };
        let active_identity = self
            .0
            .lock()
            .map_err(|_| ServiceError::StateUnavailable)?
            .as_ref()
            .map(|session| session.veil_id.clone());
        if let Some(identity) = active_identity {
            let active_space = store
                .registrations()?
                .into_iter()
                .find(|registration| registration.div_id == identity)
                .map(|registration| registration.space_id);
            if active_space
                .as_deref()
                .and_then(|id| spaces.iter().find(|space| space.id == id))
                .and_then(|space| space.local_path.as_deref())
                .is_some_and(|active_path| {
                    active_path.starts_with(target_path) || target_path.starts_with(active_path)
                })
            {
                self.lock_if_open(&identity)?;
            }
        }
        Ok(())
    }
}

/// 将核心元数据转换为不含密钥的客户端列表视图。
/// 入参：path（&Path）为容器目录；metadata（&MetaData）为调用方读取的身份/文件/目录快照。
/// 返回值：ContainerView，文件每项FileView并按逻辑名称排序，目录列表按元数据原顺序复制。
/// 异常：无Result错误返回。
/// 注意事项：仅转换不查库，不保证传入元数据源于一致事务；非UTF-8路径采用有损显示。文件name为完整内部逻辑路径，version为链长度而非版本UUID。
pub(crate) fn container_view(path: &Path, metadata: &MetaData) -> ContainerView {
    let mut files: Vec<_> = metadata
        .files
        .iter()
        .map(|file| FileView {
            id: file.file_id.clone(),
            name: file.original_name.clone(),
            size: file.size,
            version: file.version_history.len(),
            encrypted_at: file.encrypted_at.clone(),
            category: file_kind(&file.original_name).0,
            preview_mime: file_kind(&file.original_name).1,
        })
        .collect();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    ContainerView {
        name: metadata.veil_name.clone(),
        id: metadata.veil_id.clone(),
        path: path.to_string_lossy().into_owned(),
        created_at: metadata.created_at.clone(),
        files,
        directories: metadata.directories.clone(),
    }
}

/// 根据文件扩展名推导展示分类及允许的预览MIME。
/// 入参：name（&str）为文件名或内部路径，取最后扩展名并转ASCII小写。
/// 返回值：(&'static str, `Option<&'static str>`)，分类photo/video/document/archive/other；仅白名单格式有预览MIME。
/// 异常：无错误返回。
/// 注意事项：不读取文件内容、不验证真实格式，MIME仅为展示/预览路由提示；未识别或不可解析扩展名归other。
fn file_kind(name: &str) -> (&'static str, Option<&'static str>) {
    let ext = Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => ("photo", Some("image/jpeg")),
        "png" => ("photo", Some("image/png")),
        "gif" => ("photo", Some("image/gif")),
        "webp" => ("photo", Some("image/webp")),
        "heic" | "bmp" | "tif" | "tiff" => ("photo", None),
        "mp4" | "m4v" => ("video", Some("video/mp4")),
        "webm" => ("video", Some("video/webm")),
        "mov" => ("video", Some("video/quicktime")),
        "avi" | "mkv" => ("video", None),
        "pdf" => ("document", Some("application/pdf")),
        "txt" | "md" | "csv" | "log" => ("document", Some("text/plain")),
        "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" => ("document", None),
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" => ("archive", None),
        _ => ("other", None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 为交互导出测试启动请求线程，使测试主线程能轮询/决定冲突。
    /// 入参：app（&VeilService）为共享服务；names（`Vec<String>`）为内部选择；destination（&Path）为外部目标。
    /// 返回值：`JoinHandle<Result<ServiceResponse, ServiceError>>`，等待后取得最终请求结果。
    /// 异常：线程创建失败可panic，请求错误在线程结果内，线程panic经join体现。
    /// 注意事项：Clone共享同一状态；路径按有损字符串传入，仅测试使用，调用方负责结束或取消任务。
    fn run_stream_export(
        app: &VeilService,
        names: Vec<String>,
        destination: &Path,
    ) -> std::thread::JoinHandle<Result<ServiceResponse, ServiceError>> {
        let app = app.clone();
        let destination = destination.to_string_lossy().into_owned();
        std::thread::spawn(move || {
            app.execute(ServiceRequest::StreamSessionExport { names, destination })
        })
    }
    /// 为测试轮询导出冲突批次，直到出现待审核目标。
    /// 入参：app（&VeilService）为运行请求的共享服务。
    /// 返回值：ConflictBatch，含本批token、目标视图和发现文件数。
    /// 异常：轮询服务错误unwrap panic；观察超过20秒则请求取消并panic。
    /// 注意事项：每10ms重试，单次请求耗时也算在内；超时只发取消，不保证返回前工作线程已结束。
    fn await_export_conflicts(app: &VeilService) -> crate::import_pipeline::ConflictBatch {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if let ServiceResponse::ImportConflicts(Some(batch)) = app
                .execute(ServiceRequest::PollSessionExportConflicts)
                .unwrap()
            {
                return batch;
            }
            if std::time::Instant::now() > deadline {
                app.cancel_transfer();
                panic!("未收到导出冲突");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    /// 在测试中等待导出工作线程结束并读取最终业务结果。
    /// 入参：app用于超时发取消；task（`JoinHandle<Result<ServiceResponse, ServiceError>>`）为导出线程。
    /// 返回值：线程结束后返回其原Result。
    /// 异常：等待观察超过20秒请求取消并panic；线程自身panic经join.unwrap使测试失败。
    /// 注意事项：每10ms查is_finished；超时分支不join，协作取消不等于线程已退出。
    fn await_export_end(
        app: &VeilService,
        task: std::thread::JoinHandle<Result<ServiceResponse, ServiceError>>,
    ) -> Result<ServiceResponse, ServiceError> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !task.is_finished() {
            if std::time::Instant::now() > deadline {
                app.cancel_transfer();
                panic!("导出未结束");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        task.join().unwrap()
    }
    #[test]
    /// 验证目录导出按批次分页，保留空目录，并把重复选择折叠为单次导出。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn stream_export_pages_folders_preserves_empty_directories_and_deduplicates_selection() {
        let (root, manager, app, source) = stream_import_fixture();
        let specs = (0..150)
            .map(|i| crate::veil_ops::AddFileSpec::new(&source, format!("album/{i:03}")))
            .collect::<Vec<_>>();
        manager.add_files(&specs, "password").unwrap();
        manager
            .create_directory("album/nested/empty", "password")
            .unwrap();
        let output = root.path().join("output");
        let task = run_stream_export(
            &app,
            vec!["album".into(), "album/000".into(), "album".into()],
            &output,
        );
        await_export_end(&app, task).unwrap();
        assert!(output.join("album/nested/empty").is_dir());
        for i in 0..150 {
            assert_eq!(
                std::fs::read(output.join(format!("album/{i:03}"))).unwrap(),
                b"original"
            );
        }
        let p = app.transfer_progress();
        assert_eq!(
            (
                p.total_files,
                p.completed_files,
                p.checked_files,
                p.skipped_files
            ),
            (150, 150, 150, 0)
        );
        assert!(p.scan_complete);
        assert_eq!(p.phase, "completed");
    }
    #[test]
    /// 验证导出遇到冲突时等待用户选择，并在等待期间继续处理不冲突文件、复用目录锁。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn stream_export_waits_for_choices_and_reuses_directory_lock() {
        for action in [SessionImportAction::Skip, SessionImportAction::Overwrite] {
            let (root, manager, app, source) = stream_import_fixture();
            manager
                .add_files(
                    &[
                        crate::veil_ops::AddFileSpec::new(&source, "album/same"),
                        crate::veil_ops::AddFileSpec::new(&source, "album/clear"),
                    ],
                    "password",
                )
                .unwrap();
            let output = root.path().join("output");
            std::fs::create_dir_all(output.join("album")).unwrap();
            std::fs::write(output.join("album/same"), b"original").unwrap();
            let task = run_stream_export(&app, vec!["album".into()], &output);
            let batch = await_export_conflicts(&app);
            assert_eq!(batch.conflicts.len(), 1);
            assert!(batch.conflicts[0].same_content);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !output.join("album/clear").exists() {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let db = manager.open_db("password").unwrap();
            let resources = db
                .conn
                .prepare("SELECT resource_id FROM operation_locks")
                .unwrap()
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert_eq!(resources.len(), 1);
            assert!(resources[0].contains("veil-dir:"));
            let cached: i64 = db
                .conn
                .query_row("SELECT count(*) FROM file_hash", [], |r| r.get(0))
                .unwrap();
            // 夹具预先放入一个源文件，当前导出目录另有两个文件。
            assert_eq!(cached, 3);
            drop(db);
            app.execute(ServiceRequest::ResolveSessionExportConflicts {
                id: batch.id,
                resolutions: vec![SessionImportResolution {
                    target: batch.conflicts[0].target.clone(),
                    action,
                }],
            })
            .unwrap();
            await_export_end(&app, task).unwrap();
            assert_eq!(app.transfer_progress().completed_files, 2);
            assert_eq!(
                app.transfer_progress().skipped_files,
                if matches!(action, SessionImportAction::Skip) {
                    1
                } else {
                    0
                }
            );
            assert_eq!(
                std::fs::read(output.join("album/same")).unwrap(),
                b"original"
            );
        }
    }
    #[test]
    /// 验证取消时已发布文件保留，未发布的明文暂存内容被清理。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn stream_export_cancel_keeps_published_files_and_cleans_unpublished_plaintext() {
        let (root, manager, app, source) = stream_import_fixture();
        manager
            .add_files(
                &[
                    crate::veil_ops::AddFileSpec::new(&source, "album/same"),
                    crate::veil_ops::AddFileSpec::new(&source, "album/clear"),
                ],
                "password",
            )
            .unwrap();
        let output = root.path().join("output");
        std::fs::create_dir_all(output.join("album")).unwrap();
        std::fs::write(output.join("album/same"), b"keep").unwrap();
        let task = run_stream_export(&app, vec!["album".into()], &output);
        let _ = await_export_conflicts(&app);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.transfer_progress().completed_files == 0 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(app.cancel_transfer());
        assert!(await_export_end(&app, task).is_err());
        assert_eq!(app.transfer_progress().phase, "cancelled");
        assert_eq!(std::fs::read(output.join("album/same")).unwrap(), b"keep");
        assert_eq!(
            std::fs::read(output.join("album/clear")).unwrap(),
            b"original"
        );
        assert_eq!(std::fs::read_dir(output.join("album")).unwrap().count(), 2);
        assert!(matches!(
            app.execute(ServiceRequest::PollSessionExportConflicts)
                .unwrap(),
            ServiceResponse::ImportConflicts(None)
        ));
    }
    #[test]
    /// 验证复核后目标被外部改动时拒绝覆盖，并禁止把明文导出到容器内部。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn stream_export_rejects_changed_overwrite_target_and_internal_plaintext_destination() {
        let (root, manager, app, _) = stream_import_fixture();
        let output = root.path().join("out");
        std::fs::write(&output, b"before").unwrap();
        let task = run_stream_export(&app, vec!["file".into()], &output);
        let batch = await_export_conflicts(&app);
        std::fs::write(&output, b"changed after review").unwrap();
        app.execute(ServiceRequest::ResolveSessionExportConflicts {
            id: batch.id,
            resolutions: vec![SessionImportResolution {
                target: batch.conflicts[0].target.clone(),
                action: SessionImportAction::Overwrite,
            }],
        })
        .unwrap();
        assert!(await_export_end(&app, task).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"changed after review");
        assert_eq!(app.transfer_progress().completed_files, 0);
        assert!(
            app.execute(ServiceRequest::StreamSessionExport {
                names: vec!["file".into()],
                destination: manager
                    .veil_dir
                    .join("plain")
                    .to_string_lossy()
                    .into_owned()
            })
            .is_err()
        );
        assert!(!manager.veil_dir.join("plain").exists());
        assert_eq!(
            std::fs::read_dir(root.path())
                .unwrap()
                .filter(|e| e
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".tmp"))
                .count(),
            0
        );
    }
    #[test]
    /// 验证密文认证失败时删除部分导出文件，已有相邻目标不受影响。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn stream_export_authentication_failure_removes_partial_plaintext() {
        let (root, manager, app, _) = stream_import_fixture();
        let node = manager
            .open_db("password")
            .unwrap()
            .resolve("file")
            .unwrap()
            .unwrap();
        let cipher = manager.veil_dir.join(node.cipher().unwrap());
        let mut bytes = std::fs::read(&cipher).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(cipher, bytes).unwrap();
        let output = root.path().join("output");
        std::fs::create_dir(&output).unwrap();
        std::fs::write(output.join("keep"), b"existing").unwrap();
        assert!(
            app.execute(ServiceRequest::StreamSessionExport {
                names: vec!["file".into()],
                destination: output.join("exported").to_string_lossy().into_owned(),
            })
            .is_err()
        );
        assert!(!output.join("exported").exists());
        assert_eq!(std::fs::read_dir(&output).unwrap().count(), 1);
        assert_eq!(std::fs::read(output.join("keep")).unwrap(), b"existing");
        assert_eq!(app.transfer_progress().completed_files, 0);
    }

    #[test]
    /// 验证冲突队列达到上限后扫描仍会继续，并通过取消结束尚未决策的任务。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn stream_export_unbounded_discovery_continues_when_conflicts_are_full() {
        let (root, manager, app, source) = stream_import_fixture();
        manager
            .add_files(
                &(0..180)
                    .map(|i| crate::veil_ops::AddFileSpec::new(&source, format!("album/{i:03}")))
                    .collect::<Vec<_>>(),
                "password",
            )
            .unwrap();
        let output = root.path().join("output");
        std::fs::create_dir_all(output.join("album")).unwrap();
        for i in 0..180 {
            std::fs::write(output.join(format!("album/{i:03}")), b"original").unwrap();
        }
        let task = run_stream_export(&app, vec!["album".into()], &output);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let p = app.transfer_progress();
            if p.scan_complete && p.waiting_files == 128 {
                assert_eq!(p.total_files, 180);
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(app.cancel_transfer());
        assert!(await_export_end(&app, task).is_err());
        assert_eq!(app.transfer_progress().completed_files, 0);
        assert_eq!(
            std::fs::read_dir(output.join("album")).unwrap().count(),
            180
        );
    }
    /// 在测试后台线程运行一个来源的流式导入，保留主线程处理冲突。
    /// 入参：app为服务；source（&Path）为外部文件或目录。
    /// 返回值：请求线程句柄，其结果为ServiceResponse或ServiceError。
    /// 异常：线程创建失败可panic；业务错误留在线程结果。
    /// 注意事项：导入到容器根，来源路径采用有损显示；调用方负责取消/等待，丢弃句柄不会自动停止线程。
    fn run_stream_import(
        app: &VeilService,
        source: &Path,
    ) -> std::thread::JoinHandle<Result<ServiceResponse, ServiceError>> {
        let worker = app.clone();
        let source = source.to_string_lossy().into_owned();
        std::thread::spawn(move || {
            worker.execute(ServiceRequest::StreamSessionImport {
                sources: vec![source],
                directory: String::new(),
            })
        })
    }
    /// 为测试轮询流式导入冲突批次，直到出现待审核目标。
    /// 入参：app（&VeilService）为运行请求的共享服务。
    /// 返回值：ConflictBatch，含本批token、目标视图和发现文件数。
    /// 异常：轮询服务错误unwrap panic；观察超过20秒则请求取消并panic。
    /// 注意事项：每10ms重试，单次请求耗时也算在内；超时只发取消，不保证返回前工作线程已结束。
    fn await_stream_conflicts(app: &VeilService) -> crate::import_pipeline::ConflictBatch {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if let ServiceResponse::ImportConflicts(Some(batch)) = app
                .execute(ServiceRequest::PollSessionImportConflicts)
                .unwrap()
            {
                return batch;
            }
            if std::time::Instant::now() > deadline {
                app.cancel_transfer();
                panic!("未收到流式导入冲突");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    #[test]
    /// 验证导入冲突等待用户选择期间，无冲突文件仍能分批加密提交。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn stream_import_commits_clear_files_while_waiting_for_explicit_choices() {
        let (root, manager, app, source) = stream_import_fixture();
        let folder = root.path().join("album");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("same"), b"original").unwrap();
        std::fs::write(folder.join("new"), b"new").unwrap();
        manager
            .add_files(
                &[crate::veil_ops::AddFileSpec::new(&source, "album/same")],
                "password",
            )
            .unwrap();
        let worker = run_stream_import(&app, &folder);
        let batch = await_stream_conflicts(&app);
        assert_eq!(batch.conflicts.len(), 1);
        assert!(batch.conflicts[0].same_content);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while manager
            .read_file_bytes("album/new", "password", 100)
            .is_err()
        {
            if std::time::Instant::now() > deadline {
                app.cancel_transfer();
                panic!("待确认文件不应阻塞其他文件提交");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let db = manager.open_db("password").unwrap();
        let resources = db
            .conn
            .prepare("SELECT resource_id FROM operation_locks")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(resources.len(), 1);
        assert!(
            resources[0].contains("veil-dir:"),
            "文件夹导入应复用目录锁，不能另加文件锁"
        );
        drop(db);
        assert!(!worker.is_finished());
        app.execute(ServiceRequest::ResolveSessionImportConflicts {
            id: batch.id,
            resolutions: vec![SessionImportResolution {
                target: "album/same".into(),
                action: SessionImportAction::Skip,
            }],
        })
        .unwrap();
        worker.join().unwrap().unwrap();
        let progress = app.transfer_progress();
        assert_eq!(
            (
                progress.total_files,
                progress.completed_files,
                progress.skipped_files
            ),
            (2, 2, 1)
        );
        assert!(progress.scan_complete);
        assert_eq!(progress.waiting_files, 0);
    }
    #[test]
    /// 验证冲突确认后源文件版本发生变化时重新生成冲突批次，要求用户再次选择。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn stream_import_rechecks_changed_versions_and_requires_another_choice() {
        let (_root, manager, app, source) = stream_import_fixture();
        let worker = run_stream_import(&app, &source);
        let batch = await_stream_conflicts(&app);
        std::fs::write(&source, b"concurrent edit").unwrap();
        manager.add_file(&source, "password").unwrap();
        app.execute(ServiceRequest::ResolveSessionImportConflicts {
            id: batch.id.clone(),
            resolutions: vec![SessionImportResolution {
                target: "file".into(),
                action: SessionImportAction::Overwrite,
            }],
        })
        .unwrap();
        let again = await_stream_conflicts(&app);
        assert_ne!(again.id, batch.id);
        assert!(
            app.execute(ServiceRequest::ResolveSessionImportConflicts {
                id: batch.id,
                resolutions: vec![SessionImportResolution {
                    target: "file".into(),
                    action: SessionImportAction::Skip
                }]
            })
            .is_err()
        );
        app.execute(ServiceRequest::ResolveSessionImportConflicts {
            id: again.id,
            resolutions: vec![SessionImportResolution {
                target: "file".into(),
                action: SessionImportAction::Skip,
            }],
        })
        .unwrap();
        worker.join().unwrap().unwrap();
        assert_eq!(
            manager.read_file_bytes("file", "password", 100).unwrap(),
            b"concurrent edit"
        );
        assert_eq!(app.transfer_progress().skipped_files, 1);
    }

    #[test]
    /// 验证导入可连续处理多个有界冲突批次，并补齐可安全计算的缺失摘要。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn stream_import_resolves_multiple_bounded_batches_and_fills_missing_hashes() {
        let (root, manager, app, source) = stream_import_fixture();
        let folder = root.path().join("batches");
        std::fs::create_dir(&folder).unwrap();
        let mut specs = Vec::new();
        for index in 0..70 {
            std::fs::write(folder.join(format!("file-{index}")), b"original").unwrap();
            specs.push(crate::veil_ops::AddFileSpec::new(
                &source,
                format!("batches/file-{index}"),
            ));
        }
        manager.add_files(&specs, "password").unwrap();
        manager.open_db("password").unwrap().conn.execute("DELETE FROM file_hash WHERE file_id=(SELECT id FROM file_info WHERE name='file-0')", []).unwrap();
        let worker = run_stream_import(&app, &folder);
        let mut chosen = 0;
        while chosen < 70 {
            let batch = await_stream_conflicts(&app);
            assert!(batch.conflicts.len() <= 32);
            chosen += batch.conflicts.len();
            assert!(
                batch
                    .conflicts
                    .iter()
                    .all(|file| file.same_content && file.error.is_none())
            );
            app.execute(ServiceRequest::ResolveSessionImportConflicts {
                id: batch.id,
                resolutions: batch
                    .conflicts
                    .into_iter()
                    .map(|file| SessionImportResolution {
                        target: file.target,
                        action: SessionImportAction::Skip,
                    })
                    .collect(),
            })
            .unwrap();
        }
        worker.join().unwrap().unwrap();
        let progress = app.transfer_progress();
        assert_eq!(
            (
                progress.completed_files,
                progress.total_files,
                progress.skipped_files
            ),
            (70, 70, 70)
        );
        assert!(
            manager
                .list_files("password")
                .unwrap()
                .iter()
                .all(|file| file.hash.is_some())
        );
    }

    #[test]
    /// 验证冲突队列达到容量上限时文件扫描仍继续，待用户处理后再恢复提交。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn stream_import_unbounded_discovery_continues_while_conflict_queue_is_full() {
        let (root, manager, app, source) = stream_import_fixture();
        let folder = root.path().join("many");
        std::fs::create_dir(&folder).unwrap();
        let mut specs = Vec::new();
        for index in 0..600 {
            let path = folder.join(format!("file-{index}"));
            std::fs::write(&path, b"original").unwrap();
            specs.push(crate::veil_ops::AddFileSpec::new(
                &source,
                format!("many/file-{index}"),
            ));
        }
        manager.add_files(&specs, "password").unwrap();
        let worker = run_stream_import(&app, &folder);
        let _ = await_stream_conflicts(&app);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while (app.transfer_progress().waiting_files < 128
            || !app.transfer_progress().scan_complete)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let progress = app.transfer_progress();
        assert_eq!(progress.waiting_files, 128);
        assert_eq!(progress.total_files, 600);
        assert!(progress.scan_complete);
        assert_eq!(progress.completed_files, 0);
        assert!(app.cancel_transfer());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !worker.is_finished() {
            assert!(std::time::Instant::now() < deadline, "取消时队列未退出");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(matches!(
            worker.join().unwrap(),
            Err(ServiceError::ImportCancelled { .. })
        ));
    }

    /// 为会话传输测试创建真实容器、已打开服务状态和原始来源文件。
    /// 入参：无。
    /// 返回值：(TempDir, VeilManager, VeilService, PathBuf)：临时根守卫、管理器、手工填充Session的服务、含original字节的源文件；容器已添加同名file。
    /// 异常：准备中的临时文件/数据库/写入/状态访问失败经unwrap panic。
    /// 注意事项：仅测试，Debug开启进程级快速KDF；直接填Session和路径，不走正式登记/生命周期句柄设置。须保留TempDir到任务结束。
    fn stream_import_fixture() -> (tempfile::TempDir, VeilManager, VeilService, PathBuf) {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("container");
        let manager = VeilManager::new(path.clone());
        let metadata = manager.init_veil("test", "password").unwrap();
        let app = VeilService::default();
        *app.3.write().unwrap() = Some(path.clone());
        *app.0.lock().unwrap() = Some(Session {
            path,
            veil_id: metadata.veil_id.clone(),
            password: Zeroizing::new("password".into()),
        });
        let source = root.path().join("file");
        std::fs::write(&source, b"original").unwrap();
        manager.add_file(&source, "password").unwrap();
        (root, manager, app, source)
    }

    #[test]
    /// 验证导入达到时间间隔后提交、结束时刷新尾批，并在取消后注销冲突队列。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn import_flushes_on_interval_finishes_last_batch_and_cleans_cancelled_queue() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        for scenario in ["timer", "count", "last", "cancel"] {
            let root = tempfile::TempDir::new().unwrap();
            let path = root.path().join("container");
            let manager = VeilManager::new(path.clone());
            let metadata = manager.init_veil("test", "password").unwrap();
            let app = VeilService::default();
            *app.0.lock().unwrap() = Some(Session {
                path: path.clone(),
                veil_id: metadata.veil_id.clone(),
                password: Zeroizing::new("password".into()),
            });
            let count = if scenario == "last" { 1 } else { 24 };
            let sources = (0..count)
                .map(|index| {
                    let source = root.path().join(format!("file-{index}"));
                    std::fs::write(&source, vec![index as u8; 4096]).unwrap();
                    source.to_string_lossy().into_owned()
                })
                .collect();
            let worker = app.clone();
            let container = path.clone();
            let interval = if scenario == "timer" {
                std::time::Duration::from_millis(50)
            } else {
                std::time::Duration::from_secs(10)
            };
            let threshold = if scenario == "count" { 2 } else { 100 };
            let start = std::time::Instant::now();
            let task = std::thread::spawn(move || {
                let _scope = worker.1.begin("import");
                worker.import_sources_with_interval(
                    sources,
                    String::new(),
                    &container,
                    interval,
                    threshold,
                )
            });
            if scenario == "timer" {
                let limit = std::time::Instant::now() + std::time::Duration::from_secs(8);
                while app.transfer_progress().completed_files == 0 && !task.is_finished() {
                    assert!(std::time::Instant::now() < limit);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                assert!(app.transfer_progress().completed_files > 0);
                assert!(app.transfer_progress().completed_files < count as u64);
                assert!(!task.is_finished());
            }
            if scenario == "count" {
                let limit = std::time::Instant::now() + std::time::Duration::from_secs(8);
                while app.transfer_progress().completed_files < 2 && !task.is_finished() {
                    assert!(std::time::Instant::now() < limit);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                assert!(app.transfer_progress().completed_files >= 2);
                assert!(app.transfer_progress().completed_files < count as u64);
                assert!(!task.is_finished());
                assert!(start.elapsed() < interval);
            }
            if scenario == "cancel" {
                let limit = std::time::Instant::now() + std::time::Duration::from_secs(8);
                while app.transfer_progress().pending_files == 0 && !task.is_finished() {
                    assert!(std::time::Instant::now() < limit);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                assert_eq!(app.transfer_progress().completed_files, 0);
                assert!(manager.read_meta("password").unwrap().files.is_empty());
                assert!(app.cancel_transfer());
            }
            let result = task.join().unwrap();
            assert_eq!(app.transfer_progress().pending_files, 0);
            if scenario == "cancel" {
                assert!(matches!(
                    result,
                    Err(ServiceError::ImportCancelled { completed: 0, .. })
                ));
                assert!(manager.read_meta("password").unwrap().files.is_empty());
                assert_eq!(
                    std::fs::read_dir(&path)
                        .unwrap()
                        .filter_map(Result::ok)
                        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "enc"))
                        .count(),
                    0
                );
            } else {
                assert!(result.is_ok());
                assert_eq!(app.transfer_progress().completed_files, count as u64);
                if scenario == "timer" {
                    assert!(start.elapsed() >= std::time::Duration::from_millis(40));
                } else if scenario == "last" {
                    assert!(start.elapsed() < interval);
                }
            }
        }
    }

    #[test]
    /// 验证多文件准备 / 提交按批次原子处理，进度统计覆盖每个文件。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn multi_file_preparation_and_commit_are_atomic_and_count_every_file() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        for scenario in ["ok", "key", "lease"] {
            let fail = scenario != "ok";
            let root = tempfile::tempdir().unwrap();
            let manager = VeilManager::new(root.path().join("container"));
            manager.init_veil("test", "password").unwrap();
            let source = root.path().join("source");
            std::fs::write(&source, b"content").unwrap();
            let files = (0..16)
                .map(|index| {
                    crate::veil_ops::AddFileSpec::new(&source, format!("album/file-{index}"))
                        .without_overwrite()
                })
                .collect::<Vec<_>>();
            let progress = veil_core::progress::Progress::default();
            let _scope = progress.begin("import");
            veil_core::progress::start(None);
            veil_core::progress::set_file_count(files.len());
            let (sender, receiver) = veil_core::queue::BoundedQueue::new(2).split();
            let context = crate::veil_ops::ImportBatchContext {
                directories: Arc::default(),
                sender,
                directory_guard: None,
                owner: crate::execution::ExecutionService::global()
                    .unwrap()
                    .runtime()
                    .current_context()
                    .owner()
                    .clone(),
            };
            manager
                .add_files_for_import(&files, "password", None, &Mutex::new(()), &context)
                .unwrap();
            assert_eq!(progress.snapshot().pending_files, 16);
            assert!(manager.list_files("password").unwrap().is_empty());
            let mut queued = receiver.recv().unwrap();
            assert_eq!(queued.file_count(), 16);
            match scenario {
                "key" => {
                    queued.invalidate_key();
                }
                "lease" => {
                    manager
                        .open_db("password")
                        .unwrap()
                        .conn
                        .execute(
                            "UPDATE operation_locks SET owner_token='invalidated-token'",
                            [],
                        )
                        .unwrap();
                }
                _ => {}
            }
            let result = manager.commit_import_batch(
                vec![queued],
                "password",
                &std::sync::atomic::AtomicBool::new(false),
            );
            assert_eq!(result.is_err(), fail);
            assert_eq!(progress.snapshot().pending_files, 0);
            assert_eq!(
                progress.snapshot().completed_files,
                if fail { 0 } else { 16 }
            );
            assert_eq!(
                manager.list_files("password").unwrap().len(),
                if fail { 0 } else { 16 }
            );
        }
    }

    #[test]
    /// 验证同批密文记录要么一同提交，要么事务失败后全部回滚。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn encrypted_import_records_commit_together_or_roll_back_together() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        for fail in [false, true] {
            let root = tempfile::TempDir::new().unwrap();
            let manager = VeilManager::new(root.path().join("container"));
            manager.init_veil("test", "password").unwrap();
            let paths = [root.path().join("good"), root.path().join("bad")];
            for path in &paths {
                std::fs::write(path, b"content").unwrap();
            }
            let gate = Mutex::new(());
            let owner = crate::execution::ExecutionService::global()
                .unwrap()
                .runtime()
                .current_context()
                .owner()
                .clone();
            let (sender, receiver) = veil_core::queue::BoundedQueue::new(2).split();
            std::thread::scope(|scope| {
                let mut workers = Vec::new();
                for path in &paths {
                    let sender = sender.clone();
                    let gate = &gate;
                    let manager = &manager;
                    let owner = owner.clone();
                    workers.push(scope.spawn(move || {
                        let context = crate::veil_ops::ImportBatchContext {
                            directories: Arc::default(),
                            sender,
                            directory_guard: None,
                            owner,
                        };
                        let spec = crate::veil_ops::AddFileSpec::new(
                            path,
                            path.file_name().unwrap().to_str().unwrap(),
                        )
                        .without_overwrite();
                        import_one_file(manager, &spec, "password", None, gate, &context)
                    }));
                }
                let first = receiver
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap();
                let mut second = receiver
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .unwrap();
                for worker in workers {
                    assert!(worker.join().unwrap().is_ok());
                }
                // 加密线程已退出时，待提交租约仍归属于整个导入任务。
                if !fail {
                    std::thread::sleep(std::time::Duration::from_secs(6));
                }
                manager.cleanup("password").unwrap();
                let failed_name = fail.then(|| second.invalidate_key());
                assert!(manager.read_meta("password").unwrap().files.is_empty());
                let result = manager.commit_import_batch(
                    vec![first, second],
                    "password",
                    &std::sync::atomic::AtomicBool::new(false),
                );
                if let Some(name) = &failed_name {
                    assert!(
                        matches!(result, Err(veil_core::error::VeilError::ImportBatchFailed { file, .. }) if &file == name)
                    );
                } else {
                    assert!(result.is_ok());
                }
            });
            assert_eq!(
                manager.read_meta("password").unwrap().files.len(),
                if fail { 0 } else { 2 }
            );
            if fail {
                let db = crate::storage::container::ContainerDb::open(
                    &manager.veil_dir,
                    "password",
                    None,
                )
                .unwrap();
                let hashes: i64 = db
                    .conn
                    .query_row("SELECT count(*) FROM file_hash", [], |row| row.get(0))
                    .unwrap();
                assert_eq!(hashes, 0);
                assert_eq!(
                    std::fs::read_dir(&manager.veil_dir)
                        .unwrap()
                        .filter_map(Result::ok)
                        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "enc"))
                        .count(),
                    0
                );
            }
        }
    }

    #[test]
    /// 验证普通预览读取可与共享写操作共存，导出则按写入保护范围申请权限。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn preview_can_read_during_shared_write_and_export_uses_shared_write() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::tempdir().unwrap();
        let manager = VeilManager::new(root.path().join("container"));
        manager.init_veil("test", "password").unwrap();
        let source = root.path().join("source");
        std::fs::write(&source, b"content").unwrap();
        manager
            .add_files(
                &[crate::veil_ops::AddFileSpec::new(&source, "file.txt")],
                "password",
            )
            .unwrap();
        let db = crate::storage::container::ContainerDb::open(&manager.veil_dir, "password", None)
            .unwrap();
        let file = db.resolve("file.txt").unwrap().unwrap().id;
        let veil_id = db.info().unwrap().0;
        drop(db);
        let app = VeilService::default();
        *app.0.lock().unwrap() = Some(Session {
            path: manager.veil_dir.clone(),
            veil_id,
            password: Zeroizing::new("password".into()),
        });
        let guard = crate::storage::locks::LockManager::new(&manager.veil_dir, "password")
            .acquire(&[file], crate::storage::locks::LockMode::SharedWrite)
            .unwrap();
        assert!(
            matches!(app.execute(ServiceRequest::PreviewSessionFile { name: "file.txt".into() }).unwrap(), ServiceResponse::PreviewUnstable(bytes) if bytes == b"content")
        );
        let db = crate::storage::container::ContainerDb::open(&manager.veil_dir, "password", None)
            .unwrap();
        let count: i64 = db
            .conn
            .query_row("SELECT count(*) FROM operation_locks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        drop(db);
        drop(guard);
        assert!(matches!(
            app.execute(ServiceRequest::ExportSessionFile {
                name: "file.txt".into(),
                destination: root.path().join("out").to_string_lossy().into_owned()
            })
            .unwrap(),
            ServiceResponse::Done
        ));
    }

    #[test]
    /// 验证文件夹导入工作线程复用目录租约，不为每个子文件重复申请租约。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn folder_workers_reuse_the_directory_lease_without_file_leases() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let manager = VeilManager::new(root.path().join("container"));
        manager.init_veil("test", "password").unwrap();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (guard, directories) = manager
            .lock_import_directories(&["album".into()], "password", cancel)
            .unwrap();
        let source = root.path().join("source");
        std::fs::write(&source, b"content").unwrap();
        manager
            .add_files_in_directory(
                &[crate::veil_ops::AddFileSpec::new(&source, "album/file")],
                "password",
                &guard,
                &directories[0],
            )
            .unwrap();
        assert!(
            manager
                .add_files_in_directory(
                    &[crate::veil_ops::AddFileSpec::new(&source, "other/file")],
                    "password",
                    &guard,
                    &directories[0],
                )
                .is_err()
        );
        let db = crate::storage::container::ContainerDb::open(&manager.veil_dir, "password", None)
            .unwrap();
        let resources: Vec<String> = {
            let mut query = db
                .conn
                .prepare("SELECT resource_id FROM operation_locks")
                .unwrap();
            query
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(
            resources,
            [guard.credentials().unwrap()[0].resource_id.clone()]
        );
        drop(db);
        drop(guard);
        assert_eq!(
            manager
                .read_file_bytes("album/file", "password", 100)
                .unwrap(),
            b"content"
        );
    }

    #[test]
    /// 验证并行目录导入共享一把目录租约，避免同目录任务互相阻塞。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn folder_import_uses_one_directory_lease_with_parallel_workers() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("container");
        let manager = VeilManager::new(path.clone());
        let metadata = manager.init_veil("test", "password").unwrap();
        let app = VeilService::default();
        *app.3.write().unwrap() = Some(path.clone());
        *app.0.lock().unwrap() = Some(Session {
            path,
            veil_id: metadata.veil_id.clone(),
            password: Zeroizing::new("password".into()),
        });
        let folder = root.path().join("album");
        std::fs::create_dir(&folder).unwrap();
        for index in 0..24 {
            std::fs::write(
                folder.join(format!("file-{index}")),
                vec![index as u8; 4096],
            )
            .unwrap();
        }
        app.execute(ServiceRequest::ImportSessionFiles {
            sources: vec![folder.to_string_lossy().into_owned()],
            directory: String::new(),
        })
        .unwrap();
        assert_eq!(app.transfer_progress().completed_files, 24);
        for index in [0, 11, 23] {
            assert_eq!(
                manager
                    .read_file_bytes(&format!("album/file-{index}"), "password", 4096)
                    .unwrap(),
                vec![index as u8; 4096]
            );
        }
        // 已登记哈希用于重复内容比较；再次导入应跳过相同文件。
        app.execute(ServiceRequest::ImportSessionFiles {
            sources: vec![folder.to_string_lossy().into_owned()],
            directory: String::new(),
        })
        .unwrap();
        assert_eq!(app.transfer_progress().skipped_files, 24);
        assert!(
            manager
                .list_files("password")
                .unwrap()
                .iter()
                .all(|file| file.hash.is_some())
        );
        std::fs::write(folder.join("file-0"), vec![99u8; 4096]).unwrap();
        assert!(matches!(
            app.execute(ServiceRequest::ImportSessionFiles {
                sources: vec![folder.to_string_lossy().into_owned()],
                directory: String::new(),
            }),
            Err(ServiceError::ImportFileFailed {
                source: veil_core::error::VeilError::FileAlreadyExists(_),
                ..
            })
        ));
    }

    #[test]
    /// 验证单文件和多个文件夹导入各自使用正确的锁范围。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn mixed_file_and_multiple_folder_imports_use_their_own_scopes() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("container");
        let manager = VeilManager::new(path.clone());
        let metadata = manager.init_veil("test", "password").unwrap();
        let app = VeilService::default();
        *app.3.write().unwrap() = Some(path.clone());
        *app.0.lock().unwrap() = Some(Session {
            path,
            veil_id: metadata.veil_id.clone(),
            password: Zeroizing::new("password".into()),
        });
        let first = root.path().join("first");
        let second = root.path().join("second");
        let standalone = root.path().join("single.txt");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        std::fs::write(first.join("a"), b"a").unwrap();
        std::fs::write(second.join("b"), b"b").unwrap();
        std::fs::write(&standalone, b"single").unwrap();
        app.execute(ServiceRequest::ImportSessionFiles {
            sources: [&first, &second, &standalone]
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            directory: String::new(),
        })
        .unwrap();
        assert_eq!(
            manager.read_file_bytes("first/a", "password", 100).unwrap(),
            b"a"
        );
        assert_eq!(
            manager
                .read_file_bytes("second/b", "password", 100)
                .unwrap(),
            b"b"
        );
        assert_eq!(
            manager
                .read_file_bytes("single.txt", "password", 100)
                .unwrap(),
            b"single"
        );
    }

    #[test]
    /// 验证等待目录租约期间收到取消后能够结束等待并释放登记。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn waiting_for_directory_lock_can_be_cancelled() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("container");
        let manager = VeilManager::new(path.clone());
        let metadata = manager.init_veil("test", "password").unwrap();
        manager.create_directory("input", "password").unwrap();
        let root_id = crate::storage::container::ContainerDb::open(&path, "password", None)
            .unwrap()
            .root()
            .unwrap()
            .id;
        let external = crate::storage::locks::LockManager::new(&path, "password")
            .acquire_directory(&root_id, crate::storage::locks::LockMode::Write)
            .unwrap();
        let app = VeilService::default();
        *app.3.write().unwrap() = Some(path.clone());
        *app.0.lock().unwrap() = Some(Session {
            path,
            veil_id: metadata.veil_id.clone(),
            password: Zeroizing::new("password".into()),
        });
        let folder = root.path().join("input");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("file"), b"test").unwrap();
        let worker = app.clone();
        let task = std::thread::spawn(move || {
            worker.execute(ServiceRequest::ImportSessionFiles {
                sources: vec![folder.to_string_lossy().into_owned()],
                directory: String::new(),
            })
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.transfer_progress().phase != "preparing" && !task.is_finished() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let reader = app.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let refresh = std::thread::spawn(move || {
            let _ = sender.send(reader.execute(ServiceRequest::RefreshContainer).is_ok());
        });
        let refreshed = receiver.recv_timeout(std::time::Duration::from_secs(3));
        assert!(app.cancel_transfer());
        assert!(matches!(
            task.join().unwrap(),
            Err(ServiceError::ImportCancelled { completed: 0, .. })
        ));
        refresh.join().unwrap();
        assert_eq!(refreshed.ok(), Some(true));
        assert_eq!(app.transfer_progress().phase, "cancelled");
        drop(external);
    }

    #[test]
    /// 验证目录任务在文件提交前能续期共享租约，防止长任务误过期。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn shared_directory_lease_can_be_renewed_before_file_commit() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let manager = VeilManager::new(root.path().join("container"));
        manager.init_veil("test", "password").unwrap();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (guard, directories) = manager
            .lock_import_directories(&[String::new()], "password", cancel)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_secs(6));
        let source = root.path().join("file");
        std::fs::write(&source, b"content").unwrap();
        manager
            .add_files_in_directory(
                &[crate::veil_ops::AddFileSpec::new(&source, "file")],
                "password",
                &guard,
                &directories[0],
            )
            .unwrap();
        drop(guard);
        assert_eq!(
            manager.read_file_bytes("file", "password", 100).unwrap(),
            b"content"
        );
    }

    #[test]
    /// 验证并行处理大量小文件后没有漏项，任务进度与实际完成数一致。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn parallel_small_file_import_keeps_every_file_and_reports_progress() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("container");
        let manager = VeilManager::new(path.clone());
        let metadata = manager.init_veil("test", "password").unwrap();
        let app = VeilService::default();
        *app.3.write().unwrap() = Some(path.clone());
        *app.0.lock().unwrap() = Some(Session {
            path,
            veil_id: metadata.veil_id.clone(),
            password: Zeroizing::new("password".into()),
        });
        let mut sources = Vec::new();
        for index in 0..12 {
            let path = root.path().join(format!("file-{index}.txt"));
            std::fs::write(&path, vec![index as u8; 4096]).unwrap();
            sources.push(path.to_string_lossy().into_owned());
        }
        app.execute(ServiceRequest::ImportSessionFiles {
            sources,
            directory: "nested".into(),
        })
        .unwrap();
        let progress = app.transfer_progress();
        assert_eq!(
            (
                progress.phase,
                progress.completed_files,
                progress.total_files
            ),
            ("completed", 12, 12)
        );
        assert_eq!(
            (progress.processed_bytes, progress.total_bytes),
            (12 * 4096, Some(12 * 4096))
        );
        assert!((1..=4).contains(&progress.parallel_workers));
        assert_eq!(manager.read_meta("password").unwrap().files.len(), 12);
        for index in [0, 5, 11] {
            assert_eq!(
                manager
                    .read_file_bytes(&format!("nested/file-{index}.txt"), "password", 4096)
                    .unwrap(),
                vec![index as u8; 4096]
            );
        }
    }

    #[test]
    /// 验证取消不会回滚已提交文件，剩余文件可在后续重新导入。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn cancelling_import_preserves_committed_files_and_allows_retry() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("container");
        let manager = VeilManager::new(path.clone());
        let metadata = manager.init_veil("test", "password").unwrap();
        let app = VeilService::default();
        *app.3.write().unwrap() = Some(path.clone());
        *app.0.lock().unwrap() = Some(Session {
            path,
            veil_id: metadata.veil_id.clone(),
            password: Zeroizing::new("password".into()),
        });
        let mut sources = Vec::new();
        for index in 0..24 {
            let path = root.path().join(format!("file-{index}.txt"));
            std::fs::write(&path, vec![index as u8; 1024 * 1024]).unwrap();
            sources.push(path.to_string_lossy().into_owned());
        }
        let worker = app.clone();
        let worker_sources = sources.clone();
        let task = std::thread::spawn(move || {
            worker.execute(ServiceRequest::ImportSessionFiles {
                sources: worker_sources,
                directory: String::new(),
            })
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while app.transfer_progress().completed_files == 0 && !task.is_finished() {
            assert!(std::time::Instant::now() < deadline, "导入未开始");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(app.cancel_transfer());
        let result = task.join().unwrap();
        let completed = match result {
            Err(ServiceError::ImportCancelled { completed, .. }) => completed,
            _ => panic!("预期中止导入"),
        };
        assert!(completed > 0 && completed < 24);
        assert_eq!(app.transfer_progress().phase, "cancelled");
        assert_eq!(
            manager.read_meta("password").unwrap().files.len(),
            completed as usize
        );
        app.execute(ServiceRequest::ImportSessionFiles {
            sources,
            directory: String::new(),
        })
        .unwrap();
        assert_eq!(manager.read_meta("password").unwrap().files.len(), 24);
        assert_eq!(app.transfer_progress().skipped_files, completed);
    }

    #[test]
    /// 验证单个文件写入提交只更新计数，不会提前把整个导入任务标记完成。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn individual_file_commit_does_not_finish_the_import_progress() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let manager = VeilManager::new(root.path().join("container"));
        manager.init_veil("test", "password").unwrap();
        let source = root.path().join("file.txt");
        std::fs::write(&source, b"data").unwrap();
        let progress = veil_core::progress::Progress::default();
        let _scope = progress.begin("import");
        veil_core::progress::start(Some(8));
        veil_core::progress::set_file_count(2);
        veil_core::progress::begin_file("file.txt");
        manager
            .add_files_without_cleanup(
                &[crate::veil_ops::AddFileSpec::new(&source, "file.txt")],
                "password",
            )
            .unwrap();
        assert_eq!(
            std::fs::read_dir(manager.veil_dir.join(".staging"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(progress.snapshot().phase, "processing");
        veil_core::progress::file_done(false);
        veil_core::progress::begin_file("next.txt");
        manager
            .add_files_without_cleanup(
                &[crate::veil_ops::AddFileSpec::new(&source, "next.txt")],
                "password",
            )
            .unwrap();
        assert_eq!(progress.snapshot().phase, "processing");
        veil_core::progress::finalizing();
        assert_eq!(progress.snapshot().phase, "finalizing");
    }

    #[test]
    /// 验证导入去重按目标容器、目标目录和规范化来源绝对路径判断。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn duplicate_import_uses_container_target_and_selected_absolute_paths() {
        let root = tempfile::TempDir::new().unwrap();
        let source = root.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let first = source.join("first.txt");
        let second = source.join("second.txt");
        std::fs::write(&first, b"first").unwrap();
        std::fs::write(&second, b"second").unwrap();
        let app = VeilService::default();
        let container = root.path().join("container");
        *app.3.write().unwrap() = Some(container.clone());
        let first_path = first.to_string_lossy().into_owned();
        let second_path = second.to_string_lossy().into_owned();
        let directory_path = source.to_string_lossy().into_owned();
        let keys = vec![directory_path.clone(), first_path.clone()];
        let original_keys = keys.clone();
        let session_lock = app.0.lock().unwrap();
        let worker = app.clone();
        let original = std::thread::spawn(move || {
            worker.execute(ServiceRequest::ImportSessionFiles {
                sources: original_keys,
                directory: "target".into(),
            })
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !import_registry()
            .lock()
            .unwrap()
            .iter()
            .any(|key| key.container == container)
        {
            assert!(std::time::Instant::now() < deadline, "导入任务未登记");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // 重复判断只使用所选路径，目录内容和文件内容的变化不改变任务标识。
        std::fs::write(&first, b"changed").unwrap();
        std::fs::write(source.join("added.txt"), b"new").unwrap();
        let duplicate = VeilService::default();
        *duplicate.3.write().unwrap() = Some(container.clone());
        let (send, receive) = std::sync::mpsc::channel();
        let duplicate_thread = std::thread::spawn(move || {
            let result = duplicate.execute(ServiceRequest::ImportSessionFiles {
                sources: vec![first_path, directory_path],
                directory: "target".into(),
            });
            send.send(matches!(result, Err(ServiceError::DuplicateImport)))
                .unwrap();
        });
        let result = receive.recv_timeout(std::time::Duration::from_secs(2));
        assert!(app.register_import(&keys, "other").is_ok());
        assert!(app.register_import(&[second_path], "target").is_ok());
        let other_container = VeilService::default();
        *other_container.3.write().unwrap() = Some(root.path().join("other-container"));
        assert!(other_container.register_import(&keys, "target").is_ok());
        drop(session_lock);
        duplicate_thread.join().unwrap();
        assert!(result.unwrap());
        assert!(matches!(
            original.join().unwrap(),
            Err(ServiceError::NotOpen)
        ));
        assert!(app.register_import(&keys, "target").is_ok());
    }

    #[test]
    /// 验证部分导入成功后可重试，重试只跳过已确认相同的文件。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn partial_import_is_committed_and_retry_skips_only_matching_files() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let path = root.path().join("container");
        let manager = VeilManager::new(path.clone());
        let meta = manager.init_veil("test", "password").unwrap();
        let app = VeilService::default();
        *app.3.write().unwrap() = Some(path.clone());
        *app.0.lock().unwrap() = Some(Session {
            path,
            veil_id: meta.veil_id.clone(),
            password: Zeroizing::new("password".into()),
        });
        let first = root.path().join("first.txt");
        let conflict = root.path().join("conflict.txt");
        let last = root.path().join("last.txt");
        std::fs::write(&first, b"first").unwrap();
        std::fs::write(&conflict, b"original").unwrap();
        manager.add_file(&conflict, "password").unwrap();
        std::fs::write(&conflict, b"changed!").unwrap();
        std::fs::write(&last, b"last").unwrap();
        let request = || ServiceRequest::ImportSessionFiles {
            sources: [&first, &conflict, &last]
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            directory: String::new(),
        };
        let error = match app.execute(request()) {
            Err(error) => error,
            Ok(_) => panic!("预期同名文件冲突"),
        };
        let completed = match error {
            ServiceError::ImportFileFailed {
                completed,
                skipped: 0,
                ..
            } => completed,
            _ => panic!("预期报告部分完成的导入"),
        };
        assert_eq!(
            manager.read_meta("password").unwrap().files.len(),
            completed as usize + 1
        );
        for (name, expected) in [
            ("first.txt", b"first".as_slice()),
            ("last.txt", b"last".as_slice()),
        ] {
            if let Ok(content) = manager.read_file_bytes(name, "password", 100) {
                assert_eq!(content, expected);
            }
        }
        assert_eq!(
            manager
                .read_file_bytes("conflict.txt", "password", 100)
                .unwrap(),
            b"original"
        );
        std::fs::write(&conflict, b"original").unwrap();
        app.execute(request()).unwrap();
        let progress = app.transfer_progress();
        assert_eq!(
            (
                progress.phase,
                progress.completed_files,
                progress.skipped_files,
                progress.total_files
            ),
            ("completed", 3, completed + 1, 3)
        );
        assert_eq!(
            manager
                .read_file_bytes("last.txt", "password", 100)
                .unwrap(),
            b"last"
        );
    }

    #[test]
    /// 验证传输失败时进度快照进入失败状态并保留已完成统计。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn failed_transfer_reports_failed_status() {
        let app = VeilService::default();
        *app.3.write().unwrap() = Some(PathBuf::from("/test/container"));
        assert!(
            app.execute(ServiceRequest::ImportSessionFiles {
                sources: vec![],
                directory: String::new()
            })
            .is_err()
        );
        let progress = app.transfer_progress();
        assert_eq!(progress.operation, "import");
        assert_eq!(progress.phase, "failed");
    }

    /// 在测试断言中取出容器视图响应，简化创建/刷新调用。
    /// 入参：response（ServiceResponse）为按值消费的服务结果。
    /// 返回值：Container变体中的ContainerView。
    /// 异常：其他变体直接panic，表示测试预期与协议不符。
    /// 注意事项：仅测试，不提供生产容错转换。
    fn container(response: ServiceResponse) -> ContainerView {
        match response {
            ServiceResponse::Container(view) => view,
            _ => panic!("预期返回容器"),
        }
    }

    #[test]
    /// 验证 在测试容器上创建并取得生命周期锁，再打开会话供后续锁语义断言。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn create_lock_and_open_container() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let app = VeilService::default();
        let created = container(
            app.execute(ServiceRequest::CreateContainer {
                parent: root.path().display().to_string(),
                name: "我的容器".into(),
                password: "password".into(),
            })
            .unwrap(),
        );
        assert_eq!(created.name, "我的容器");
        assert!(Path::new(&created.path).join("veil.db").exists());
        assert_eq!(
            container(app.execute(ServiceRequest::RefreshContainer).unwrap()).id,
            created.id
        );
        let source = root.path().join("example.txt");
        std::fs::write(&source, b"example").unwrap();
        VeilManager::new(PathBuf::from(&created.path))
            .add_file(&source, "password")
            .unwrap();
        let refreshed = container(app.execute(ServiceRequest::RefreshContainer).unwrap());
        assert_eq!(refreshed.files.len(), 1);
        assert_eq!(refreshed.files[0].name, "example.txt");
        assert_eq!(refreshed.files[0].version, 1);
        assert!(matches!(
            app.execute(ServiceRequest::LockContainer),
            Ok(ServiceResponse::Locked)
        ));
        assert!(matches!(
            app.execute(ServiceRequest::RefreshContainer),
            Err(ServiceError::NotOpen)
        ));
        assert!(
            app.execute(ServiceRequest::OpenContainer {
                path: created.path.clone(),
                password: "wrong".into()
            })
            .is_err()
        );
        let opened = container(
            app.execute(ServiceRequest::OpenContainer {
                path: created.path,
                password: "password".into(),
            })
            .unwrap(),
        );
        assert_eq!(opened.id, created.id);
    }

    #[test]
    /// 验证请求层把并发版本冲突作为可识别业务结果传回调用方。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn requests_preserve_version_conflicts() {
        use crate::operations::{ContainerOperation as Op, ContainerOperationResult as Out};
        use crate::veil_ops::AddFileSpec;
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let service = VeilService::default();
        let created = container(
            service
                .execute(ServiceRequest::CreateContainer {
                    parent: root.path().display().to_string(),
                    name: "test".into(),
                    password: "password".into(),
                })
                .unwrap(),
        );
        let dir = Path::new(&created.path);
        let source = root.path().join("source.txt");
        std::fs::write(&source, b"one").unwrap();
        let run = |operation| {
            service.execute(ServiceRequest::OperateContainer {
                directory: dir,
                veil_id: &created.id,
                operation,
            })
        };
        let files = [AddFileSpec::new(&source, "source.txt")];
        assert!(matches!(
            run(Op::AddFiles {
                files: &files,
                password: "password"
            }),
            Ok(ServiceResponse::Operation(Out::Added(_)))
        ));
        let output = root.path().join("export.txt");
        let version = match run(Op::ExtractFileWithVersion {
            name: "source.txt",
            output: &output,
            password: "password",
        })
        .unwrap()
        {
            ServiceResponse::Operation(Out::Version(version)) => version,
            _ => panic!("预期返回版本快照"),
        };
        std::fs::write(&source, b"two").unwrap();
        run(Op::AddFiles {
            files: &files,
            password: "password",
        })
        .unwrap();
        let stale = [AddFileSpec::new(&source, "source.txt").with_base_version(version)];
        assert!(matches!(
            run(Op::AddFiles {
                files: &stale,
                password: "password"
            }),
            Err(ServiceError::Core(
                veil_core::error::VeilError::VersionConflict(_)
            ))
        ));
        let package = root.path().join("existing.veil");
        std::fs::write(&package, b"existing").unwrap();
        assert!(
            run(Op::Pack {
                output: &package,
                password: "password"
            })
            .is_err()
        );
        assert_eq!(std::fs::read(package).unwrap(), b"existing");
    }
}
