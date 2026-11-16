//! 文件与目录范围租约，申请、验证、交接均在短写事务内完成。
//!
//! 所属模块：容器存储层并发控制，避免多个进程同时修改同一文件或目录树时产生覆盖和结构冲突。
//! 核心数据：LockManager 持有容器路径、受零化保护的密码和等待策略；Credential 是一张有期限的租约凭证；
//! LeaseGuard 在后台续租并在释放或析构时归还租约；Scope 把文件、目录和整容器资源转换成可比较范围。
//! 核心流程：acquire 在 SQLite 即时写事务中检查冲突并插入租约；validate 在使用前核验本地期限和数据库记录；
//! renew 延长租约；release 删除记录；handoff 原子替换所有者；dead_locks_until 探测失联执行者后由 reap 回收。
//! 依赖能力：SQLite 事务、容器文件树、执行者存活检测、后台线程、敏感字符串零化和结构化日志。
//! 整体目标：让跨线程和跨进程的读写操作遵守同一资源范围规则；租约有期限并可续期，进程退出后可回收，
//! 数据库事务保持短小，不能把耗时的文件传输放在持锁写事务里。
use super::{container::ContainerDb, id, invalid, json, now, parse};
use crate::{
    error::{Result, VeilError},
    execution::{ExecutionEndpoint, ExecutionOwner, ExecutionServer, ExecutionService, Liveness},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
pub use veil_core::locking::{LockMode, LockOptions};
use veil_core::logging::{self, Field, Level};
use zeroize::Zeroizing;

/// 一次租约在数据库中的有效时长；守卫需要在到期前续租。
pub const LEASE: Duration = Duration::from_secs(30);
/// 后台守卫检查并续租的常规间隔，短于租约有效时长以留出数据库重试余量。
pub const RENEW_INTERVAL: Duration = Duration::from_secs(5);
/// SQLite 写锁短暂竞争时，在租约尚未失效前继续尝试续租。
const RENEW_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const RENEW_MAX_RETRIES: u32 = 16;
/// 遇到资源冲突后再次申请前的等待间隔，避免忙等占用 CPU。
pub const BACKOFF: Duration = Duration::from_millis(20);
/// 未传入自定义选项时等待锁的最长时间。
pub const DEFAULT_WAIT: Duration = Duration::from_secs(30);
const PROBE_BATCH_BUDGET: Duration = Duration::from_millis(250);
/// 创建默认锁等待策略：最多等待 DEFAULT_WAIT，且不关联外部取消标志。
/// 返回：可直接传给 LockManager::with_options 的默认配置。
/// 入参：无。
/// 异常：不访问数据库，不返回错误。
/// 注意事项：默认 30 秒只约束等锁，不等于已持有租约的有效时长或业务操作总时限。
pub fn default_options() -> LockOptions {
    LockOptions {
        timeout: Some(DEFAULT_WAIT),
        cancellation: None,
    }
}
static SERVER: OnceLock<ExecutionServer> = OnceLock::new();
/// 取得进程共享的身份探测端点，帮助其他进程提前回收已结束持有者的租约。
/// 入参：execution（&ExecutionService）为本进程登记服务。
/// 返回值：服务可用时返回 Some(ExecutionEndpoint) 副本，启动失败为 None。
/// 异常：监听失败记日志后返回 None，不阻止租约按到期规则工作。
/// 注意事项：并发首次调用可能各自启动服务；OnceLock 只保留一个，未保存的临时服务释放时会停止并等待线程。
fn endpoint(execution: &ExecutionService) -> Option<ExecutionEndpoint> {
    // 全进程共享一个监听端点，避免每个锁管理器都重复打开执行者探测服务。
    if SERVER.get().is_none() {
        let server = match execution.listen() {
            Ok(server) => server,
            Err(error) => {
                logging::failure(
                    Level::Warn,
                    "locks",
                    "owner_probe",
                    "OWNER_PROBE_UNAVAILABLE",
                    &error,
                    &[],
                );
                return None;
            }
        };
        let _ = SERVER.set(server);
    }
    SERVER.get().map(|server| server.endpoint().clone())
}
// 复用现有数据库的 read/write 约束，不改变旧容器表结构。
const SHARED_WRITE_PREFIX: &str = "veil-sw:";
const READ_ONLY_PREFIX: &str = "veil-ro:";
const DIRECTORY_PREFIX: &str = "veil-dir:";
const CONTAINER_RESOURCE: &str = "veil-container";
const PENDING_PREFIX: &str = "veil-pending:";

/// 锁资源解码后的逻辑范围，用于判断两把租约是否涉及重叠文件树。
#[derive(Debug, Clone)]
enum Scope<'a> {
    /// 整个容器；与容器中的任意文件或目录都重叠。
    Container,
    /// 单个文件或预留文件。
    File {
        /// 文件节点 ID；预留项可能暂时还没有对应的文件行。
        id: &'a str,
        /// 文件所属目录 ID；普通文件可没有显式父级时退回用文件 ID 判断。
        parent: Option<&'a str>,
    },
    /// 指定目录及其后代节点。
    Directory(&'a str),
}

/// 去掉锁类型前缀并把资源编码还原成容器、文件或目录范围。
/// 入参 resource：数据库记录或调用方资源字符串。返回：供重叠判定使用的逻辑 Scope。
/// 注意：此解析只解释资源格式，不验证对应文件节点是否还存在；申请锁时另行检查。
/// 异常：解析不返回 Result；不符合专用前缀的字符串按文件范围解释。
/// 注意事项：resource: &str 的借用必须覆盖返回 Scope 的使用期，解析不验证目录是否存在。
fn decode_scope(resource: &str) -> Scope<'_> {
    let resource = resource
        .strip_prefix(SHARED_WRITE_PREFIX)
        .or_else(|| resource.strip_prefix(READ_ONLY_PREFIX))
        .unwrap_or(resource);
    if resource == CONTAINER_RESOURCE {
        Scope::Container
    } else if let Some(directory) = resource.strip_prefix(DIRECTORY_PREFIX) {
        Scope::Directory(directory)
    } else if let Some(pending) = resource.strip_prefix(PENDING_PREFIX) {
        match pending.split_once(':') {
            Some((id, parent)) => Scope::File {
                id,
                parent: Some(parent),
            },
            None => Scope::File {
                id: resource,
                parent: None,
            },
        }
    } else {
        Scope::File {
            id: resource,
            parent: None,
        }
    }
}

/// 查询 file_info 父子关系，判断 ancestor 是否为 descendant 本身或其祖先目录。
/// 入参 conn：当前事务使用的 SQLite 连接；ancestor/descendant：文件树节点 ID。
/// 返回：存在祖先关系时 true。异常：递归查询失败时返回数据库错误。注意：调用方应在事务中调用，
/// 使锁冲突判定看到一致的目录树。
fn is_ancestor(conn: &Connection, ancestor: &str, descendant: &str) -> Result<bool> {
    // 从目标节点出发，逐层沿 parent_id 向上查找；包含起点，因此节点也是自己的祖先。
    // EXISTS 只回答是否找到目标祖先，不把整条父链加载到应用内存。
    Ok(conn.query_row("WITH RECURSIVE parents(id,parent_id) AS (SELECT id,parent_id FROM file_info WHERE id=?1 UNION ALL SELECT f.id,f.parent_id FROM file_info f JOIN parents p ON f.id=p.parent_id) SELECT EXISTS(SELECT 1 FROM parents WHERE id=?2)", params![descendant, ancestor], |row| row.get(0))?)
}

/// 判断两个逻辑资源范围是否相交；目录关系通过 SQLite 文件树递归查询确认。
/// 入参 conn：当前事务连接；left/right：已解码的资源范围。返回：范围重叠时 true。
/// 异常：读取祖先关系失败时返回数据库错误。容器范围始终与任何范围重叠。
/// 注意事项：资源比较依赖数据库当前文件树，宜在申请/验证使用的同一事务中调用。
/// 文件范围比较文件 ID；目录对预留文件则比较它记录的 parent，而不是尚未建库的文件节点。
fn scopes_overlap(conn: &Connection, left: &Scope<'_>, right: &Scope<'_>) -> Result<bool> {
    match (left, right) {
        (Scope::Container, _) | (_, Scope::Container) => Ok(true),
        (Scope::File { id: a, .. }, Scope::File { id: b, .. }) => Ok(a == b),
        (Scope::Directory(a), Scope::Directory(b)) => {
            Ok(is_ancestor(conn, a, b)? || is_ancestor(conn, b, a)?)
        }
        (Scope::Directory(dir), Scope::File { id, parent })
        | (Scope::File { id, parent }, Scope::Directory(dir)) => {
            is_ancestor(conn, dir, parent.unwrap_or(id))
        }
    }
}

