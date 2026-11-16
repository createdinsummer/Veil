//! 为各调用方组合容器定位与文件操作。
//!
//! 所属业务模块：veil-service 的命令（commands）模块。CLI/桌面端常按「准备计划 →
//! 用户确认 → 提交执行」两阶段做事：本模块把容器定位（选择器解析）、导入、打包
//! 拆成可跨请求携带的计划对象，同时提供 CLI 直接可用的流式传输入口。
//!
//! 文件内核心清单：
//! - `ContainerHandle`：已定位的容器操作句柄（身份 + 目录，可派生 VeilManager）；
//! - `prepare_container` / `operate`：容器操作的定位与执行；
//! - `ImportPlan` / `ImportConflict` / `ImportDecision` / `ImportResult`：
//!   导入的两阶段模型与冲突决策（跳过/覆盖/中止）；
//! - `prepare_import` / `commit_import`：导入准备与提交；
//! - `PackPlan` / `PackResult` / `prepare_pack` / `commit_pack`：打包的两阶段模型。
//!
//! 依赖能力：config 的容器解析（AppConfig::resolve_veil）、veil_ops 的
//! VeilManager/AddFileSpec、operations 的容器操作、import 的路径收集、
//! comparison 的冲突比较。
//!
//! 整体业务目标：让「导入/打包」这类需要用户确认的操作能安全地分两步完成——
//! 准备阶段解析与收集任务，冲突决策可另行执行；配置解析可能写缓存或恢复链接，内容写入在提交阶段。
//! 期间用户修改选择不会破坏
//! 容器数据（提交时会重新校验版本与租约）。

pub use crate::command_transfer::TransferResult;
use crate::config::{AppConfig, ResolvedVeil};
use crate::import::collect_add_specs;
use crate::operations::{ContainerOperation, ContainerOperationResult};
use crate::veil_ops::{AddFileSpec, VeilManager};
use crate::workflow::WorkflowError;
use std::path::{Path, PathBuf};
use veil_core::error::VeilError;

/// 已定位且绑定容器身份的操作目标。
///
/// 业务定位：解析成功后容器的事实集合（ID/名称/目录/链接）的只读句柄，
/// 可廉价 Clone 并在多个请求间传递；所有底层操作都通过它派生出 VeilManager。
#[derive(Clone)]
pub struct ContainerHandle {
    /// 解析结果：容器 ID、展示名、实际目录、链接路径与恢复标志。
    resolved: ResolvedVeil,
}
impl ContainerHandle {
    /// 读取定位时缓存的容器展示名称。
    /// 入参：self（&ContainerHandle）为已定位句柄。
    /// 返回值：与句柄同寿命的 &str。
    /// 异常：无错误返回。
    /// 注意事项：此处不解锁核对权威名称，名称可能在定位后变化。
    pub fn name(&self) -> &str {
        &self.resolved.veil_name
    }
    /// 借用后续操作绑定的稳定容器身份。
    /// 入参：self（&ContainerHandle）为已定位句柄。
    /// 返回值：&str，定位结果中的容器 ID。
    /// 异常：无错误返回。
    /// 注意事项：不在本方法重新校验；manager 用此身份检查后续开库结果。
    pub(crate) fn id(&self) -> &str {
        &self.resolved.veil_id
    }
    /// 借用加密数据库与密文所在目录。
    /// 入参：self 为已定位句柄。
    /// 返回值：&Path，定位时得到的容器目录。
    /// 异常：无错误返回。
    /// 注意事项：不检查目录现在是否存在；符号链接和挂载状态可能在定位后改变。
    pub fn directory(&self) -> &Path {
        &self.resolved.veil_dir
    }
    /// 向客户端提供定位时采用缓存恢复分支的链接提示。
    /// 入参：self（&ContainerHandle）为定位句柄。
    /// 返回值：恢复标志为 true 且带链接时返回 Some(&Path)，其他情况为 None。
    /// 异常：无错误返回。
    /// 注意事项：标志代表采用缓存恢复路径，不保证本次调用实际新建了文件，也不重新验证文件仍在。
    pub fn recovered_link(&self) -> Option<&Path> {
        if self.resolved.recovered_link {
            self.resolved.link_path.as_deref()
        } else {
            None
        }
    }
    /// 为本次操作生成绑定预期身份的容器管理器。
    /// 入参：self（&ContainerHandle）提供目录和容器 ID。
    /// 返回值：新的 VeilManager，后续开库按预期 ID 核对。
    /// 异常：无错误返回。
    /// 注意事项：仅构造，不开库、不校验密码或申请资源租约；各操作自行完成前置条件。
    pub(crate) fn manager(&self) -> VeilManager {
        VeilManager::for_veil(self.resolved.veil_dir.clone(), &self.resolved.veil_id)
    }
}

