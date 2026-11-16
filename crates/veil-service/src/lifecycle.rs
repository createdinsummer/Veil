//! 容器创建的事务编排。
//!
//! 创建容器横跨文件系统与两份登记（本机应用库＋链接文件），没有天然的整体
//! 事务。本模块把流程排成固定顺序，并在每类失败点给出明确回滚策略：
//! 目录占位后初始化回调失败 → 尽力清理本次目录（可能包含部分写入）；初始化成功但登记失败 →
//! 保留已经提交的容器数据并报告位置。依赖配置、目录原子占位与调用方提供的初始化回调；
//! CreationError 区分前置检查、初始化及登记失败，create_registered_container 实现固定顺序。

use crate::config::AppConfig;
use std::path::Path;
use veil_core::error::VeilError;

/// 创建过程中的失败阶段，便于调用方提供对应提示。
#[derive(Debug, thiserror::Error)]
pub enum CreationError {
    /// 目标容器目录已存在，不能覆盖或接管别人的目录。
    #[error("容器目录已存在：{}", .0.display())]
    ContainerExists(std::path::PathBuf),
    /// 目标链接文件已存在；此创建流程不覆盖该入口，容器仍可由其他登记路径访问。
    #[error("链接已存在：{}", .0.display())]
    LinkExists(std::path::PathBuf),
    /// 创建目录或初始化回调失败；回调可能已写入部分数据，随后仅尽力清理本次目录。
    #[error("创建容器失败：{0}")]
    Initialize(#[source] VeilError),
    /// 初始化前的参数、生命周期锁或本机身份预检查失败，尚未调用初始化回调。
    #[error("登记容器失败：{0}")]
    Register(#[source] VeilError),
    /// 容器已初始化成功但登记失败：数据保留在 directory，等待重新登记。
    #[error("登记容器失败，数据保留在 {}：{source}", directory.display())]
    RegisterInitialized {
        /// 初始化回调已成功的容器目录，失败后保留供重新登记。
        directory: std::path::PathBuf,
        #[source]
        /// 后续本机登记失败的原始原因，不表示已初始化目录也被回滚。
        source: VeilError,
    },
    /// 链接文件写入失败（容器数据同样保留）。
    #[error("登记链接失败：{0}")]
    Link(#[source] VeilError),
}

/// 协调目录占位、容器初始化、本机登记和链接发布，按初始化结果决定失败清理策略。
/// 入参：config（&mut AppConfig）提供本机登记；veil_dir（&Path）须等于 work_root/veil_id；veil_id（&str）为合法 UUID；veil_name（&str）去空白后须非空；password（&str）须非空；work_root（&Path）为父工作目录；work_root_existed（bool）由调用方记录此前是否存在，影响回滚时是否尝试删除空根目录；link_path（&Path）为不能覆盖的链接目标；initialize 为 FnOnce(&Path, &str, &str, &str) -> `Result<MetaData, VeilError>`，接收已占位目录、ID、名称、密码。
/// 返回值：初始化、容器登记及链接登记全部成功返回 Ok(())。
/// 异常：参数/本机身份预检查错误为 Register；目录/链接已存在为各自 Exists；初始化/创建错误为 Initialize；初始化成功后的容器登记错误为 RegisterInitialized；链接失败为 Link。
/// 注意事项：回调返回 Ok 才视作初始化完成；Err 时尽力递归删除本次占位目录，回调须遵守此约定。完成初始化后保留数据，不能跨文件系统和应用库整体回滚；清理失败仅记日志，原错误保留。
pub(crate) fn create_registered_container(
    config: &mut AppConfig,
    veil_dir: &Path,
    veil_id: &str,
    veil_name: &str,
    password: &str,
    work_root: &Path,
    work_root_existed: bool,
    link_path: &Path,
    initialize: impl FnOnce(&Path, &str, &str, &str) -> Result<veil_core::metadata::MetaData, VeilError>,
) -> Result<(), CreationError> {
    // 容器 ID 必须是合法 UUID 格式，否则后续所有按 ID 定位的逻辑都会失效。
    crate::work::validate_veil_id(veil_id).map_err(CreationError::Register)?;
    // 目录位置、名称、密码三个前置条件一起校验，避免半途失败。
    if veil_dir != work_root.join(veil_id) || veil_name.trim().is_empty() || password.is_empty() {
        return Err(CreationError::Register(VeilError::WorkError(
            "容器参数无效".into(),
        )));
    }
    // 共享生命周期锁阻止遵守协议的删除；不独占其他共享使用者，创建竞争仍由目录原子占位裁决。
    let _container_lock = config
        .container_lifecycle_lock(veil_id)
        .map_err(CreationError::Register)?;
    // 确认该 ID 尚未在本机登记，避免重复身份。
    config
        .ensure_veil_id_available(veil_id)
        .map_err(CreationError::Register)?;
    // 目录与链接必须先都不存在，才能开始创建。
    if veil_dir.exists() {
        return Err(CreationError::ContainerExists(veil_dir.to_path_buf()));
    }
    if link_path.exists() {
        return Err(CreationError::LinkExists(link_path.to_path_buf()));
    }
    // 创建工作根目录；此时尚无容器数据，失败可安全报 Initialize。
    crate::fsutil::create_dir_all_durable(work_root)
        .map_err(|e| CreationError::Initialize(e.into()))?;
    // 创建容器目录本身；与其他进程竞争时给出「目录已存在」。
    std::fs::create_dir(veil_dir).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            CreationError::ContainerExists(veil_dir.into())
        } else {
            CreationError::Initialize(e.into())
        }
    })?;
    // 标记：数据库初始化是否已经成功提交。它决定失败时回滚目录还是保留数据。
    let mut initialized = false;
    let result = (|| {
        // 回调实际创建加密容器数据库并返回元数据。
        let metadata = initialize(veil_dir, veil_id, veil_name, password)
            .map_err(CreationError::Initialize)?;
        // 走到这里说明容器数据已提交，之后的失败都要保留数据。
        initialized = true;
        #[cfg(feature = "fault-injection")]
        veil_core::fsutil::fault_point("after_container_init");
        // 把容器登记进本机应用库；失败时数据已初始化，必须保留并指明位置。
        config
            .register_directory(veil_dir, &metadata.veil_id, veil_name, &metadata.created_at)
            .map_err(|source| CreationError::RegisterInitialized {
                directory: veil_dir.into(),
                source,
            })?;
        // 最后写入链接文件，作为该容器的便携入口。
        config
            .register_link_at(&metadata.veil_id, veil_name, veil_dir, link_path)
            .map_err(CreationError::Link)?;
        Ok(())
    })();
    // 失败时的回滚/保留处理，按「数据是否已提交」分成两条路径。
    if let Err(error) = &result {
        use veil_core::logging::{self, Field, Level};
        if initialized {
            // 数据已提交：保留容器目录，只记录部分失败事件与失败阶段。
            logging::event(
                Level::Error,
                "service",
                "create_container",
                "CONTAINER_CREATE_PARTIAL",
                "容器已初始化，但本机登记未完成；已保留容器数据",
                &[
                    ("database", Field::Path(veil_dir)),
                    (
                        "phase",
                        Field::Label(if matches!(error, CreationError::Link(_)) {
                            "register_link"
                        } else {
                            "register_container"
                        }),
                    ),
                    ("outcome", Field::Label("initialized_data_preserved")),
                ],
            );
        } else {
            // 回调未返回成功：按初始化失败约定清理本次占位目录；是否写入过部分数据由回调决定。
            if let Err(cleanup) = std::fs::remove_dir_all(veil_dir) {
                if cleanup.kind() != std::io::ErrorKind::NotFound {
                    logging::failure(
                        Level::Warn,
                        "service",
                        "create_rollback",
                        "CREATE_CLEANUP_FAILED",
                        &cleanup,
                        &[
                            ("resource", Field::Path(veil_dir)),
                            ("outcome", Field::Label("directory_preserved")),
                        ],
                    );
                }
            }
            // 工作根目录如果是本次新建的，也一并回收；目录非空则保留他人内容。
            if !work_root_existed && work_root != veil_dir {
                if let Err(cleanup) = std::fs::remove_dir(work_root) {
                    if !matches!(
                        cleanup.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                    ) {
                        logging::failure(
                            Level::Warn,
                            "service",
                            "create_rollback",
                            "WORK_ROOT_CLEANUP_FAILED",
                            &cleanup,
                            &[("resource", Field::Path(work_root))],
                        );
                    }
                }
            }
        }
    }
    result
}