/// 查找与请求资源冲突的有效租约。
/// 入参 conn：申请事务连接；resource：待申请资源；mode：读、写或共享写模式。返回：无冲突为 None，
/// 有冲突时返回诊断级别和占用资源 ID。异常：查询或目录范围比较失败时返回数据库错误。
/// 注意：调用方必须在同一即时写事务里随后插入新租约，避免检查后其他进程抢先申请。
fn conflict(conn: &Connection, resource: &str, mode: LockMode) -> Result<Option<(String, String)>> {
    // 只取尚未到期的锁：第一列是被保护的资源编码，第二列是数据库中的读/写模式。
    // 收集结果后逐条查询目录祖先关系，以判断不同资源名是否仍覆盖同一目录树。
    let mut statement =
        conn.prepare("SELECT resource_id,mode FROM operation_locks WHERE expires_at>?1")?;
    let active = statement
        .query_map([now()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // 按资源范围检查每一条尚未到期的租约；忽略按语义可并行的只读或共享读取。
    for (other, other_mode) in active {
        // 两个共享读租约允许共存；普通只读标记也不阻挡其他申请。
        if other.starts_with(READ_ONLY_PREFIX) || (mode == LockMode::Read && other_mode == "read") {
            continue;
        }
        if scopes_overlap(conn, &decode_scope(resource), &decode_scope(&other))? {
            // 返回第一条冲突即可；标签帮助调用方说明是何种访问占用了资源。
            let level = if other.starts_with(SHARED_WRITE_PREFIX) {
                "SW"
            } else if other_mode == "read" {
                "SR"
            } else {
                "EW"
            };
            return Ok(Some((level.into(), other)));
        }
    }
    Ok(None)
}

/// 为“不稳定读取”查找是否存在覆盖该资源的排他写租约。
/// 入参 conn：申请事务连接；resource：需要读取的范围。返回：第一条冲突写租约资源 ID 或 None。
/// 异常：数据库读取或祖先判断失败时返回错误。共享写锁不在此处视作排他写锁。
/// 注意事项：只返回第一条匹配的排他写租约，不申请新锁；普通读取不登记租约，后续写入仍可能发生。
fn conflict_with_exclusive(conn: &Connection, resource: &str) -> Result<Option<String>> {
    // 普通只读不保存租约，只需排除当前已存在且范围重叠的排他写入。
    let mut statement = conn
        .prepare("SELECT resource_id FROM operation_locks WHERE expires_at>?1 AND mode='write'")?;
    for other in statement.query_map([now()], |row| row.get::<_, String>(0))? {
        let other = other?;
        // 共享写也以 write 落库，必须结合资源前缀将它从排他写中区分出来。
        if !other.starts_with(SHARED_WRITE_PREFIX)
            && scopes_overlap(conn, &decode_scope(resource), &decode_scope(&other))?
        {
            return Ok(Some(other));
        }
    }
    Ok(None)
}

/// 在已有写事务内检查受影响的资源，避免目录结构变更绕过范围锁。
/// 入参 conn：当前写事务连接；resource：将被变更的资源；mode：变更所需的锁模式。
/// 返回：无冲突时成功；异常：锁冲突时返回 LockConflict，数据库检查失败时返回数据库错误。
/// 注意：必须在实际修改目录项的同一事务中调用。
pub(crate) fn ensure_available(conn: &Connection, resource: &str, mode: LockMode) -> Result<()> {
    if let Some((level, scope)) = conflict(conn, resource, mode)? {
        Err(VeilError::LockConflict { level, scope })
    } else {
        Ok(())
    }
}

/// 修改目录项只与覆盖该位置的目录租约冲突，不锁住同目录的其他文件。
/// 确认修改某目录的子项不会违反容器锁或覆盖该目录的租约。
/// 入参 conn：执行目录修改的写事务；parent：父目录节点 ID。返回：无冲突时成功。
/// 异常：冲突返回 LockConflict，SQL 或祖先关系检查失败返回数据库错误。注意：同目录其他文件的独立文件锁不阻止新增兄弟项。
pub(crate) fn ensure_parent_available(conn: &Connection, parent: &str) -> Result<()> {
    // 新增或移除子项会改变父目录，因此只检查目录范围和整个容器的有效租约。
    let mut statement = conn.prepare("SELECT resource_id,mode FROM operation_locks WHERE expires_at>?1 AND (resource_id LIKE 'veil-dir:%' OR resource_id LIKE 'veil-sw:veil-dir:%' OR resource_id=?2 OR resource_id=?3)")?;
    let directories = statement
        .query_map(
            params![
                now(),
                CONTAINER_RESOURCE,
                format!("{SHARED_WRITE_PREFIX}{CONTAINER_RESOURCE}")
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // 每条记录代表一个可能覆盖父目录的租约；先识别模式，再检查覆盖范围。
    for (directory, mode) in directories {
        let level = if directory.starts_with(SHARED_WRITE_PREFIX) {
            "SW"
        } else if mode == "read" {
            "SR"
        } else {
            "EW"
        };
        if matches!(decode_scope(&directory), Scope::Container) {
            // 整容器租约覆盖所有目录，无须继续向上查找。
            return Err(VeilError::LockConflict {
                level: level.into(),
                scope: directory,
            });
        }
        if let Scope::Directory(identity) = decode_scope(&directory) {
            // 租约目录等于父目录或位于其上层时，修改会触及受保护的目录树。
            if is_ancestor(conn, identity, parent)? {
                return Err(VeilError::LockConflict {
                    level: level.into(),
                    scope: directory,
                });
            }
        }
    }
    Ok(())
}

/// 为一个目录 ID 生成可识别的锁资源名。入参 id：容器目录节点 ID；返回：带目录前缀的资源键。
/// 异常：不访问数据库、不返回错误。
/// 注意事项：id: &str 只拼接为 veil-dir: 前缀资源键，空值与节点存在性由申请阶段验证。
pub(crate) fn directory_resource(id: &str) -> String {
    format!("{DIRECTORY_PREFIX}{id}")
}
/// 为尚未落入文件表的文件预留项生成锁资源名。入参 id：预留文件 ID；parent：预期父目录 ID。
/// 返回：同时编码文件身份和父目录的资源键，供导入时保护新增位置。
/// 入参：id: &str 为待创建文件 ID；parent: &str 为父目录 ID，均应来自容器身份生成流程。
/// 返回值：String 类型的预留文件资源编码，用于文件入库前的目录范围冲突判定。
/// 异常：只编码字符串，不返回错误。
/// 注意事项：不新建数据库节点；调用方须提供有效父目录并避免自行拼接冲突的分隔格式。
pub(crate) fn pending_resource(id: &str, parent: &str) -> String {
    format!("{PENDING_PREFIX}{id}:{parent}")
}
/// 管理单个容器数据库中的锁租约申请、续期、释放和所有者交接。
#[derive(Clone)]
pub struct LockManager {
    /// 容器数据库所在目录。
    directory: PathBuf,
    /// 解锁容器的密码；通过 Zeroizing 在释放内存时尽量清除明文。
    password: Arc<Zeroizing<String>>,
    /// 等待期限和可选取消标志。
    options: LockOptions,
}
/// 持有者用来验证、续租和释放一条租约的凭证。
#[derive(Clone)]
pub struct Credential {
    /// 数据库中该租约的唯一 ID。
    pub lock_id: String,
    /// 被保护的资源及锁类型前缀。
    pub resource_id: String,
    /// 本次申请的随机批次令牌，防止旧凭证误释放新租约。
    token: String,
    /// 申请时登记的执行者身份。
    owner: ExecutionOwner,
    /// 进程单调时钟记录的签发时刻，用于本地过期判定。
    issued: Instant,
    /// 墙上时钟签发时间，对照数据库过期时间并检测时钟跳变。
    wall_issued: i64,
    /// 进程单调时钟下的本地有效截止时间。
    valid_until: Instant,
}
impl std::fmt::Debug for Credential {
    /// 只显示锁 ID 和资源，不暴露令牌、执行者身份或时钟凭据。
    /// 入参：self: &Credential 为待诊断凭证；f 为标准库格式化输出器。
    /// 返回值：成功输出有限字段时为 Ok(())，输出端失败为 fmt::Error。
    /// 异常：不访问数据库，输出失败只通过 fmt::Result 传播。
    /// 注意事项：资源键本身仍可能属于业务信息，写入日志应继续遵守日志脱敏入口。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("lock_id", &self.lock_id)
            .field("resource_id", &self.resource_id)
            .finish_non_exhaustive()
    }
}
impl Credential {
    /// 判断凭证是否代表可与普通读取并存的共享写模式。
    /// 入参：self: &Credential 为待识别凭证。
    /// 返回值：资源键带共享写前缀时为 true，否则 false。
    /// 异常：不返回错误。
    /// 注意事项：只识别编码，不验证期限、令牌或数据库所有权。
    pub fn is_shared_write(&self) -> bool {
        self.resource_id.starts_with(SHARED_WRITE_PREFIX)
    }
    /// 普通读取不保证读取内容稳定。
    /// 判断该凭证是否为不持久化数据库行的普通读取标记。
    /// 入参：self: &Credential 为待识别凭证。
    /// 返回值：资源键带普通只读前缀时为 true，否则 false。
    /// 异常：不返回错误。
    /// 注意事项：true 表示不依靠持久化租约阻止写入，不保证内容稳定。
    pub fn is_read_only(&self) -> bool {
        self.resource_id.starts_with(READ_ONLY_PREFIX)
    }
    /// 若资源范围是目录，返回其节点 ID；文件和整容器资源返回 None。
    /// 入参：self: &Credential 为持有资源编码的凭证。
    /// 异常：不返回错误。
    /// 注意事项：返回 &str 借用凭证内资源键；不验证该节点当前仍存在。
    pub(crate) fn directory_id(&self) -> Option<&str> {
        match decode_scope(&self.resource_id) {
            Scope::Directory(id) => Some(id),
            Scope::File { .. } | Scope::Container => None,
        }
    }
    /// 若资源范围是文件或预留文件，返回其节点 ID；目录和整容器返回 None。
    /// 入参：self: &Credential 为持有资源编码的凭证。
    /// 异常：不返回错误。
    /// 注意事项：返回 &str 借用凭证内编码；预留文件可能尚未写入 file_info。
    pub(crate) fn file_id(&self) -> Option<&str> {
        match decode_scope(&self.resource_id) {
            Scope::File { id, .. } => Some(id),
            Scope::Directory(_) | Scope::Container => None,
        }
    }
    /// 仅凭本进程记录检查期限，并发现系统时钟跳变造成的不一致。
    /// 入参：self: &Credential 为携带签发时间和截止时间的凭证。
    /// 返回值：单调期限未过且系统时钟与单调耗时偏差小于 1000 毫秒时为 true。
    /// 异常：不访问数据库，不返回 Result。
    /// 注意事项：true 只说明本地时间检查通过，不能代替数据库中令牌、所有者与到期时间检查。
    fn local_valid(&self) -> bool {
        let elapsed = self.issued.elapsed().as_millis() as i64;
        Instant::now() < self.valid_until && (now() - self.wall_issued - elapsed).abs() < 1000
    }
}
impl LockManager {
    /// 创建针对一个容器数据库的锁管理器。
    /// 入参 directory：容器目录；password：解锁该容器数据库的密码。返回：使用默认等待策略的管理器。
    /// 注意：密码会复制到零化包装中；构造时不打开数据库，也不验证路径或密码。
    /// 异常：只保存路径与密码副本，不返回错误。
    /// 注意事项：directory 可为相对路径；原始 password 借用不由管理器清零，内部副本最终释放时单独清零。
    pub fn new(directory: impl AsRef<Path>, password: &str) -> Self {
        Self {
            directory: directory.as_ref().into(),
            password: Arc::new(Zeroizing::new(password.into())),
            options: default_options(),
        }
    }
    /// 替换申请锁时的超时与取消选项，并返回配置后的管理器。
    /// 入参 options：调用方定义的等待时长和可选取消标志。返回：可继续使用的管理器。
    /// 异常：不访问数据库，也不验证 timeout 是否为零。
    /// 注意事项：options 按值替换本实例；修改只影响后续申请等待，不改变既有租约的期限或续租间隔。
    pub fn with_options(mut self, options: LockOptions) -> Self {
        self.options = options;
        self
    }
    /// 为当前执行上下文申请一个或多个资源锁，并启动自动续租守卫。
    /// 入参 resources：容器内资源键；mode：读、只读、共享写或排他写模式。返回：持有租约的 LeaseGuard。
    /// 异常：运行时不可用、锁冲突、超时、取消或数据库错误时返回 VeilError。注意：资源会排序去重，
    /// 守卫应与真正持有资源的任务同生命周期，不能仅保留等待句柄。
    pub fn acquire(&self, resources: &[String], mode: LockMode) -> Result<LeaseGuard> {
        let execution = ExecutionService::global()?;
        let owner = execution.runtime().current_context().owner().clone();
        self.acquire_for(resources, mode, owner, endpoint(&execution))
    }
    /// 申请覆盖指定目录及其后代的锁。
    /// 入参 identity：容器目录节点 ID；mode：访问模式。返回：自动续租的租约守卫。
    /// 异常：ID 为空、目录不存在、范围冲突或数据库失败时返回错误。
    /// 注意事项：identity: &str 应为现有目录 ID；资源范围包括后代，冲突仍按访问模式比较。
    pub fn acquire_directory(&self, identity: &str, mode: LockMode) -> Result<LeaseGuard> {
        if identity.is_empty() {
            return Err(invalid("目录锁资源不能为空"));
        }
        self.acquire(&[directory_resource(identity)], mode)
    }
    /// 申请覆盖整个容器的锁；与文件和目录的范围都重叠，是否冲突还取决于读写模式兼容性。
    /// 入参 mode：访问模式。返回：自动续租的租约守卫。异常：锁冲突、超时、取消或数据库失败时返回错误。
    /// 注意事项：mode: LockMode 决定兼容性；共享读之间可共存，不能把整容器范围理解为必定排他。
    pub fn acquire_container(&self, mode: LockMode) -> Result<LeaseGuard> {
        self.acquire(&[CONTAINER_RESOURCE.into()], mode)
    }
    /// 一次申请多个目录范围的锁，避免逐目录申请时出现部分成功的中间状态。
    /// 入参 identities：非空目录 ID 集合；mode：访问模式。返回：统一管理这些租约的守卫。
    /// 异常：ID 集合为空、含空 ID、目录无效、锁冲突或数据库失败时返回错误。
    /// 注意事项：identities: &[String] 每项是一条目录身份，不是磁盘路径；会转换资源键后统一排序去重。
    pub(crate) fn acquire_directories(
        &self,
        identities: &[String],
        mode: LockMode,
    ) -> Result<LeaseGuard> {
        if identities.is_empty() || identities.iter().any(|id| id.is_empty()) {
            return Err(invalid("目录锁资源不能为空"));
        }
        self.acquire(
            &identities
                .iter()
                .map(|id| directory_resource(id))
                .collect::<Vec<_>>(),
            mode,
        )
    }
    /// 以指定执行者身份申请资源锁，供跨任务交接或服务代理调用。
    /// 入参 resources：资源键集合；mode：锁模式；owner：数据库记录的所有者；endpoint：可选存活探测端点。
    /// 返回：开始后台续租的 LeaseGuard。异常：参数无效、冲突、等待超时、取消、执行服务或数据库失败。
    /// 注意：资源由一个即时写事务统一检查和登记；失败日志会记录资源、模式、次数和耗时。
    pub fn acquire_for(
        &self,
        resources: &[String],
        mode: LockMode,
        owner: ExecutionOwner,
        endpoint: Option<ExecutionEndpoint>,
    ) -> Result<LeaseGuard> {
        let started = Instant::now();
        logging::remember_sensitive(self.password.as_str());
        let mut attempts = 0;
        let result = self.acquire_for_inner(resources, mode, owner, endpoint, &mut attempts);
        let context = [
            ("database", Field::Path(&self.directory)),
            (
                "resource",
                Field::Resource(resources.first().map(String::as_str).unwrap_or("container")),
            ),
            ("lock_count", Field::Count(resources.len() as u64)),
            ("mode", Field::Label(diagnostic_mode(mode))),
            ("elapsed_ms", Field::Duration(started.elapsed())),
            ("attempts", Field::Count(attempts)),
            (
                "timeout_ms",
                Field::Duration(self.options.timeout.unwrap_or(Duration::MAX)),
            ),
        ];
        match &result {
            Err(VeilError::WorkError(message)) if message == "锁等待已取消" => {}
            Err(error) => logging::failure(
                if matches!(error, VeilError::LockConflict { .. }) {
                    Level::Warn
                } else {
                    Level::Error
                },
                "locks",
                "acquire",
                "LOCK_ACQUIRE_FAILED",
                error,
                &context,
            ),
            Ok(_) if started.elapsed() >= Duration::from_secs(1) => logging::event(
                Level::Warn,
                "locks",
                "acquire",
                "LOCK_WAIT_RECOVERED",
                "锁等待结束，已取得租约",
                &context,
            ),
            _ => {}
        }
        result
    }
    /// 循环尝试申请锁，直到成功、收到取消、等待超时或遇到不可恢复错误。
    /// 入参 resources/mode/owner/endpoint：申请内容和持有者；attempts：由调用方累计的诊断计数。
    /// 返回：所有请求租约一次性建立后的守卫。异常：资源格式错误、冲突等待耗尽、取消或 SQL 失败。
    /// 注意事项：attempts: &mut u64 只统计单次数据库申请次数；超时且已有资源冲突时优先返回最后的 LockConflict。
    fn acquire_for_inner(
        &self,
        resources: &[String],
        mode: LockMode,
        owner: ExecutionOwner,
        endpoint: Option<ExecutionEndpoint>,
        attempts: &mut u64,
    ) -> Result<LeaseGuard> {
        // 排序并去重，统一检查与记录顺序；整组申请在单个写事务内完成，
        // 并非先持有部分资源再等待剩余资源，不应把此排序理解为独立锁的防死锁算法。
        let mut resources = resources.to_vec();
        resources.sort();
        resources.dedup();
        if resources.iter().any(|r| {
            r.is_empty() || r.starts_with(SHARED_WRITE_PREFIX) || r.starts_with(READ_ONLY_PREFIX)
        }) {
            return Err(invalid("文件锁资源不能为空"));
        }
        let start = Instant::now();
        let mut last_conflict = None;
        let mut reported_wait = false;
        // 每轮先响应取消/期限，再用短数据库等待尝试原子申请；冲突后退避再试。
        loop {
            if let Err(error) = self.check_wait(start) {
                return Err(
                    if matches!(&error, VeilError::WorkError(message) if message == "锁等待超时")
                    {
                        last_conflict.unwrap_or(error)
                    } else {
                        error
                    },
                );
            }
            let timeout = self
                .options
                .timeout
                .map(|d| d.saturating_sub(start.elapsed()))
                .unwrap_or(Duration::from_millis(100))
                .min(Duration::from_millis(100));
            // 每次数据库等待最多 100 毫秒，让外层有机会重新检查取消标志和总期限。
            *attempts += 1;
            let result =
                self.try_acquire(&resources, mode, &owner, endpoint.as_ref(), timeout, start);
            if !reported_wait && start.elapsed() >= Duration::from_secs(1) {
                if let Err(error) = &result {
                    logging::failure(
                        Level::Warn,
                        "locks",
                        "acquire",
                        "LOCK_WAITING",
                        error,
                        &[
                            ("database", Field::Path(&self.directory)),
                            (
                                "resource",
                                Field::Resource(
                                    resources.first().map(String::as_str).unwrap_or("container"),
                                ),
                            ),
                            ("elapsed_ms", Field::Duration(start.elapsed())),
                            ("attempts", Field::Count(*attempts)),
                            ("mode", Field::Label(diagnostic_mode(mode))),
                        ],
                    );
                    reported_wait = true;
                }
            }
            match result {
                Ok(credentials) => return LeaseGuard::start(self.clone(), credentials),
                // 数据库正忙或资源被占用都可以重试；其他错误通常无法靠等待自行恢复。
                Err(VeilError::Database(ref e)) if busy(e) => {}
                Err(error @ VeilError::LockConflict { .. }) => last_conflict = Some(error),
                Err(VeilError::WorkError(ref message))
                    if message == "锁等待超时" && last_conflict.is_some() =>
                {
                    return Err(last_conflict.unwrap());
                }
                Err(e) => return Err(e),
            }
            std::thread::sleep(BACKOFF);
        }
    }
    /// 检查一次锁等待是否已收到取消请求或超过配置的期限。
    /// 入参 start：本次等待开始的单调时钟时刻。返回：仍可等待时成功；取消或超时返回 WorkError。
    /// 异常：取消或超时返回 WorkError；取消检查优先于期限检查。
    /// 注意事项：start: Instant 是当前调用等待起点；timeout=None 允许无限等待，零时限会在首次尝试前失败。
    fn check_wait(&self, start: Instant) -> Result<()> {
        if self
            .options
            .cancellation
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Acquire))
        {
            return Err(VeilError::WorkError("锁等待已取消".into()));
        }
        if self.options.timeout.is_some_and(|d| start.elapsed() >= d) {
            return Err(VeilError::WorkError("锁等待超时".into()));
        }
        Ok(())
    }
    /// 在单次 SQLite 即时写事务内回收失效租约、检查资源范围并登记整组新租约。
    /// 入参 resources/mode/owner/endpoint：申请信息；timeout：本轮数据库锁等待时限；start：总等待起点。
    /// 返回：与申请项对应的凭证集合。异常：资源不存在、冲突、取消、数据库忙或事务写入失败。
    /// 注意：冲突判断、旧租约回收与插入在同一事务中完成，不能拆成多个事务。
    fn try_acquire(
        &self,
        resources: &[String],
        mode: LockMode,
        owner: &ExecutionOwner,
        endpoint: Option<&ExecutionEndpoint>,
        timeout: Duration,
        start: Instant,
    ) -> Result<Vec<Credential>> {
        let mut db =
            ContainerDb::open_with_timeout(&self.directory, &self.password, None, timeout)?;
        let deadline = self.options.timeout.map(|d| start + d);
        // 祖先或后代目录也可能覆盖请求范围，因此先探测当前容器的全部存活者，而非只查同名资源。
        let dead = dead_locks_until(
            &db.conn,
            None,
            deadline,
            self.options.cancellation.as_deref(),
        )?;
        self.check_wait(start)?;
        // 即时写事务先取得 SQLite 写入资格，使检查冲突和插入租约之间不会被其他写事务插队。
        let tx = db
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        reap(&tx, &dead)?;
        self.check_wait(start)?;
        // 记录每项是否可降为普通只读凭证；稍后与资源列表按位置一一配对写入。
        let mut downgraded = Vec::with_capacity(resources.len());
        // 逐个验证目标节点、父目录和范围冲突；全部通过前不写入任何锁行。
        for resource in resources {
            self.check_wait(start)?;
            if let Scope::Directory(identity) = decode_scope(resource) {
                // 目录锁必须指向真实目录，避免为不存在的目录树建立保护范围。
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM file_info WHERE id=?1 AND kind='directory')",
                    [identity],
                    |row| row.get(0),
                )?;
                if !exists {
                    return Err(invalid("目录锁目标不存在"));
                }
            }
            if let Scope::File {
                id,
                parent: Some(parent),
            } = decode_scope(resource)
            {
                if id.is_empty() || parent.is_empty() {
                    return Err(invalid("文件锁资源无效"));
                }
                let actual: Option<Option<String>> = tx
                    .query_row("SELECT parent_id FROM file_info WHERE id=?1", [id], |row| {
                        row.get(0)
                    })
                    .optional()?;
                // 已存在的文件必须属于声明的父目录；尚未创建的文件则要求父目录真实存在。
                match actual {
                    Some(Some(actual)) if actual == parent => {}
                    Some(_) => return Err(invalid("文件锁父目录不匹配")),
                    None => {
                        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM file_info WHERE id=?1 AND kind='directory')", [parent], |row| row.get(0))?;
                        if !exists {
                            return Err(invalid("文件锁父目录不存在"));
                        }
                    }
                }
            }
            // 普通读取不创建租约行，但仍需检查排他写租约；其余模式按完整冲突规则比较。
            let read_only = mode == LockMode::ReadOnly;
            if read_only {
                if let Some(scope) = conflict_with_exclusive(&tx, resource)? {
                    return Err(VeilError::LockConflict {
                        level: "EW".into(),
                        scope,
                    });
                }
            } else if let Some((level, scope)) = conflict(&tx, resource, mode)? {
                return Err(VeilError::LockConflict { level, scope });
            }
            downgraded.push(read_only);
        }
        let time = now();
        let issued = Instant::now();
        // 结果集合与去重后的资源列表一一对应；同批凭证共用令牌，便于识别本次申请。
        let mut credentials = Vec::new();
        let token = id();
        // 所有资源均通过检查后，用同一令牌逐项登记租约，保证成组申请一起提交或回滚。
        for (resource, read_only) in resources.iter().zip(downgraded) {
            let stored_resource = if mode == LockMode::SharedWrite {
                format!("{SHARED_WRITE_PREFIX}{resource}")
            } else if read_only {
                format!("{READ_ONLY_PREFIX}{resource}")
            } else {
                resource.clone()
            };
            let credential = Credential {
                lock_id: id(),
                resource_id: stored_resource.clone(),
                token: token.clone(),
                owner: owner.clone(),
                issued,
                wall_issued: time,
                valid_until: issued + LEASE,
            };
            if !read_only {
                tx.execute(
                    "INSERT INTO operation_locks VALUES (?1,'file',?2,?3,?4,?5,?6,?7)",
                    params![
                        credential.lock_id,
                        stored_resource,
                        mode.as_str(),
                        json(owner)?,
                        endpoint.map(json).transpose()?,
                        credential.token,
                        time + LEASE.as_millis() as i64
                    ],
                )?;
            }
            credentials.push(credential);
        }
        self.check_wait(start)?;
        tx.commit()?;
        Ok(credentials)
    }
    /// 尝试延长所给凭证的有效期，供手动管理租约的调用方使用。
    /// 入参：credentials（&mut [Credential]）为本容器凭证，每项对应一项资源；成功后原位更新本地签发和到期时间。
    /// 返回值：整批更新提交成功返回 Ok(())；空组或全为普通只读标记直接成功。
    /// 异常：凭证失效、数据库打开/事务/写入/提交失败返回 VeilError。
    /// 注意事项：本入口不进行数据库忙重试；单次数据库操作自身仍可能等待。不要与使用旧快照的业务操作并发混用。
    pub fn renew(&self, credentials: &mut [Credential]) -> Result<()> {
        self.renew_with_retry(
            credentials,
            veil_core::database::BusyRetryPolicy {
                max_retries: 0,
                deadline: Instant::now(),
                retry_interval: Duration::ZERO,
            },
        )
    }
    /// 在有限数据库忙重试范围内续租，并记录失败或慢续租诊断。
    /// 入参：credentials 为待更新凭证切片；retry（BusyRetryPolicy）含最多重试次数、允许开始重试的截止时刻和间隔。
    /// 返回值：续租成功为 Ok(())。
    /// 异常：凭证失效或最终数据库错误直接返回；仅数据库忙类错误可重试。
    /// 注意事项：截止时刻限制重试开始，不保证整个函数按时结束；外层守卫调用期间持有凭证 Mutex，其他凭证读取会等待。
    fn renew_with_retry(
        &self,
        credentials: &mut [Credential],
        retry: veil_core::database::BusyRetryPolicy,
    ) -> Result<()> {
        let started = Instant::now();
        logging::remember_sensitive(self.password.as_str());
        let remaining = credentials
            .iter()
            .map(|c| c.valid_until.saturating_duration_since(Instant::now()))
            .min()
            .unwrap_or_default();
        let result = veil_core::database::retry_busy(retry, || self.renew_inner(credentials));
        let context = [
            ("database", Field::Path(&self.directory)),
            (
                "resource",
                Field::Resource(
                    credentials
                        .first()
                        .map(|c| c.resource_id.as_str())
                        .unwrap_or("none"),
                ),
            ),
            (
                "lock_id",
                Field::Resource(
                    credentials
                        .first()
                        .map(|c| c.lock_id.as_str())
                        .unwrap_or("none"),
                ),
            ),
            ("lock_count", Field::Count(credentials.len() as u64)),
            ("remaining_lease_ms", Field::Duration(remaining)),
            ("lease_ms", Field::Duration(LEASE)),
            ("renew_interval_ms", Field::Duration(RENEW_INTERVAL)),
            ("elapsed_ms", Field::Duration(started.elapsed())),
        ];
        if let Err(error) = &result {
            logging::failure(
                Level::Error,
                "locks",
                "renew",
                "LEASE_RENEW_FAILED",
                error,
                &context,
            );
        } else if started.elapsed() >= Duration::from_secs(1) {
            logging::event(
                Level::Warn,
                "locks",
                "renew",
                "LEASE_RENEW_SLOW",
                "租约续租耗时过长",
                &context,
            );
        }
        result
    }
    /// 在即时写事务中验证整组租约并延长到期时间，再同步刷新本地期限。
    /// 入参 credentials：待续租凭证。返回：数据库和本地有效期均更新时成功。
    /// 异常：凭证过期、归属变化或数据库访问失败时返回错误。
    /// 注意事项：credentials: &mut [Credential] 每项是一条同组租约；空组或全是普通只读标记直接成功，
    /// 不打开数据库；持久化租约先统一验证，再提交更新，最后刷新本地时钟字段。
    fn renew_inner(&self, credentials: &mut [Credential]) -> Result<()> {
        if credentials.iter().all(Credential::is_read_only) {
            return Ok(());
        }
        let mut db = ContainerDb::open(&self.directory, &self.password, None)?;
        let tx = db
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // 先验证整个租约组仍归当前持有者所有，再统一更新数据库截止时间。
        for c in credentials.iter() {
            validate(&tx, c, None)?;
        }
        let time = now();
        let instant = Instant::now();
        for c in credentials.iter() {
            tx.execute(
                "UPDATE operation_locks SET expires_at=?1 WHERE lock_id=?2 AND owner_token=?3",
                params![time + LEASE.as_millis() as i64, c.lock_id, c.token],
            )?;
        }
        tx.commit()?;
        for c in credentials {
            c.issued = instant;
            c.wall_issued = time;
            c.valid_until = instant + LEASE;
        }
        Ok(())
    }
    /// 核验并归还所给资源租约，同时记录释放失败诊断。
    /// 入参：credentials（&[Credential]）为本容器待释放的凭证集合，每项对应数据库租约或普通只读标记。
    /// 返回值：事务删除提交成功返回 Ok(())；空组或全只读标记无需访问数据库。
    /// 异常：任一持久凭证失效或数据库操作失败返回 VeilError。
    /// 注意事项：本方法不清空调用方的凭证副本，也不停止其他守卫续租；通常由 LeaseGuard 协调调用。
    pub fn release(&self, credentials: &[Credential]) -> Result<()> {
        let started = Instant::now();
        logging::remember_sensitive(self.password.as_str());
        let remaining = credentials
            .iter()
            .map(|c| c.valid_until.saturating_duration_since(Instant::now()))
            .min()
            .unwrap_or_default();
        let result = self.release_inner(credentials);
        let context = [
            ("database", Field::Path(&self.directory)),
            (
                "resource",
                Field::Resource(
                    credentials
                        .first()
                        .map(|c| c.resource_id.as_str())
                        .unwrap_or("none"),
                ),
            ),
            (
                "lock_id",
                Field::Resource(
                    credentials
                        .first()
                        .map(|c| c.lock_id.as_str())
                        .unwrap_or("none"),
                ),
            ),
            ("lock_count", Field::Count(credentials.len() as u64)),
            ("remaining_lease_ms", Field::Duration(remaining)),
            ("lease_ms", Field::Duration(LEASE)),
            ("renew_interval_ms", Field::Duration(RENEW_INTERVAL)),
            ("elapsed_ms", Field::Duration(started.elapsed())),
        ];
        if let Err(error) = &result {
            logging::failure(
                Level::Error,
                "locks",
                "release",
                "LEASE_RELEASE_FAILED",
                error,
                &context,
            );
        }
        result
    }
    /// 在一个即时写事务里先验证所有权，再删除整组租约行。
    /// 入参 credentials：待释放凭证。返回：删除提交成功时 Ok(())。异常：凭证失效或 SQLite 操作失败。
    /// 注意事项：credentials: &[Credential] 为空或全是普通只读时无操作成功；
    /// 持久租约验证与 DELETE 使用同一即时写事务，不能用过期旧凭证删除新持有者的记录。
    fn release_inner(&self, credentials: &[Credential]) -> Result<()> {
        if credentials.iter().all(Credential::is_read_only) {
            return Ok(());
        }
        let mut db = ContainerDb::open(&self.directory, &self.password, None)?;
        let tx = db
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // 防止旧凭证删除被重新分配的锁：先核验令牌、所有者和期限，再执行 DELETE。
        for c in credentials {
            validate(&tx, c, None)?;
        }
        for c in credentials {
            tx.execute(
                "DELETE FROM operation_locks WHERE lock_id=?1 AND owner_token=?2",
                params![c.lock_id, c.token],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    /// 原子地把整组租约转交给另一个执行者，并签发替代凭证。
    /// 入参 credentials：当前完整租约组；owner：新执行者身份；endpoint：新执行者的可选探测地址。
    /// 返回：新的凭证集合。异常：普通只读凭证、重复凭证、只交接关联租约的一部分、凭证过期或 SQL 失败。
    /// 注意：交接在一个短即时写事务内执行；原凭证成功后不应继续使用。
    pub fn handoff(
        &self,
        credentials: &[Credential],
        owner: ExecutionOwner,
        endpoint: Option<&ExecutionEndpoint>,
    ) -> Result<Vec<Credential>> {
        if credentials.iter().any(Credential::is_read_only) {
            return Err(invalid("普通读取不支持锁交接"));
        }
        let mut db = ContainerDb::open(&self.directory, &self.password, None)?;
        let tx = db
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for c in credentials {
            validate(&tx, c, None)?;
        }
        // 旧令牌 -> 请求中的租约数：逐组与数据库总数比较，禁止只移交同组中的部分资源。
        let mut groups = std::collections::HashMap::new();
        // 本次交接出现过的 lock_id 集合：每轮加入一张凭证，重复即拒绝整个交接。
        let mut identities = std::collections::HashSet::new();
        // 确保没有重复锁 ID，并按旧令牌统计关联租约数，下面要求每个令牌对应的锁全部一起移交。
        for c in credentials {
            if !identities.insert(&c.lock_id) {
                return Err(invalid("交接锁重复"));
            }
            *groups.entry(c.token.as_str()).or_insert(0i64) += 1;
        }
        // 数据库中同一令牌的总锁数必须与请求数相同，否则拒绝部分交接。
        for (token, count) in groups {
            let total: i64 = tx.query_row(
                "SELECT count(*) FROM operation_locks WHERE owner_token=?1",
                [token],
                |r| r.get(0),
            )?;
            if total != count {
                return Err(invalid("关联锁必须整体交接"));
            }
        }
        let time = now();
        let instant = Instant::now();
        let token = id();
        let mut replacement = Vec::new();
        // 为新持有者生成一枚令牌，并为每项复制一份更新后的所有权与到期信息。
        for c in credentials {
            let mut next = c.clone();
            next.token = token.clone();
            next.owner = owner.clone();
            next.issued = instant;
            next.wall_issued = time;
            next.valid_until = instant + LEASE;
            tx.execute("UPDATE operation_locks SET owner_identity=?1,owner_endpoint=?2,owner_token=?3,expires_at=?4 WHERE lock_id=?5 AND owner_token=?6",params![json(&owner)?,endpoint.map(json).transpose()?,next.token,time+LEASE.as_millis() as i64,c.lock_id,c.token])?;
            replacement.push(next);
        }
        tx.commit()?;
        Ok(replacement)
    }
}
/// 检查凭证模式编码和本地时间，区分需要查库的持久租约与普通读取标记。
/// 入参：c（&Credential）为凭证；mode（`Option<LockMode>`）为预期模式，None 不额外限制模式。
/// 返回值：普通只读且模式兼容返回 true；本地有效的持久租约返回 false，提醒调用方继续查库。
/// 异常：模式不兼容或持久租约本地到期/时钟偏差过大时返回参数类错误。
/// 注意事项：普通只读标记提前返回，不校验其本地到期时间；false 不是无效结论，尚需查库验证所有权及实际读写模式。
fn validate_local(c: &Credential, mode: Option<LockMode>) -> Result<bool> {
    if c.is_read_only() {
        return if mode.is_none() || mode == Some(LockMode::ReadOnly) {
            Ok(true)
        } else {
            Err(invalid("锁类型不匹配"))
        };
    }
    if let Some(mode) = mode {
        if (mode == LockMode::SharedWrite) != c.resource_id.starts_with(SHARED_WRITE_PREFIX)
            || (mode == LockMode::ReadOnly && !c.is_read_only())
            || (mode == LockMode::Write && c.is_read_only())
        {
            return Err(invalid("锁类型不匹配"));
        }
    }
    if !c.local_valid() {
        return Err(invalid("文件锁租约已失效，请重新申请"));
    }
    Ok(false)
}
/// 在实际数据库连接上确认凭证对应的租约仍存在、未过期且属于当前持有者。
/// 入参 conn：操作所在事务连接；c：租约凭证；mode：可选的预期模式。返回：凭证仍有效时成功。
/// 异常：凭证本地状态无效、数据库记录不存在 / 过期 / 所有者不符或查询失败时返回错误。
/// 注意：应在保护随后业务修改的同一事务内验证，避免校验后锁状态发生变化。
pub(crate) fn validate(conn: &Connection, c: &Credential, mode: Option<LockMode>) -> Result<()> {
    if validate_local(c, mode)? {
        return Ok(());
    }
    let valid:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM operation_locks WHERE lock_id=?1 AND resource_id=?2 AND owner_token=?3 AND owner_identity=?4 AND expires_at>?5 AND (?6 IS NULL OR mode=?6))",params![c.lock_id,c.resource_id,c.token,json(&c.owner)?,now(),mode.map(LockMode::as_str)],|r|r.get(0))?;
    if !valid {
        return Err(invalid("文件锁凭证已失效"));
    }
    Ok(())
}
/// 在同一事务快照内批量验证凭证，避免每个文件单独查询租约表。
/// 入参 conn：调用方已开启的事务连接；requested：凭证及可选的预期模式配对列表。返回：全部有效时成功。
/// 异常：任何凭证失效或批量 SQL 执行失败时返回错误。注意：SQL 分成 128 条一组以避开 SQLite 参数上限。
pub(crate) fn validate_many(
    conn: &Connection,
    requested: &[(&Credential, Option<LockMode>)],
) -> Result<()> {
    use rusqlite::types::Value;
    // 需查库的（凭证引用、预期模式）列表；名称 writable 包含共享读，并非只存写租约。
    let mut writable = Vec::new();
    // 普通只读标记无需查询租约表；其余凭证先通过本地检查后再汇总到 SQL 批次。
    for &(credential, mode) in requested {
        if !validate_local(credential, mode)? {
            writable.push((credential, mode));
        }
    }
    // 每次将最多 128 张凭证作为 VALUES 表与租约表左连接，发现缺失或过期行即整体失败。
    for batch in writable.chunks(128) {
        let rows = vec!["(?,?,?,?,?)"; batch.len()].join(",");
        // 按每张凭证五个字段的顺序绑定参数，末尾追加统一的数据库过期比较时刻。
        let mut values = Vec::new();
        for (credential, mode) in batch {
            values.extend([
                Value::Text(credential.lock_id.clone()),
                Value::Text(credential.resource_id.clone()),
                Value::Text(credential.token.clone()),
                Value::Text(json(&credential.owner)?),
                mode.map(|mode| Value::Text(mode.as_str().to_owned()))
                    .unwrap_or(Value::Null),
            ]);
        }
        values.push(Value::Integer(now()));
        let sql = format!(
            "WITH expected(lock_id,resource_id,token,owner,mode) AS (VALUES {rows})
            SELECT EXISTS(SELECT 1 FROM expected e LEFT JOIN operation_locks l
              ON l.lock_id=e.lock_id AND l.resource_id=e.resource_id AND l.owner_token=e.token
              AND l.owner_identity=e.owner AND (e.mode IS NULL OR l.mode=e.mode)
              WHERE l.lock_id IS NULL OR l.expires_at<=?)"
        );
        let invalid_credential: bool =
            conn.query_row(&sql, rusqlite::params_from_iter(values), |row| row.get(0))?;
        if invalid_credential {
            return Err(invalid("文件锁凭证已失效"));
        }
    }
    Ok(())
}

/// 在同一写事务内批量删除已验证的租约，避免每个凭证单独执行 DELETE。
/// 入参 conn：当前写事务连接；credentials：刚在同一事务中验证过的凭证。返回：删除数量与有效凭证数一致时成功。
/// 异常：删除数量不匹配表示凭证在释放过程中变化；SQL 错误会原样返回。
/// 注意：调用方必须先在同一事务内执行 validate_many；只读凭证没有数据库行，会被跳过。
pub(crate) fn release_validated_many(conn: &Connection, credentials: &[&Credential]) -> Result<()> {
    use rusqlite::types::Value;
    let mut unique = std::collections::HashSet::new();
    let credentials = credentials
        .iter()
        // 去掉只读凭证并按 lock_id 去重，避免对同一数据库行重复删除。
        .filter(|credential| {
            !credential.is_read_only() && unique.insert(credential.lock_id.as_str())
        })
        .collect::<Vec<_>>();
    // 将待释放凭证分批删除，控制单条 SQL 的绑定参数数量。
    for batch in credentials.chunks(128) {
        let rows = vec!["(?,?)"; batch.len()].join(",");
        let mut values = Vec::new();
        for credential in batch {
            values.extend([
                Value::Text(credential.lock_id.clone()),
                Value::Text(credential.token.clone()),
            ]);
        }
        let deleted = conn.execute(
            &format!("DELETE FROM operation_locks WHERE (lock_id,owner_token) IN (VALUES {rows})"),
            rusqlite::params_from_iter(values),
        )?;
        if deleted != batch.len() {
            return Err(invalid("文件锁释放期间凭证发生变化"));
        }
    }
    Ok(())
}

/// 在调用方已有事务中验证并释放单条租约。
/// 入参 conn：当前事务连接；c：待释放凭证。返回：验证并删除成功时 Ok(())。
/// 异常：凭证失效或 SQL 删除失败时返回错误。调用方负责提交事务。
/// 注意事项：conn: &Connection 应指向调用方的写事务；本方法不提交、不回滚，也不重新打开数据库。
pub(crate) fn release_in(conn: &Connection, c: &Credential) -> Result<()> {
    validate(conn, c, None)?;
    conn.execute(
        "DELETE FROM operation_locks WHERE lock_id=?1 AND owner_token=?2",
        params![c.lock_id, c.token],
    )?;
    Ok(())
}
/// 找出有效期尚未结束但其执行者已确认退出的锁，供申请事务安全回收。
/// 入参 conn：当前容器数据库连接；resources：可选资源筛选；deadline：总探测截止时间；
/// cancellation：可选取消标志。返回：已确认死亡的锁 ID 与所有者令牌配对列表。
/// 异常：执行服务不可用、记录损坏或 SQL 失败时返回错误。注意：250ms 是逐项探测的调度预算，
/// SQL 查询和系统调用可能另有耗时，不能据此保证整个函数在 250ms 内结束；
/// 无响应或探测预算耗尽都不会被误判为死亡；最终删除仍需在后续写事务中完成。
fn dead_locks_until(
    conn: &Connection,
    resources: Option<&[String]>,
    deadline: Option<Instant>,
    cancellation: Option<&AtomicBool>,
) -> Result<Vec<(String, String)>> {
    let execution = ExecutionService::global()?;
    let start = Instant::now();
    let budget_deadline = start + PROBE_BATCH_BUDGET;
    let deadline = deadline
        .map(|d| d.min(budget_deadline))
        .unwrap_or(budget_deadline);
    let mut stmt=conn.prepare("SELECT lock_id,owner_token,owner_identity,owner_endpoint FROM operation_locks WHERE expires_at>?1 AND (?2 IS NULL OR resource_id IN (SELECT value FROM json_each(?2)))")?;
    // 每项为（租约ID、旧令牌、执行身份JSON、可选端点JSON）；只收集尚未到期且命中精确资源键筛选的行。
    let records = stmt
        .query_map(params![now(), resources.map(json).transpose()?], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // 逐条探测尚未过期租约的执行者；只有明确返回 Dead 才可进入回收候选集合。
    let mut dead = Vec::new();
    for (identity, token, owner, endpoint) in records {
        if Instant::now() >= deadline || cancellation.is_some_and(|c| c.load(Ordering::Acquire)) {
            break;
        }
        let owner: ExecutionOwner = parse(&owner)?;
        let endpoint: Option<ExecutionEndpoint> = endpoint.map(|e| parse(&e)).transpose()?;
        let timeout =
            Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now()));
        // 活着或状态未知都保留锁，避免网络或调度延迟导致活动任务的租约被误删。
        if execution.check_owner(&owner, endpoint.as_ref(), timeout) == Liveness::Dead {
            dead.push((identity, token));
        }
    }
    if !dead.is_empty() {
        logging::event(
            Level::Warn,
            "locks",
            "owner_probe",
            "STALE_OWNER_DETECTED",
            "检测到已退出执行者遗留的租约，等待事务回收",
            &[
                ("dead_owner_lock_count", Field::Count(dead.len() as u64)),
                ("elapsed_ms", Field::Duration(start.elapsed())),
                ("outcome", Field::Label("eligible_for_recovery")),
            ],
        );
    }
    Ok(dead)
}
/// 在调用方事务中删除已经到期或已确认所有者死亡的租约行。
/// 入参 conn：执行申请的即时写事务；dead：探测得到的锁 ID / 所有者令牌对。返回：清理成功时 Ok(())。
/// 异常：SQL 删除失败时返回数据库错误。注意：必须通过令牌匹配，避免删掉随后重新申请的同一资源锁。
pub(crate) fn reap(conn: &Connection, dead: &[(String, String)]) -> Result<()> {
    conn.execute("DELETE FROM operation_locks WHERE expires_at<=?1", [now()])?;
    // 先清理自然过期租约，再只删除存活探测明确判死的对应所有者记录。
    for (identity, token) in dead {
        conn.execute(
            "DELETE FROM operation_locks WHERE lock_id=?1 AND owner_token=?2",
            params![identity, token],
        )?;
    }
    Ok(())
}
/// 把公开锁模式转成简短诊断标签。入参 mode：锁模式；返回：SR、RO、SW 或 EW。
/// 异常：穷尽匹配枚举，不访问数据库，不返回错误。
/// 注意事项：仅用于诊断，不能作为 operation_locks.mode 的落库值。
fn diagnostic_mode(mode: LockMode) -> &'static str {
    match mode {
        LockMode::Read => "SR",
        LockMode::ReadOnly => "RO",
        LockMode::SharedWrite => "SW",
        LockMode::Write => "EW",
    }
}
/// 判断 SQLite 错误是否属于可短暂重试的数据库忙或数据库已锁定。
/// 入参 e：SQLite 错误；返回：仅这两种并发占用状态返回 true。
/// 异常：仅检查 SQLite 错误码，不返回错误。
/// 注意事项：true 表示错误属于可暂时等待的类别，不保证重试会成功或业务操作可安全重放。
fn busy(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}