/// 按容器选择器解析本机位置，生成可跨请求携带的操作目标。
/// 入参：selector（&str）为 ID、名称、目录或 .veil-link 路径，由 AppConfig 规则解析。
/// 返回值：成功返回 ContainerHandle，含身份、路径和链接恢复提示。
/// 异常：配置/选择器/登记/链接/文件系统失败返回 VeilError。
/// 注意事项：配置加载可能初始化应用库和默认空间；解析链接可能恢复文件或更新缓存，不能称为纯只读，也未必验证容器密码。
pub(crate) fn prepare_container(selector: &str) -> Result<ContainerHandle, VeilError> {
    let mut config = AppConfig::load()?;
    Ok(ContainerHandle {
        resolved: config.resolve_veil(selector)?,
    })
}

/// 对已定位容器执行一次统一业务操作。
/// 入参：container（&ContainerHandle）为目标；operation（ContainerOperation）为按值消费的请求，借用数据须在调用期间有效。
/// 返回值：成功返回与请求对应的 ContainerOperationResult。
/// 异常：底层密码、身份、路径、锁、数据库、认证或 I/O 错误传播。
/// 注意事项：同步调用，可能等待和写磁盘；定位句柄不等于已解锁会话或持续资源锁。
pub(crate) fn operate(
    container: &ContainerHandle,
    operation: ContainerOperation<'_>,
) -> Result<ContainerOperationResult, VeilError> {
    crate::operations::execute(&container.manager(), operation)
}

/// 服务层准备的文件导入计划。
///
/// 业务定位：导入的「准备阶段」产物——目标容器、逐文件导入规格（来源路径、
/// 容器内目标路径、覆盖标志与基础版本）、以及冲突确认时跳过的文件数。
/// 计划对象可跨请求携带，用户决策后再提交执行。
pub struct ImportPlan {
    /// 目标容器句柄。
    container: ContainerHandle,
    /// 待导入文件列表：每个元素代表一个「外部文件 → 容器内路径」的导入任务。
    files: Vec<AddFileSpec>,
    /// 用户选择跳过的文件计数（汇总进最终导入结果）。
    skipped: usize,
}
impl ImportPlan {
    /// 取得导入计划的容器身份，供提交层协调生命周期。
    /// 入参：self（&ImportPlan）为导入计划。
    /// 返回值：&str，计划中句柄保存的容器 ID。
    /// 异常：无错误返回。
    /// 注意事项：不申请锁，不刷新定位。
    pub(crate) fn container_id(&self) -> &str {
        self.container.id()
    }

    /// 逐项询问已有目标的处理策略，并为允许覆盖的文件绑定基础版本。
    /// 入参：self（ImportPlan）被消费；password（&str）用于开库/内容哈希比较；decide（FnMut(&ImportConflict) -> `Result<ImportDecision, VeilError>`）在当前线程同步决定 Skip/Overwrite/Error。
    /// 返回值：成功返回仅保留待导入任务的新计划，累积跳过数；空闲目标强制禁止覆盖，覆盖文件绑定读取时版本。
    /// 异常：回调错误、目标变化、覆盖目录或 Error 决定、比较/密码/锁/取消/数据库/I/O 失败返回 VeilError。
    /// 注意事项：不写入文件内容，但开库/比较可能影响租约和配置资源；多个读取不在统一事务。same_content 是一次比较提示，不应替代最终版本校验。错误时不返回部分选择计划。
    pub fn resolve_conflicts(
        mut self,
        password: &str,
        mut decide: impl FnMut(&ImportConflict) -> Result<ImportDecision, VeilError>,
    ) -> Result<Self, VeilError> {
        use crate::comparison::{
            ComparisonChecks, ConflictOutcome, FileInput, check_file_conflict, check_path_conflict,
        };
        let manager = self.container.manager();
        // 重新收集通过冲突检查的文件任务。
        let mut selected = Vec::new();
        // 遍历导入任务：每个 file 是一个「外部文件 → 容器内路径」的导入项。
        for file in self.files {
            // 路径空闲：无需决策，原样保留（禁止覆盖标志，保证提交时不会误覆盖）。
            if !check_path_conflict(&manager, &file.target, password, None)? {
                selected.push(file.without_overwrite());
                continue;
            }
            // 目标被占用：读取节点判断是文件还是目录。
            let db = manager.open_db(password)?;
            let node = db
                .resolve(&file.target)?
                .ok_or_else(|| VeilError::VersionConflict("目标已变化，请重新导入".into()))?;
            let is_directory = node.kind != "file";
            // 对文件记录当前版本快照（容器 ID + 文件 ID + 版本链），
            // 作为覆盖时的基础版本，提交时校验未发生并发修改。
            let base = if is_directory {
                None
            } else {
                Some(veil_core::version::VersionSnapshot {
                    veil_id: db.info()?.0,
                    file_id: node.id.clone(),
                    version_history: node.versions()?.to_vec(),
                })
            };
            drop(db);
            // 只按哈希比较内容（不逐字节）：相同内容允许用户选择跳过。
            let outcome = check_file_conflict(
                &manager,
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
                None,
            )?;
            // 组装冲突信息：目标路径、内容是否相同、占用者是否为目录。
            let conflict = ImportConflict {
                error: None,
                target: file.target.clone(),
                same_content: outcome == ConflictOutcome::NoConflict,
                is_directory,
            };
            // 按用户决策分流：
            match decide(&conflict)? {
                // 跳过：只计数，不保留任务。
                ImportDecision::Skip => self.skipped += 1,
                // 覆盖文件：绑定读取时版本快照并允许覆盖已有目标；提交时验版本。
                ImportDecision::Overwrite if !is_directory => {
                    let mut file = file.with_base_version(base.unwrap());
                    file.reject_existing = false;
                    selected.push(file);
                }
                // 覆盖目录没有意义（文件不能替换目录），与 Error 决策一样直接失败。
                ImportDecision::Overwrite | ImportDecision::Error => {
                    return Err(VeilError::FileAlreadyExists(file.target));
                }
            }
        }
        // 用筛选后的任务替换原计划。
        self.files = selected;
        Ok(self)
    }
    /// 判断计划是否还有需要执行的导入任务。
    /// 入参：self（&ImportPlan）为已准备或筛选后的计划。
    /// 返回值：files 为空为 true，否则 false。
    /// 异常：无错误返回。
    /// 注意事项：为空可能是空来源，也可能全部跳过；不表示未发生过冲突或密码已验证。
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
    /// 透出导入目标定位时的缓存恢复提示。
    /// 入参：self（&ImportPlan）为导入计划。
    /// 返回值：Some(&Path) 表示句柄记录了恢复分支链接，否则 None。
    /// 异常：无错误返回。
    /// 注意事项：不重新恢复链接，也不证明该文件现在仍存在。
    pub fn recovered_link(&self) -> Option<&Path> {
        self.container.recovered_link()
    }
}
/// 已占用目标的比较信息；相同内容也交给调用方选择。
///
/// 业务定位：一条「导入目标已被占用」的冲突事实，供 UI 展示与决策。
/// 即使内容相同也交给调用方（可能用户想跳过而非覆盖）。
pub struct ImportConflict {
    /// 冲突的附带错误说明（本计划流程中恒为 None，供流式流程复用）。
    pub error: Option<String>,
    /// 容器内被占用的目标路径。
    pub target: String,
    /// 本次哈希比较是否得到 NoConflict；是瞬时提示，不证明最终提交前目标仍未变化。
    pub same_content: bool,
    /// 占用者是否为目录（目录不能被文件覆盖）。
    pub is_directory: bool,
}

/// 调用方对一条导入冲突的决策。
#[derive(Clone, Copy)]
pub enum ImportDecision {
    /// 覆盖已有文件（提交时校验版本未变）。
    Overwrite,
    /// 跳过该文件。
    Skip,
    /// 中止导入（视为用户拒绝）。
    Error,
}