/// 自动续租守卫：维持一组租约有效，并在释放或析构时停止续租、归还凭证。
///
/// 守卫应随实际工作移动到阻塞闭包中；等待句柄被丢弃不会释放闭包内的租约。若容器目录即将删除，
/// 应调用 abandon，避免析构时再尝试打开已删除的数据库。
pub struct LeaseGuard {
    /// 用于租约续期、释放和错误记录的容器管理器。
    manager: LockManager,
    /// 续租线程和业务调用方共享的当前凭证集合。
    credentials: Arc<std::sync::Mutex<Vec<Credential>>>,
    /// 要求后台续租线程退出的原子标记。
    stop: Arc<AtomicBool>,
    /// 后台续租线程句柄，用于停止后等待其完全退出。
    worker: Option<JoinHandle<()>>,
}
impl LeaseGuard {
    /// 接管已提交的租约并启动定期续租线程。
    /// 入参：manager（LockManager）提供数据库访问和密码；credentials（`Vec<Credential>`）是本守卫负责的一组凭证。
    /// 返回值：成功返回 LeaseGuard，凭证通过 `Arc<Mutex<Vec<Credential>>>` 与续租线程共享。
    /// 异常：执行服务初始化或线程创建失败返回错误；此前已提交的租约不在此回滚，等待到期/后续回收。
    /// 注意事项：续租失败或共享锁中毒会停止后台循环；业务仍须在事务中验证凭证，不能因守卫尚存在就认定续租成功。
    fn start(manager: LockManager, credentials: Vec<Credential>) -> Result<Self> {
        let credentials = Arc::new(std::sync::Mutex::new(credentials));
        let stop = Arc::new(AtomicBool::new(false));
        let shared = credentials.clone();
        let stopped = stop.clone();
        let renewal = manager.clone();
        let worker = ExecutionService::global()?
            .spawn_thread("文件租约续租", move |_| {
                let mut next_renewal = Instant::now() + RENEW_INTERVAL;
                // 线程短暂休眠以避免忙等，到达续租时刻后在持有状态锁期间刷新整组凭证。
                while !stopped.load(Ordering::Acquire) {
                    std::thread::park_timeout(Duration::from_millis(100));
                    if Instant::now() >= next_renewal {
                        // 锁保护续租后的整组凭证，避免读到一半新、一半旧的有效期。
                        // 数据库重试期间也持有此锁，因此 credentials() 和释放/交接前的取快照可能等待。
                        if let Ok(mut c) = shared.lock() {
                            let deadline = c
                                .iter()
                                .map(|credential| credential.valid_until)
                                .min()
                                .unwrap_or_else(Instant::now);
                            match renewal.renew_with_retry(
                                &mut c,
                                veil_core::database::BusyRetryPolicy {
                                    max_retries: RENEW_MAX_RETRIES,
                                    deadline,
                                    retry_interval: RENEW_RETRY_INTERVAL,
                                },
                            ) {
                                Ok(()) => next_renewal = Instant::now() + RENEW_INTERVAL,
                                // 失败即停止自动续租，不擅自延长本地有效期；调用方后续验证会发现失效。
                                Err(_) => break,
                            }
                        } else {
                            logging::event(
                                Level::Error,
                                "locks",
                                "renew",
                                "LEASE_STATE_POISONED",
                                "续租线程无法访问租约状态，已停止续租",
                                &[("database", Field::Path(&renewal.directory))],
                            );
                            break;
                        }
                    }
                }
            })?
            .handle;
        Ok(Self {
            manager,
            credentials,
            stop,
            worker: Some(worker),
        })
    }
    /// 取出凭证快照，供同一租约范围内的事务验证使用。
    /// 返回：当前完整凭证列表的副本。异常：共享状态锁已中毒时返回租约状态错误。
    /// 入参：self: &LeaseGuard 为仍持有租约状态的守卫。
    /// 注意事项：返回 `Vec<Credential>` 是瞬时副本，后台续租或交接后会变化；使用前仍须验证，不得仅凭快照判定有效。
    pub fn credentials(&self) -> Result<Vec<Credential>> {
        self.credentials
            .lock()
            .map(|c| c.clone())
            .map_err(|_| invalid("租约状态不可用"))
    }
    /// 结束后台续租并显式归还守卫持有的资源。
    /// 入参：self（LeaseGuard）按值消费，调用后不能再用于业务。
    /// 返回值：释放事务成功返回 Ok(())，并清空本地凭证。
    /// 异常：取凭证失败、租约失效或数据库释放失败返回错误。
    /// 注意事项：先等待续租线程结束；返回错误时析构仍可能再次尝试释放，因此一次调用可能包含两次数据库释放尝试。
    pub fn release(mut self) -> Result<()> {
        self.stop_worker();
        let result = self.manager.release(&self.credentials()?);
        if result.is_ok() {
            self.released_in_transaction();
        }
        result
    }
    /// 清空已在外部事务中成功释放的凭证。
    /// 仅在释放租约的事务已确认提交后调用，避免析构重复释放并制造虚假异常。
    /// 入参：self: &LeaseGuard，外部事务已经确认释放其凭证。
    /// 返回值：无；能取得共享锁时清空本地凭证列表。
    /// 异常：共享锁中毒时静默跳过，不返回清空是否成功。
    /// 注意事项：不删除数据库行，不停止续租线程；禁止在数据库释放尚未提交时提前调用。
    pub(crate) fn released_in_transaction(&self) {
        if let Ok(mut owned) = self.credentials.lock() {
            owned.clear();
        }
    }
    /// 容器目录即将整体删除时，停止续租并丢弃凭据，不再尝试访问已删除的数据库。
    /// 入参：无；返回：消费守卫并结束后台线程。此操作不访问锁数据库，也不执行 DELETE。
    /// 异常：没有 Result 返回；共享凭证锁中毒时清空会跳过，析构仍可能尝试记录释放错误。
    /// 注意事项：该方法不删除数据库租约行，剩余记录需要依靠到期或执行者退出后的回收机制清理。
    pub(crate) fn abandon(mut self) {
        self.stop_worker();
        self.released_in_transaction();
    }
    /// 在容器改密提交后，用新密码归还原业务租约。
    /// 入参：self 按值消费；password（&str）为已生效的新密码，会复制到受零化包装的内部存储。
    /// 返回值：凭证释放成功为 Ok(())。
    /// 异常：凭证状态不可用、密码不匹配或数据库释放失败返回错误。
    /// 注意事项：须先确认改密事务已提交；停止续租后替换管理器密码，失败后的析构也使用新密码再次尝试释放。
    pub(crate) fn release_after_rekey(mut self, password: &str) -> Result<()> {
        self.stop_worker();
        let credentials = self.credentials()?;
        self.manager.password = Arc::new(Zeroizing::new(password.to_owned()));
        let result = self.manager.release(&credentials);
        if result.is_ok() {
            if let Ok(mut owned) = self.credentials.lock() {
                owned.clear();
            }
        }
        result
    }
    /// 停止旧守卫续租，将整组租约转给新执行者，并为新凭证启动守卫。
    /// 入参：self 按值消费；owner（ExecutionOwner）为新持有者；endpoint（`Option<&ExecutionEndpoint>`）为其可选可信本机探测端点。
    /// 返回值：交接提交且新续租线程启动成功后返回新 LeaseGuard。
    /// 异常：凭证无效、部分组交接、数据库失败或新线程创建失败返回错误。
    /// 注意事项：数据库交接成功后清空旧凭证；若随后启动失败，新所有权不会回滚，租约等待到期回收。事务失败时旧守卫析构会尝试释放旧组。
    pub fn handoff(
        mut self,
        owner: ExecutionOwner,
        endpoint: Option<&ExecutionEndpoint>,
    ) -> Result<Self> {
        self.stop_worker();
        let next = self
            .manager
            .handoff(&self.credentials()?, owner, endpoint)?;
        if let Ok(mut c) = self.credentials.lock() {
            c.clear();
        }
        Self::start(self.manager.clone(), next)
    }
    /// 通知续租线程停止、唤醒其休眠并等待线程退出；重复调用安全。
    /// 入参：self: &mut LeaseGuard 为独占访问的续租守卫。
    /// 返回值：无；有工作线程时取出句柄、请求停止并 join。
    /// 异常：后台线程 panic 只记录诊断日志，不向调用者重新抛出。
    /// 注意事项：可能阻塞至当前续租/数据库等待结束，不保证设置 stop 后立即返回。
    fn stop_worker(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            if worker.join().is_err() {
                logging::event(
                    Level::Error,
                    "locks",
                    "renew_worker",
                    "LEASE_WORKER_PANICKED",
                    "租约续租线程异常退出",
                    &[("database", Field::Path(&self.manager.directory))],
                );
            }
        }
    }
}
impl Drop for LeaseGuard {
    /// 析构时先停止续租，再尽力释放非空凭证；失败已由 release 记录且不能覆盖业务结果。
    /// 入参：self: &mut LeaseGuard 为即将离开作用域的租约守卫。
    /// 返回值：无；优先结束续租线程，再尝试归还当前非空租约组。
    /// 异常：租约释放失败只记日志；凭证锁中毒时跳过释放并记错，遗留记录等后续回收。
    /// 注意事项：析构可能等待线程和数据库操作，不是立即撤销；不能靠丢弃业务等待句柄释放仍在工作的租约。
    fn drop(&mut self) {
        self.stop_worker();
        if let Ok(c) = self.credentials.lock() {
            if !c.is_empty() {
                // release 已统一记录失败，析构不能覆盖原业务结果。
                let _ = self.manager.release(&c);
            }
        } else {
            logging::error("locks", "release", "LEASE_STATE_POISONED");
        }
    }
}