/// 导入结果，包含展示所需的目标与密文名称。
///
/// targets 与 encrypted_names 按下标一一对应：每个目标路径对应一个新生成的
/// 随机密文文件名。
pub struct ImportResult {
    /// 冲突确认时跳过的文件数。
    pub skipped: usize,
    /// 成功导入的容器内目标路径列表。
    pub targets: Vec<String>,
    /// 与 targets 对应的密文文件名列表（如 xxxx.enc）。
    pub encrypted_names: Vec<String>,
}

/// 定位导入容器并把本地来源展开成逐文件任务。
/// 入参：selector（&str）为容器选择器；source（&Path）为本地文件或目录；destination（`Option<&str>`）为可选容器目标路径，映射规则由 collect_add_specs 处理。
/// 返回值：返回 ImportPlan，每项 AddFileSpec 强制 reject_existing，skipped 初始为0。
/// 异常：定位错误转 ServiceError；来源收集失败归入 ServiceError::Import。
/// 注意事项：不解锁容器、不写内容，尚未检测目标冲突；可直接提交禁止覆盖计划，也可先 resolve_conflicts。来源目录只收文件，不保留空目录任务。
pub(crate) fn prepare_import(
    selector: &str,
    source: &Path,
    destination: Option<&str>,
) -> Result<ImportPlan, crate::session::ServiceError> {
    let container = prepare_container(selector)?;
    // collect_add_specs 扫描来源（文件或目录树），生成逐文件导入规格。
    let files =
        collect_add_specs(source, destination).map_err(crate::session::ServiceError::Import)?;
    Ok(ImportPlan {
        container,
        files: files
            .into_iter()
            .map(AddFileSpec::without_overwrite)
            .collect(),
        skipped: 0,
    })
}
/// 执行计划中的加密写入与元数据提交，并回报逐文件结果。
/// 入参：plan（ImportPlan）被消费；password（&str）为目标容器密码。
/// 返回值：返回 ImportResult：跳过数、目标路径和对应密文名，后两个列表按下标对应；空计划不打开容器库。
/// 异常：底层身份/密码/版本/覆盖限制/取消/资源锁/加密/数据库/I/O 失败返回 VeilError。
/// 注意事项：本层不重试；add_files 先提交目录创建，再统一提交这批文件引用。失败可能留下目录或不确定提交结果，不能承诺跨磁盘整体回滚；空计划成功不证明密码正确。
pub(crate) fn commit_import(plan: ImportPlan, password: &str) -> Result<ImportResult, VeilError> {
    // 目标路径列表按计划顺序收集，与密文名一一对应。
    let targets = plan.files.iter().map(|file| file.target.clone()).collect();
    let encrypted_names = if plan.files.is_empty() {
        Vec::new()
    } else {
        plan.container.manager().add_files(&plan.files, password)?
    };
    Ok(ImportResult {
        skipped: plan.skipped,
        targets,
        encrypted_names,
    })
}

/// 服务层准备的打包计划：目标容器与输出 .veil 包路径（准备阶段产物）。
pub struct PackPlan {
    /// 目标容器句柄。
    container: ContainerHandle,
    /// 输出包文件路径（准备时校验尚不存在）。
    output: PathBuf,
}
impl PackPlan {
    /// 取得打包计划的容器身份，供提交层协调生命周期。
    /// 入参：self（&PackPlan）为打包计划。
    /// 返回值：&str，计划中句柄保存的容器 ID。
    /// 异常：无错误返回。
    /// 注意事项：不申请锁，不检查输出占用。
    pub(crate) fn container_id(&self) -> &str {
        self.container.id()
    }

    /// 透出打包目标定位时的缓存恢复提示。
    /// 入参：self（&PackPlan）为打包计划。
    /// 返回值：Some(&Path) 表示句柄记录了恢复分支链接，否则 None。
    /// 异常：无错误返回。
    /// 注意事项：不重新恢复链接，也不检查输出包。
    pub fn recovered_link(&self) -> Option<&Path> {
        self.container.recovered_link()
    }
}
/// 打包结果：容器名、关联链接路径、输出包路径、文件数与包大小（用于展示）。
pub struct PackResult {
    /// 容器展示名称。
    pub name: String,
    /// 与容器关联的链接文件路径（若有）。
    pub link_path: Option<PathBuf>,
    /// 最终生成的 .veil 包路径。
    pub output: PathBuf,
    /// 包内文件总数。
    pub file_count: usize,
    /// 包文件大小（字节）。
    pub size: u64,
}

/// 确定待备份容器与包输出位置，并拒绝已观察到的输出占用。
/// 入参：selector（&str）为容器选择器；output（`Option<&str>`）为显式路径，可无 .veil 后缀；None 使用当前目录下原容器名加 .vault.veil。
/// 返回值：返回保存容器句柄和输出 PathBuf 的 PackPlan。
/// 异常：定位失败或输出已存在返回 ServiceError，后者为 WorkflowError::PackOutputExists。
/// 注意事项：默认名称不在此做文件名清理；exists 检查不预留输出，真正提交仍须不覆盖发布。定位可能更新配置或恢复链接。
pub(crate) fn prepare_pack(
    selector: &str,
    output: Option<&str>,
) -> Result<PackPlan, crate::session::ServiceError> {
    let container = prepare_container(selector)?;
    let output = output
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{}.vault.veil", container.name())));
    if output.exists() {
        return Err(WorkflowError::PackOutputExists(output).into());
    }
    Ok(PackPlan { container, output })
}
/// 重新检查输出后生成容器加密备份包，并统计发布结果。
/// 入参：plan（PackPlan）按值消费；password（&str）为容器密码。
/// 返回值：成功返回 PackResult，含定位名称/链接、输出路径、引用文件数及包文件字节数。
/// 异常：观察到占用及打包返回的 Io(AlreadyExists) 映射为 PackOutputExists；其他打包及读取输出元数据错误传播为 ServiceError。
/// 注意事项：without_overwrite 处理检查后竞争；包已发布后同步或统计失败也可能返回错误，因此返回 Err 不等于没有生成文件。
pub(crate) fn commit_pack(
    plan: PackPlan,
    password: &str,
) -> Result<PackResult, crate::session::ServiceError> {
    // 提交时再查一次：两阶段之间可能有其他进程写入了同名文件。
    if plan.output.exists() {
        return Err(WorkflowError::PackOutputExists(plan.output).into());
    }
    let manager = plan.container.manager();
    // 打包器带 without_overwrite：底层拒绝覆盖已存在文件。
    let pack = crate::package::VeilPacker::new(&plan.output)
        .without_overwrite()
        .pack_container(&manager, password);
    // 并发创建导致的 AlreadyExists 统一映射成「输出已存在」工作流错误。
    if let Err(VeilError::Io(ref error)) = pack
        && error.kind() == std::io::ErrorKind::AlreadyExists
    {
        return Err(WorkflowError::PackOutputExists(plan.output).into());
    }
    // count 是包内文件数；再取元数据得包大小。
    let count = pack?;
    let size = std::fs::metadata(&plan.output)
        .map_err(VeilError::Io)?
        .len();
    Ok(PackResult {
        name: plan.container.resolved.veil_name,
        link_path: plan.container.resolved.link_path,
        output: plan.output,
        file_count: count,
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ServiceError, ServiceRequest, ServiceResponse, VeilService};

    /// 建立流式命令测试用的真实临时加密容器与定位句柄。
    /// 入参：无。
    /// 返回值：(TempDir, VeilManager, ContainerHandle)，依次为目录清理守卫、未绑定预期 ID 的管理器、绑定初始化 ID 的句柄。
    /// 异常：准备步骤失败通过 unwrap 引发 panic。
    /// 注意事项：开启进程级快速测试 KDF，须保留 TempDir；本夹具不创建额外来源文件，由具体测试写入。
    fn streaming_fixture() -> (tempfile::TempDir, VeilManager, ContainerHandle) {
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("container");
        let manager = VeilManager::new(directory.clone());
        let metadata = manager.init_veil("test", "password").unwrap();
        let handle = ContainerHandle {
            resolved: ResolvedVeil {
                veil_name: "test".into(),
                veil_id: metadata.veil_id.clone(),
                veil_dir: directory,
                link_path: None,
                recovered_link: false,
            },
        };
        (root, manager, handle)
    }
    #[test]
    /// 验证导入等待冲突选择期间其他文件仍提交，并对映射目标目录持有正确的范围锁。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn command_import_continues_while_waiting_for_a_choice_and_uses_mapped_directory_lock() {
        let (root, manager, handle) = streaming_fixture();
        let folder = root.path().join("source");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("same"), b"original").unwrap();
        std::fs::write(folder.join("clear"), b"clear").unwrap();
        manager
            .add_files(
                &[AddFileSpec::new(folder.join("same"), "renamed/same")],
                "password",
            )
            .unwrap();
        let mut snapshots = Vec::new();
        let mut choices = 0;
        let result = handle
            .stream_import(
                &folder,
                Some("renamed"),
                "password",
                |conflict| {
                    choices += 1;
                    assert_eq!(conflict.target, "renamed/same");
                    assert!(conflict.same_content);
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                    while manager
                        .read_file_bytes("renamed/clear", "password", 100)
                        .is_err()
                    {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "等待选择时其他文件必须继续提交"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(20));
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
                    Ok(ImportDecision::Skip)
                },
                |snapshot| snapshots.push(snapshot),
            )
            .unwrap();
        assert_eq!(choices, 1);
        assert_eq!(
            (
                result.total_files,
                result.completed_files,
                result.skipped_files
            ),
            (2, 2, 1)
        );
        assert_eq!(snapshots.first().unwrap().total_files, 0);
        assert_eq!(snapshots.last().unwrap().phase, "completed");
        assert!(snapshots.last().unwrap().scan_complete);
        assert!(
            manager
                .open_db("password")
                .unwrap()
                .resolve("source")
                .unwrap()
                .is_none()
        );
    }
    #[test]
    /// 验证命令行导出取消后已发布文件保留，未发布的暂存明文被清理。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn command_export_cancellation_preserves_published_files_and_cleans_staging() {
        let (root, manager, handle) = streaming_fixture();
        let source = root.path().join("source");
        std::fs::write(&source, b"original").unwrap();
        manager
            .add_files(
                &[
                    AddFileSpec::new(&source, "album/same"),
                    AddFileSpec::new(&source, "album/clear"),
                ],
                "password",
            )
            .unwrap();
        let output = root.path().join("out");
        std::fs::create_dir_all(output.join("album")).unwrap();
        std::fs::write(output.join("album/same"), b"keep").unwrap();
        let result = handle.stream_export(
            "album",
            &output,
            "password",
            |_| {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                while !output.join("album/clear").exists() {
                    assert!(std::time::Instant::now() < deadline);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(VeilError::Cancelled)
            },
            |_| {},
        );
        assert!(matches!(
            result,
            Err(ServiceError::Core(VeilError::Cancelled))
        ));
        assert_eq!(std::fs::read(output.join("album/same")).unwrap(), b"keep");
        assert_eq!(
            std::fs::read(output.join("album/clear")).unwrap(),
            b"original"
        );
        assert_eq!(std::fs::read_dir(output.join("album")).unwrap().count(), 2);
    }
    #[test]
    /// 验证进度回调发生 panic 时先取消后台传输，再等待线程退出，避免任务泄漏。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    /// 异常：测试准备或断言失败会 panic；场景中预期的错误由测试体捕获或核对，不作为生产错误返回。
    fn command_transfer_callback_panic_cancels_the_worker_before_joining() {
        let (root, manager, handle) = streaming_fixture();
        let source = root.path().join("source");
        std::fs::write(&source, b"original").unwrap();
        manager.add_file(&source, "password").unwrap();
        let output = root.path().join("output");
        std::fs::write(&output, b"keep").unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = handle.stream_export(
                "source",
                &output,
                "password",
                |_| panic!("模拟调用方中断"),
                |_| {},
            );
        }));
        assert!(result.is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"keep");
    }

    #[test]
    /// 验证冲突决议按文件内容保留正确结果，并拒绝基于过期批次的确认。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn import_choices_preserve_content_and_reject_stale_confirmation() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let directory = root.path().join("container");
        let manager = VeilManager::new(directory.clone());
        let metadata = manager.init_veil("test", "password").unwrap();
        let handle = ContainerHandle {
            resolved: ResolvedVeil {
                veil_name: "test".into(),
                veil_id: metadata.veil_id.clone(),
                veil_dir: directory,
                link_path: None,
                recovered_link: false,
            },
        };
        let source = root.path().join("source");
        std::fs::write(&source, b"original").unwrap();
        manager
            .add_files(&[AddFileSpec::new(&source, "file")], "password")
            .unwrap();
        let plan = || ImportPlan {
            container: handle.clone(),
            files: vec![AddFileSpec::new(&source, "file").without_overwrite()],
            skipped: 0,
        };
        assert!(commit_import(plan(), "password").is_err());
        let skipped = plan()
            .resolve_conflicts("password", |conflict| {
                assert!(conflict.same_content);
                Ok(ImportDecision::Skip)
            })
            .unwrap();
        let result = commit_import(skipped, "password").unwrap();
        assert_eq!(result.skipped, 1);
        assert!(result.targets.is_empty());
        std::fs::write(&source, b"edited").unwrap();
        let confirmed = plan()
            .resolve_conflicts("password", |conflict| {
                assert!(!conflict.same_content);
                Ok(ImportDecision::Overwrite)
            })
            .unwrap();
        let concurrent = root.path().join("concurrent");
        std::fs::write(&concurrent, b"concurrent edit").unwrap();
        manager
            .add_files(&[AddFileSpec::new(&concurrent, "file")], "password")
            .unwrap();
        assert!(matches!(
            commit_import(confirmed, "password"),
            Err(VeilError::VersionConflict(_))
        ));
        assert_eq!(
            manager.read_file_bytes("file", "password", 100).unwrap(),
            b"concurrent edit"
        );
        manager.create_directory("folder", "password").unwrap();
        let directory_plan = ImportPlan {
            container: handle,
            files: vec![AddFileSpec::new(&source, "folder")],
            skipped: 0,
        };
        assert!(
            directory_plan
                .resolve_conflicts("password", |conflict| {
                    assert!(conflict.is_directory);
                    Ok(ImportDecision::Overwrite)
                })
                .is_err()
        );
    }

    #[test]
    /// 验证命令请求可执行导入和打包流程，且失败时不破坏已经存在的输出。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn import_and_pack_requests_execute_and_preserve_existing_output() {
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let root = tempfile::TempDir::new().unwrap();
        let directory = root.path().join("container");
        let manager = VeilManager::new(directory.clone());
        let metadata = manager.init_veil("test", "password").unwrap();
        let handle = ContainerHandle {
            resolved: ResolvedVeil {
                veil_name: "test".into(),
                veil_id: metadata.veil_id.clone(),
                veil_dir: directory,
                link_path: None,
                recovered_link: false,
            },
        };
        let source = root.path().join("file.txt");
        std::fs::write(&source, b"hello").unwrap();
        let plan = ImportPlan {
            container: handle.clone(),
            skipped: 0,
            files: vec![AddFileSpec::new(&source, "file.txt")],
        };
        let service = VeilService::default();
        let result = service
            .execute(ServiceRequest::CommitImport {
                plan,
                password: "password",
            })
            .unwrap();
        match result {
            ServiceResponse::Imported(result) => {
                assert_eq!(result.targets, ["file.txt"]);
                assert_eq!(result.encrypted_names.len(), 1);
            }
            _ => panic!("预期返回导入结果"),
        }
        let output = root.path().join("output.veil");
        std::fs::write(&output, b"keep").unwrap();
        let plan = PackPlan {
            container: handle.clone(),
            output: output.clone(),
        };
        assert!(matches!(
            service.execute(ServiceRequest::CommitContainerPack {
                plan,
                password: "password"
            }),
            Err(ServiceError::Workflow(WorkflowError::PackOutputExists(_)))
        ));
        assert_eq!(std::fs::read(&output).unwrap(), b"keep");
        std::fs::remove_file(&output).unwrap();
        let plan = PackPlan {
            container: handle,
            output: output.clone(),
        };
        match service
            .execute(ServiceRequest::CommitContainerPack {
                plan,
                password: "password",
            })
            .unwrap()
        {
            ServiceResponse::ContainerPacked(result) => {
                assert_eq!(result.file_count, 1);
                assert!(result.size > 0);
            }
            _ => panic!("预期返回打包结果"),
        }
        assert!(output.exists());
    }
}
