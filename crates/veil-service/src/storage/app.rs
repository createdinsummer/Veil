//! 本机索引、设置与缓存，不允许写入容器密码或文件密钥。
//!
//! 本模块管理本机应用库 app.db（普通 SQLite，非加密）的全部业务访问，
//! 对应以下五张业务表：
//! - space_info：空间登记（稳定 ID、名称、本机绑定目录、共享/专属模式）；
//! - space_div：空间与容器登记（容器在本机的归属与展示名称缓存）；
//! - user_setting：用户偏好（提示级别、布局、免密时长、导入合并阈值等）；
//! - system_setting：本机系统设置（device_id、默认空间引用）；
//! - local_record：本机状态与缓存（最近访问、链接缓存、提示状态、容器状态）。
//! app.db 是展示与定位的索引，不是容器内容的权威来源；解锁后必须回容器
//! 库核对真实身份。本模块还负责两类「本机文件锁」：空间锁与容器生命周期
//! 锁，锁文件放在 app.db 同目录的独立子目录里，不随容器目录删除。
use super::{ensure_schema, id, invalid, json, locks::LockMode, now, parse};
use crate::error::Result;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fs::OpenOptions,
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// 免密时长的设置键：解锁成功后密码在本机内存中保留的秒数。
pub const UNLOCK_CACHE_SETTING: &str = "unlock_cache_seconds";
/// 未单独设置时，解锁成功后密码默认在本机内存中保留 1 小时。
pub const DEFAULT_UNLOCK_CACHE_SECONDS: u64 = 3600;
/// 免密时长上限：24 小时；0 表示关闭免密。
pub const MAX_UNLOCK_CACHE_SECONDS: u64 = 86_400;
/// 导入任务合并文件记录时的最长等待时间，单位为秒。
pub const IMPORT_COMMIT_INTERVAL_SETTING: &str = "import_commit_interval_seconds";
/// 导入合并间隔默认 1 秒。
pub const DEFAULT_IMPORT_COMMIT_INTERVAL_SECONDS: u64 = 1;
/// 导入合并间隔上限 60 秒。
pub const MAX_IMPORT_COMMIT_INTERVAL_SECONDS: u64 = 60;
/// 导入任务合并文件记录时的数量阈值（累积多少条触发一次提交）。
pub const IMPORT_COMMIT_COUNT_SETTING: &str = "import_commit_count";
/// 导入合并数量默认 8 条。
pub const DEFAULT_IMPORT_COMMIT_COUNT: u64 = 8;
/// 导入合并数量上限 100 条。
pub const MAX_IMPORT_COMMIT_COUNT: u64 = 100;
/// 按扩展名登记的默认打开应用：`{"扩展名": "应用 bundle identifier"}`。
/// 未登记的扩展名由客户端使用系统默认应用打开。
pub const EXTERNAL_OPEN_DEFAULTS_SETTING: &str = "external_open_defaults";
/// 打开方式登记表的上限：防止设置被塞进海量条目。
pub const MAX_EXTERNAL_OPEN_DEFAULTS: usize = 200;
/// 单个扩展名的最大长度（不含点号）。
pub const MAX_EXTENSION_LENGTH: usize = 16;
/// 应用 bundle identifier 的最大长度。
pub const MAX_BUNDLE_ID_LENGTH: usize = 255;

/// 平台注入的应用数据目录：移动端在初始化服务前设置，桌面端/CLI 走默认策略。
static PLATFORM_DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
/// 功能：在平台启动时指定本进程应用数据根，供移动端沙盒等无法使用默认目录的场景。
/// 入参：path: PathBuf 必须为绝对路径，保存后供后续 app_data_dir 使用。
/// 返回值：首次成功设置返回 Ok(())，同时尽力把日志目录切到此根下 logs。
/// 异常：相对路径或 OnceLock 已设置返回 InvalidFormat；日志切目录失败被忽略。
/// 注意事项：设置后不能用本入口更换目录，也不自动搬迁已打开数据库；应在服务初始化前调用。
/// 本函数不创建应用库，日志配置失败也不撤销已经成功保存的根路径。
pub fn configure_app_data_dir(path: PathBuf) -> Result<()> {
    if !path.is_absolute() {
        return Err(invalid("应用目录必须是绝对路径"));
    }
    PLATFORM_DIRECTORY
        .set(path.clone())
        .map_err(|_| invalid("应用目录已初始化"))?;
    // 日志不可用不能阻止应用目录初始化；日志后端通过 health 暴露失败。
    let _ = veil_core::logging::set_directory(path.join("logs"));
    Ok(())
}
/// 功能：按平台和测试约定定位当前应用库、默认空间及生命周期锁的根目录。
/// 入参：无，优先平台注入，其次 VEIL_APP_DATA_DIR，最后测试专用目录或系统本地数据目录下 Veil。
/// 返回值：Ok(PathBuf)，平台/环境分支要求绝对路径；单元测试默认进程内共享一个临时根。
/// 异常：环境注入为相对路径或无法定位系统数据目录返回 InvalidFormat；测试默认临时根创建失败会 panic。
/// 注意事项：只定位，不创建 app.db 或默认空间；测试根通过静态守卫持有，不能据每次调用推断是新的隔离目录。
pub fn app_data_dir() -> Result<PathBuf> {
    // 移动端：使用平台适配层在启动时注入的私有目录。
    if let Some(path) = PLATFORM_DIRECTORY.get() {
        return Ok(path.clone());
    }
    // 显式注入的环境变量：供测试与部署场景使用。
    if let Some(path) = std::env::var_os("VEIL_APP_DATA_DIR") {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(invalid("VEIL_APP_DATA_DIR 必须是绝对路径"));
        }
        return Ok(path);
    }
    // 测试：每个进程一个临时目录，避免测试互相污染。
    #[cfg(test)]
    {
        static TEST_DIRECTORY: OnceLock<tempfile::TempDir> = OnceLock::new();
        return Ok(TEST_DIRECTORY
            .get_or_init(|| tempfile::TempDir::new().expect("测试目录"))
            .path()
            .into());
    }
    // 桌面端与 CLI：系统数据目录下的 Veil 子目录（两者必须一致）。
    #[cfg(not(test))]
    dirs::data_local_dir()
        .map(|p| p.join("Veil"))
        .ok_or_else(|| invalid("无法定位应用数据目录"))
}

/// 空间：本机登记的空间信息（对应 space_info 表）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Space {
    /// 空间稳定 ID（UUID）：改名或换目录都不变。
    pub id: String,
    /// 空间展示名称。
    pub name: String,
    /// 空间说明（可空）。
    pub description: Option<String>,
    /// 本设备绑定的空间文件夹位置（可空：未绑定本机目录的空间）。
    pub local_path: Option<PathBuf>,
    /// 存储模式：shared 可放多个容器；dedicated 只允许一个容器。
    pub storage_mode: String,
    /// 空间在列表中的排列顺序。
    pub sort_order: i64,
    /// 空间创建时间（UTC 毫秒）。
    pub created_at: i64,
    /// 最后修改时间（UTC 毫秒）。
    pub updated_at: i64,
}
/// 空间与容器登记（对应 space_div 表）：容器在本机属于哪个空间。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registration {
    /// 容器 ID（对应容器库 div_info.veil_id），跨库逻辑引用。
    pub div_id: String,
    /// 所属空间 ID（外键引用 space_info.id）。
    pub space_id: String,
    /// 容器展示名称缓存：未解锁时也用于展示与定位。
    pub cached_name: String,
    /// 空间内的排列顺序。
    pub sort_order: i64,
    /// 本机登记时间（UTC 毫秒）。
    pub registered_at: i64,
    /// 登记记录修改时间（UTC 毫秒）。
    pub updated_at: i64,
}
/// 最近访问的容器（local_record/recent_div 的 JSON 内容）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecentContainer {
    /// 容器 ID。
    pub id: String,
    /// 容器展示名称。
    pub name: String,
    /// 容器目录位置（缓存用途，不是权威来源）。
    pub path: String,
}

/// 仅识别 iOS 应用容器下由本应用生成的默认空间路径。
///
/// iOS 更新/还原应用时，沙盒容器目录的 UUID 会变化，导致登记的旧路径
/// 失效。本函数把旧空间路径换成新应用容器下同结构的路径；只接受自己
/// 应用默认空间目录的形状，其他形状一律返回 None（不迁移）。
#[cfg(any(target_os = "ios", test))]
/// 功能：在 iOS 沙盒 UUID 改变后，为本应用默认布局的空间推导新位置。
/// 入参：old: &Path 为旧空间目录；current_root: &Path 为当前 Veil 应用根；space_id: &str 为规范小写 UUID v4。
/// 返回值：旧/新路径均符合已知沙盒布局且沙盒 UUID 不同时返回 Some(PathBuf)，目标为 current_root/spaces/space_id；其他情况 None。
/// 异常：不访问文件系统、不返回 Result，路径结构、UUID 或 UTF-8 检查失败均用 None 表示。
/// 注意事项：只推导候选，不证明路径存在或内容属于此空间；实际存在性检查和数据库重绑由 rebind_ios_sandbox_spaces 负责。
fn ios_relocated_space_path(old: &Path, current_root: &Path, space_id: &str) -> Option<PathBuf> {
    // 空间 ID 必须是 UUID，避免把任意路径重绑。
    if !veil_core::metadata::is_uuid_v4(space_id) {
        return None;
    }
    // 从 iOS 应用容器路径中提取应用沙盒 UUID；路径形状不符返回 None。
    /// 功能：识别 iOS 默认应用数据路径中的沙盒实例 ID，供空间迁移时安全重绑。
    /// 入参：path 为待识别完整路径；suffix 为 UUID 后必须精确匹配的业务目录后缀。
    /// 返回值：路径前缀、UUID v4 与后缀均匹配时返回小写 UUID；否则 None。
    /// 异常：不访问文件系统、不返回 I/O 错误；非 UTF-8 UUID 段返回 None。
    /// 注意事项：只验证路径形状，不能据此证明目录存在或属于某个已安装应用。
    fn container_id(path: &Path, suffix: &Path) -> Option<String> {
        let app_root = path
            .strip_prefix("/private/var/mobile/Containers/Data/Application")
            .or_else(|_| path.strip_prefix("/var/mobile/Containers/Data/Application"))
            .ok()?;
        let mut parts = app_root.components();
        // 第一段必须是应用沙盒 UUID。
        let uuid = parts.next()?.as_os_str().to_str()?;
        // UUID 后的剩余部分必须精确等于预期后缀。
        if !veil_core::metadata::is_uuid_v4(&uuid.to_ascii_lowercase()) || parts.as_path() != suffix
        {
            return None;
        }
        Some(uuid.to_ascii_lowercase())
    }
    // 应用目录内 Veil 数据根目录的相对形状。
    let app_suffix = Path::new("Library/Application Support/Veil");
    // 旧空间路径必须形如 <应用容器>/Library/Application Support/Veil/spaces/<id>。
    let old_suffix = app_suffix.join("spaces").join(space_id);
    let old_id = container_id(old, &old_suffix)?;
    // 当前根目录必须形如 <应用容器>/Library/Application Support/Veil。
    let current_id = container_id(current_root, app_suffix)?;
    // 新旧沙盒 UUID 相同说明没有迁移发生，不需要重绑。
    if old_id == current_id {
        return None;
    }
    // 用新沙盒 UUID 换掉旧前缀，得到重绑后的空间路径。
    Some(current_root.join("spaces").join(space_id))
}

/// 本机应用库访问对象：conn 是池化普通 SQLite 连接，directory 记录 app.db 与本地锁文件所在根。
/// 用于空间登记、用户/系统设置、本机缓存和生命周期协调；应用库只保存索引与恢复线索，不保存解锁秘密。
pub struct AppStore {
    /// 打开 app.db 的数据库连接（普通 SQLite，无密码）。
    pub(crate) conn: veil_core::database::Connection,
    /// app.db 所在目录（也是空间锁、容器锁等锁文件目录的根）。
    pub directory: PathBuf,
}
impl AppStore {
    /// 功能：为一次本机索引、偏好或缓存操作取得当前应用库访问句柄。
    /// 入参：无，根位置由 app_data_dir 确定。
    /// 返回值：Ok(AppStore)，使用定位到的根打开或初始化 app.db。
    /// 异常：目录定位、文件系统、连接或结构/设备身份检查失败传播 VeilError。
    /// 注意事项：不是全局单例句柄，每次调用新建访问包装，底层连接可池化复用；首次打开会产生初始化写入。
    pub fn global() -> Result<Self> {
        Self::open(&app_data_dir()?)
    }
    /// 功能：建立本机普通 SQLite 业务库访问，确认受支持结构并确保本设备身份可用。
    /// 入参：directory: &Path 为 app.db 的父目录，允许缺失；这里不会把所有传入路径强制绝对化。
    /// 返回值：Ok(AppStore)，含池化连接及传入目录副本，完成结构版本 1 和 device_id 初始化/校验；iOS 还尝试沙盒空间重绑。
    /// 异常：目录、连接、结构、JSON 或设备身份失败返回 VeilError；新 UUID 依赖底层随机源，失败可能 panic。
    /// 注意事项：允许新建，但已有未知结构不接管；首次空结构仅短轮询，最终仍交 ensure_schema 校验。
    /// 设备身份的检查写入使用独立即时事务，不与先前建表处在同一个事务；失败可能已留下目录或已初始化结构，不能按 Err 推断毫无持久化副作用。
    pub fn open(directory: &Path) -> Result<Self> {
        crate::fsutil::create_dir_all_durable(directory)?;
        // app.db 文件名与位置是首期实现约定，由 core 统一命名。
        let path = directory.join(veil_core::database::APP_DB);
        // 判断库文件是否由本次调用新建（决定是否允许执行建表）。
        let create = !path.try_exists()?;
        let mut conn = veil_core::database::open(&path, None, true)?;
        // 另一个首次启动进程可能已经创建文件、尚未提交结构；只等待空结构，绝不接管未知库。
        if !create {
            let start = std::time::Instant::now();
            loop {
                // 轮询版本与表数量：对方进程可能正在建表。
                let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
                let tables: i64 = conn.query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table'",
                    [],
                    |r| r.get(0),
                )?;
                // 结构已就绪、已有表、或等待超时：停止等待，交给下方校验。
                if version != 0
                    || tables != 0
                    || start.elapsed() >= veil_core::database::BUSY_TIMEOUT
                {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        // 建表或校验结构（app 库类型标识 0x56415050，结构版本 1）。
        ensure_schema(&mut conn, 0x56415050, include_str!("app.sql"), 1, create)?;
        if create {
            // 首次写入可能改变文件标识，重新打开后校验数据库结构。
            drop(conn);
            conn = veil_core::database::open(&path, None, false)?;
            ensure_schema(&mut conn, 0x56415050, include_str!("app.sql"), 1, false)?;
        }
        let mut store = Self {
            conn,
            directory: directory.to_path_buf(),
        };
        // 确保本机设备身份存在：已存在则校验格式，不存在则生成 UUID 写入。
        // 在独立即时写事务中重查并保存设备身份，避免并发首次启动写出两个值；结构检查此前已单独完成。
        let tx = store
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let device: Option<String> = tx
            .query_row(
                "SELECT value_json FROM system_setting WHERE key='device_id'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(value) = device {
            // 已有设备身份：必须是合法 UUID，否则视为数据异常。
            let value: String = parse(&value)?;
            if !veil_core::metadata::is_uuid_v4(&value) {
                return Err(invalid("设备身份为空"));
            }
        } else {
            // 首次启动：写入一个新的设备身份（进程重启后保持不变）。
            tx.execute(
                "INSERT INTO system_setting VALUES ('device_id',?1,?2)",
                params![json(&id())?, now()],
            )?;
        }
        tx.commit()?;
        // iOS 特有：应用沙盒 UUID 变化时重绑空间路径（见 ios_relocated_space_path）。
        #[cfg(target_os = "ios")]
        store.rebind_ios_sandbox_spaces()?;
        Ok(store)
    }

    /// 功能：保护空间内普通创建、导入等使用过程，与空间整体删除或移动的排他操作协调。
    /// 入参：self: &AppStore；identity: &str 为已登记且绑定本机目录的空间 ID。
    /// 返回值：Ok(`Vec<File>`)，包含该空间及当前登记中路径重叠空间的已加共享锁句柄。
    /// 异常：空间不存在/未绑定、锁文件打开或加锁失败传播 VeilError。
    /// 注意事项：共享锁允许多个正常使用者共存，不等同于容器内容只读；可能阻塞且没有本入口超时，持有整组句柄到业务结束。
    pub fn acquire_space_shared_lock(&self, identity: &str) -> Result<Vec<std::fs::File>> {
        self.acquire_space_lock(identity, LockMode::SharedWrite)
    }

    /// 功能：对空间及其已知父子空间取得排他使用权，防止目录级删除/移动与内部使用冲突。
    /// 入参：self: &AppStore；identity: &str 为目标空间 ID。
    /// 返回值：Ok(`Vec<File>`)，每项对应一个重叠空间的排他文件锁，按空间 ID 顺序取得。
    /// 异常：目标无效、未绑定、锁文件或系统加锁失败传播 VeilError。
    /// 注意事项：遇占用会阻塞，不使用容器租约 LockOptions 的期限/取消；只覆盖当前空间快照，新增或外部不遵守协议的操作仍需上层协调。
    pub fn acquire_space_exclusive_lock(&self, identity: &str) -> Result<Vec<std::fs::File>> {
        self.acquire_space_lock(identity, LockMode::Write)
    }

    /// 功能：将一个空间操作扩展为所有已知路径重叠空间的锁组，协调父子目录间的生命周期操作。
    /// 入参：self: &AppStore；identity: &str 为目标空间 ID；mode: LockMode 只接受 SharedWrite（系统共享锁）或 Write（系统排他锁）。
    /// 返回值：Ok(`Vec<File>`) 为按去重排序 ID 取得的全部锁句柄，关闭句柄释放对应锁。
    /// 异常：不支持的模式、空间/路径缺失、打开锁文件或系统加锁失败返回 VeilError；部分取得后失败由局部句柄析构释放前面的锁。
    /// 注意事项：按路径组成部分判断祖先/后代重叠，不在这里重新规范化每条路径；稳定顺序减少遵守同协议者的反向拿锁风险。
    /// 单文件争用转阻塞等待，没有总时限或取消，不能据排序宣称对任意其他外部锁顺序都绝不死锁。
    fn acquire_space_lock(&self, identity: &str, mode: LockMode) -> Result<Vec<std::fs::File>> {
        let exclusive = match mode {
            // 此处 SharedWrite 表示多个普通空间使用者可共存，映射为系统共享文件锁；不是容器内容只读语义。
            LockMode::SharedWrite => false,
            LockMode::Write => true,
            // 空间锁只支持写侧语义：普通读不改变空间结构，无需加锁。
            LockMode::Read | LockMode::ReadOnly => {
                return Err(invalid("空间锁仅允许共享写锁或排他写锁"));
            }
        };
        // 从空间快照找到目标，要求已经保存本机目录绑定；此处不实际探测目录是否仍存在或可访问。
        let spaces = self.spaces()?;
        let target = spaces
            .iter()
            .find(|space| space.id == identity)
            .ok_or_else(|| invalid("空间不存在"))?;
        let target_path = target
            .local_path
            .as_deref()
            .ok_or_else(|| invalid("空间目录不可用"))?;
        // 找出所有目录路径与目标重叠的空间：防止「父空间删目录时子空间
        // 正在里面写容器」这类跨空间竞争。
        let mut overlapping = spaces
            .iter()
            .filter(|space| {
                space.local_path.as_deref().is_some_and(|path| {
                    path.starts_with(target_path) || target_path.starts_with(path)
                })
            })
            .map(|space| space.id.as_str())
            .collect::<Vec<_>>();
        // 稳定排序＋去重：并发方也按同样顺序加锁，避免死锁。
        overlapping.sort_unstable();
        overlapping.dedup();

        // 逐空间打开锁文件并加锁；文件句柄必须持有到操作结束（RAII 释放）。
        // files 保存已取得锁的句柄，每项对应一个重叠空间；组内全部成功才转交调用方，
        // 中途失败返回时局部 Vec 析构，自动关闭先前句柄以释放已取得锁。
        let mut files = Vec::with_capacity(overlapping.len());
        for identity in overlapping {
            let file = self.open_space_lock_file(identity).map_err(|error| {
                veil_core::logging::failure(
                    veil_core::logging::Level::Error,
                    "storage",
                    "space_lock",
                    "SPACE_LOCK_OPEN_FAILED",
                    &error,
                    &[
                        ("resource", veil_core::logging::Field::Resource(identity)),
                        (
                            "mode",
                            veil_core::logging::Field::Label(if exclusive {
                                "exclusive"
                            } else {
                                "shared"
                            }),
                        ),
                        ("phase", veil_core::logging::Field::Label("open_lock_file")),
                    ],
                );
                error
            })?;
            Self::lock_space_file(&file, identity, exclusive)?;
            files.push(file);
        }
        Ok(files)
    }

    /// 功能：给一份空间资源文件加系统锁，并记录发生等待或最终失败的诊断信息。
    /// 入参：file: &File 为已打开锁文件；identity: &str 只用于日志资源指纹；exclusive: bool 选择排他或共享模式。
    /// 返回值：加锁成功后 Ok(())，锁继续由传入句柄持有。
    /// 异常：try_lock 的实际系统错误或阻塞加锁失败转换为 VeilError::Io。
    /// 注意事项：先非阻塞尝试，WouldBlock 后发等待日志并调用阻塞 lock/lock_shared；日志不会给业务新增超时。
    /// 成功后不在本函数解锁，调用方必须保持句柄到受保护操作结束，也应遵守锁组的稳定取得顺序。
    fn lock_space_file(file: &std::fs::File, identity: &str, exclusive: bool) -> Result<()> {
        use veil_core::logging::{self, Field, Level};
        let mode = if exclusive { "exclusive" } else { "shared" };
        // 先非阻塞尝试：空闲时零等待直接拿到。
        let initial = if exclusive {
            file.try_lock()
        } else {
            file.try_lock_shared()
        };
        let (result, waiting) = match initial {
            // 一次成功：记录等锁起点，直接返回。
            Ok(()) => return Ok(()),
            // 加锁本身的错误（非「被占用」）：直接按失败处理。
            Err(std::fs::TryLockError::Error(error)) => (Err(error), None),
            // 被其他操作持有：转入阻塞等待（记录起点用于超时诊断日志）。
            Err(std::fs::TryLockError::WouldBlock) => {
                let started = std::time::Instant::now();
                logging::event(
                    Level::Warn,
                    "storage",
                    "space_lock",
                    "SPACE_LOCK_WAITING",
                    "空间锁被其他操作持有，开始等待",
                    &[
                        ("resource", Field::Resource(identity)),
                        ("mode", Field::Label(mode)),
                        ("phase", Field::Label("acquire_os_lock")),
                        ("outcome", Field::Label("waiting")),
                    ],
                );
                // 保留原协议的阻塞等待和锁顺序；日志不引入业务超时。
                (
                    if exclusive {
                        file.lock()
                    } else {
                        file.lock_shared()
                    },
                    Some(started),
                )
            }
        };
        // 结果诊断：失败记失败事件；等待超过 1 秒的成功记「已恢复」事件。
        if let Err(error) = &result {
            logging::failure(
                Level::Error,
                "storage",
                "space_lock",
                "SPACE_LOCK_ACQUIRE_FAILED",
                error,
                &[
                    ("resource", Field::Resource(identity)),
                    ("mode", Field::Label(mode)),
                    ("phase", Field::Label("acquire_os_lock")),
                    ("outcome", Field::Label("not_acquired")),
                ],
            );
        } else if let Some(started) = waiting {
            if started.elapsed() >= std::time::Duration::from_secs(1) {
                logging::event(
                    Level::Warn,
                    "storage",
                    "space_lock",
                    "SPACE_LOCK_RECOVERED",
                    "空间锁等待结束，已取得锁",
                    &[
                        ("resource", Field::Resource(identity)),
                        ("mode", Field::Label(mode)),
                        ("elapsed_ms", Field::Duration(started.elapsed())),
                        ("outcome", Field::Label("acquired")),
                    ],
                );
            }
        }
        result.map_err(Into::into)
    }

    /// 功能：声明本地容器正被会话、传输或登记使用，防止同协议删除流程并发清掉目录。
    /// 入参：self: &AppStore；identity: &str 为安全单段容器身份。
    /// 返回值：Ok(File)，为 container-locks 命名空间中已加共享锁的句柄。
    /// 异常：ID 校验、建锁目录、打开/权限或共享加锁失败传播 VeilError。
    /// 注意事项：可能无限阻塞等待其他排他持有者；不要求容器密码，不确认目录已存在，也不是内容租约。
    /// 锁文件放在应用根，容器目录删除不应删除仍被持有的这份锁载体。
    pub(crate) fn acquire_container_shared_lock(&self, identity: &str) -> Result<std::fs::File> {
        crate::work::validate_veil_id(identity)?;
        let file = self.open_resource_lock_file("container-locks", identity)?;
        // 阻塞等待共享锁；删除拿排他锁时这里会等待其完成。
        file.lock_shared()?;
        Ok(file)
    }

    /// 功能：删除前尝试取得容器的本地排他生命周期权限，容器仍在使用时立即拒绝。
    /// 入参：self: &AppStore；identity: &str 为安全单段容器身份。
    /// 返回值：try_lock 成功时 Ok(File)，持有期间遵守协议的共享使用者不能新取得锁。
    /// 异常：争用返回 InvalidFormat“容器正在使用”；校验、打开或真正系统加锁失败传播对应 VeilError。
    /// 注意事项：争用可能来自共享使用者或另一排他操作者；不会排队等待、不验证密码或自动删除容器，实际删除由调用方完成。
    pub(crate) fn acquire_container_delete_lock(&self, identity: &str) -> Result<std::fs::File> {
        crate::work::validate_veil_id(identity)?;
        let file = self.open_resource_lock_file("container-locks", identity)?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            // 被共享持有者占用：容器正在使用，直接拒绝删除，不阻塞同空间其他容器。
            Err(std::fs::TryLockError::WouldBlock) => Err(invalid("容器正在使用，不能删除")),
            Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }

    /// 功能：定位空间专属的本地锁载体，为后续共享/排他加锁准备句柄。
    /// 入参：self: &AppStore；identity: &str 为非空空间 ID。
    /// 返回值：Ok(File)，来自 space-locks 命名空间，尚未加业务文件锁。
    /// 异常：空身份返回 InvalidFormat，目录/打开/权限失败传播 VeilError。
    /// 注意事项：只要求非空，ID 文件名通过哈希生成；不能把“文件已打开”当作已经取得空间使用权。
    fn open_space_lock_file(&self, identity: &str) -> Result<std::fs::File> {
        if identity.is_empty() {
            return Err(invalid("空间锁 ID 不能为空"));
        }
        self.open_resource_lock_file("space-locks", identity)
    }

    /// 功能：为本机生命周期协议创建或复用一份持久锁文件，避免直接用敏感身份作文件名。
    /// 入参：self: &AppStore 提供应用根；namespace: &str 为内部可信目录名，如 space-locks/container-locks；identity: &str 为要散列的资源身份。
    /// 返回值：Ok(File)，路径为应用根/命名空间/BLAKE3身份摘要.lock，以读写、可创建、不截断模式打开。
    /// 异常：建目录、设置权限或打开文件失败传播 I/O 错误。
    /// 注意事项：本方法不加锁，不验证 namespace 的路径边界；只应由内部固定命名空间调用。
    /// Unix 在建/打开后收紧为目录 0700、文件 0600，没有 O_NOFOLLOW 或创建瞬间私有模式保证，不能据哈希命名推断整个路径已安全校验。
    fn open_resource_lock_file(&self, namespace: &str, identity: &str) -> Result<std::fs::File> {
        // namespace 由本模块固定调用点选择；身份只进入散列文件名，不作为目录段拼接。
        let lock_directory = self.directory.join(namespace);
        std::fs::create_dir_all(&lock_directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&lock_directory, std::fs::Permissions::from_mode(0o700))?;
        }
        // 身份散列后得到固定无分隔符文件名；不直接显示原文，但确定性哈希不等于不可猜测的匿名标识。
        let lock_path = lock_directory.join(format!("{}.lock", blake3::hash(identity.as_bytes())));
        // 锁载体允许复用，不截断既有文件；这里仅打开，后续调用者才取得共享或排他文件锁。
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(file)
    }

    /// iOS 更新或还原应用时，私有数据目录的系统标识可能发生变化。
    /// 只重新绑定应用默认空间，并且必须在新位置找到原有空间目录。
    ///
    /// 重绑规则：旧路径不再存在、新路径存在且同结构 → 更新 space_info 的
    /// local_path，并把最近访问记录里的路径前缀一并替换。整个更新在单个
    /// 事务内完成。
    #[cfg(target_os = "ios")]
    /// 功能：iOS 应用沙盒根变化后，把本应用默认布局空间及最近访问缓存重绑到新根。
    /// 入参：self: &mut AppStore，提供当前应用根及空间列表。
    /// 返回值：所有候选处理和即时事务提交成功后 Ok(())；无候选或旧位置仍可用时不改相应空间。
    /// 异常：根/候选规范化、空间查询、SQL 或事务提交失败传播 VeilError。
    /// 注意事项：只处理路径结构符合规则且旧目录非目录、新目录为目录的候选；不搬运实际容器文件。
    /// 空间列表在开事务前读取，更新以旧 local_path 条件保护；最近记录路径按旧前缀替换，与该次事务一起提交。
    fn rebind_ios_sandbox_spaces(&mut self) -> Result<()> {
        let root = std::fs::canonicalize(&self.directory)?;
        let spaces = self.spaces()?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // 逐空间检查是否需要重绑。
        for space in spaces {
            let Some(old) = space.local_path else {
                continue;
            };
            // 计算新位置；非本应用默认空间形状返回 None（跳过）。
            let Some(new) = ios_relocated_space_path(&old, &root, &space.id) else {
                continue;
            };
            // 旧路径还存在或新路径不存在都不迁移，防止覆盖真实数据。
            if old.is_dir() || !new.is_dir() {
                continue;
            }
            let new = std::fs::canonicalize(new)?;
            // 条件更新：只有当前登记的仍是旧路径才改写，避免覆盖其他变更。
            tx.execute(
                "UPDATE space_info SET local_path=?1,updated_at=?2 WHERE id=?3 AND local_path=?4",
                params![
                    new.to_string_lossy().as_ref(),
                    now(),
                    space.id,
                    old.to_string_lossy().as_ref()
                ],
            )?;
            // 最近访问记录只是缓存；路径随空间重新绑定一同更新。
            let old_prefix = format!("{}/", old.display());
            let new_prefix = format!("{}/", new.display());
            tx.execute("UPDATE local_record SET value_json=json_set(value_json,'$.path',?1 || substr(json_extract(value_json,'$.path'), length(?2)+1)),updated_at=?3 WHERE record_type='recent_div' AND substr(json_extract(value_json,'$.path'),1,length(?2))=?2", params![new_prefix, old_prefix, now()])?;
        }
        tx.commit()?;
        Ok(())
    }
    /// 功能：读取本机持久设备身份，供本机空间或诊断关联使用。
    /// 入参：self: &AppStore。
    /// 返回值：Ok(String)，为 system_setting/device_id 记录反序列化后的文本。
    /// 异常：缺失返回 InvalidFormat，数据库/JSON 类型错误传播 VeilError。
    /// 注意事项：格式检查在 open 初始化阶段执行，此方法本身只读取字符串；设备身份不是容器口令或文件密钥。
    pub fn device_id(&self) -> Result<String> {
        self.system("device_id")?
            .ok_or_else(|| invalid("设备身份缺失"))
    }
    /// 功能：列出本机全部逻辑空间，供选择、展示和路径重叠锁范围计算。
    /// 入参：self: &AppStore；没有筛选参数。
    /// 返回值：Ok(`Vec<Space>`)，每行 space_info 转一条对象，按 sort_order、id 升序，允许空列表或 local_path=None。
    /// 异常：SQL 准备、逐行字段类型转换或查询失败传播 VeilError。
    /// 注意事项：不扫描磁盘或验证各绑定目录仍可用；local_path 从保存文本构造，不在这里 canonicalize。
    pub fn spaces(&self) -> Result<Vec<Space>> {
        let mut stmt=self.conn.prepare("SELECT id,name,description,local_path,storage_mode,sort_order,created_at,updated_at FROM space_info ORDER BY sort_order,id")?;
        Ok(stmt
            .query_map([], |r| {
                Ok(Space {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    description: r.get(2)?,
                    local_path: r.get::<_, Option<String>>(3)?.map(PathBuf::from),
                    storage_mode: r.get(4)?,
                    sort_order: r.get(5)?,
                    created_at: r.get(6)?,
                    updated_at: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
    /// 功能：为一个目录登记新的逻辑空间，保存共享/专属模式供后续容器登记约束使用。
    /// 入参：self: &mut AppStore；name: &str 为 trim 后非空的名称；path: `Option<&Path>` 为目录，None 采用应用根/spaces/新UUID；dedicated: bool 决定模式。
    /// 返回值：Ok(Space)，含新随机 ID、规范化路径、原样名称、空说明、排序 0 及当前毫秒时间。
    /// 异常：空名称、路径非 UTF-8、目录/SQL/唯一目录约束失败返回 VeilError；随机 ID 获取依赖底层随机源。
    /// 注意事项：先建目录再写单条登记，写失败不会自动删除目录；同名空间可以存在，local_path 由数据库唯一约束去重。
    /// 不把该空间设为默认，不检查专属目录已有多少实际未登记容器。
    pub fn create_space(
        &mut self,
        name: &str,
        path: Option<&Path>,
        dedicated: bool,
    ) -> Result<Space> {
        if name.trim().is_empty() {
            return Err(invalid("空间名称不能为空"));
        }
        // 生成空间稳定 ID，后续改名/换目录都不变。
        let identity = id();
        // 未指定目录时使用 <应用数据目录>/spaces/<空间 ID> 的约定布局。
        let path = path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.directory.join("spaces").join(&identity));
        // 先创建目录再规范化，统一 /var 与 /private/var 等别名。
        crate::fsutil::create_dir_all_durable(&path)?;
        let path = std::fs::canonicalize(path)?;
        let time = now();
        // 新增一条逻辑空间：说明 NULL、排序 0，规范化目录唯一；创建与更新时间相同，
        // dedicated/shared 存储模式供随后 register 的容器数量约束使用，不在此扫描磁盘容器数。
        self.conn.execute(
            "INSERT INTO space_info VALUES (?1,?2,NULL,?3,?4,0,?5,?5)",
            params![
                identity,
                name,
                path.to_str().ok_or_else(|| invalid("路径不是 UTF-8"))?,
                if dedicated { "dedicated" } else { "shared" },
                time
            ],
        )?;
        Ok(Space {
            id: identity,
            name: name.into(),
            description: None,
            local_path: Some(path),
            storage_mode: if dedicated {
                "dedicated".into()
            } else {
                "shared".into()
            },
            sort_order: 0,
            created_at: time,
            updated_at: time,
        })
    }
    /// 功能：修改空间展示名称并记录更新时间，保持身份及容器归属不变。
    /// 入参：self: &AppStore；identity: &str 为空间 ID；name: &str 为 trim 后非空新名称，原文本保存。
    /// 返回值：恰好更新一条时 Ok(())。
    /// 异常：空名称或影响行数不为 1 返回 InvalidFormat，SQL 失败传播数据库错误。
    /// 注意事项：不要求名称全库唯一，不移动磁盘目录或刷新其他调用方已加载的 WorkConfig。
    pub fn rename_space(&self, identity: &str, name: &str) -> Result<()> {
        if name.trim().is_empty() {
            return Err(invalid("空间名称不能为空"));
        }
        // 按稳定 ID 只改展示名称和更新时间，路径与容器归属保持原样；更新零行说明目标已不存在。
        if self.conn.execute(
            "UPDATE space_info SET name=?1,updated_at=?2 WHERE id=?3",
            params![name, now(), identity],
        )? != 1
        {
            return Err(invalid("空间不存在"));
        }
        Ok(())
    }
    /// 功能：读取默认空间；尚未配置时原子登记一份默认共享空间和系统引用。
    /// 入参：self: &mut AppStore，为可开启写事务的应用库访问对象。
    /// 返回值：Ok(Space)，为 system_setting/default_space_id 指向的已登记空间；新建后提交并递归重读。
    /// 异常：默认引用损坏、路径非 UTF-8、目录/JSON/SQL 或事务提交失败传播 VeilError。
    /// 注意事项：即时事务内重查引用，协调首次启动并发；新目录的创建属于外部副作用，数据库回滚也不会自动删除目录。
    /// 已有默认引用不会自动修复或另选空间，指向不存在 ID 时明确失败。
    pub fn default_space(&mut self) -> Result<Space> {
        // 已有默认空间引用：校验引用有效后直接返回。
        if let Some(identity) = self.system::<String>("default_space_id")? {
            return self
                .spaces()?
                .into_iter()
                .find(|s| s.id == identity)
                .ok_or_else(|| invalid("默认空间引用无效"));
        }
        // 同一事务检查并创建，避免多个首次启动各生成默认空间。
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // 事务内重查：可能别的进程刚写入。
        let existing: Option<String> = tx
            .query_row(
                "SELECT value_json FROM system_setting WHERE key='default_space_id'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if existing.is_none() {
            // 创建名为「默认空间」的共享空间并登记为默认。
            let identity = id();
            let path = self.directory.join("spaces").join(&identity);
            crate::fsutil::create_dir_all_durable(&path)?;
            tx.execute(
                "INSERT INTO space_info VALUES (?1,'默认空间',NULL,?2,'shared',0,?3,?3)",
                params![
                    identity,
                    std::fs::canonicalize(path)?
                        .to_str()
                        .ok_or_else(|| invalid("路径不是 UTF-8"))?,
                    now()
                ],
            )?;
            // 与新空间行在同一事务登记默认引用，防止只写出引用而没有对应空间，或两个启动者各选不同默认空间。
            tx.execute(
                "INSERT INTO system_setting VALUES ('default_space_id',?1,?2)",
                params![json(&identity)?, now()],
            )?;
        }
        tx.commit()?;
        // 提交后递归读取：上面读到的可能已不是最新状态。
        self.default_space()
    }
    /// 功能：将默认创建/展示入口切换到一个已登记空间。
    /// 入参：self: &mut AppStore；identity: &str 为目标空间 ID。
    /// 返回值：即时事务内确认存在并保存系统设置成功后 Ok(())。
    /// 异常：目标不存在返回 InvalidFormat，查询、序列化、写入或提交失败传播 VeilError。
    /// 注意事项：存在校验与引用写入同一事务，不自动检查该空间目录可访问，也不移动现有容器或改变其存储模式。
    pub fn set_default_space(&mut self, identity: &str) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // 引用有效性校验与写入在同一事务，防止引用被并发删除的空间。
        if !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM space_info WHERE id=?1)",
            [identity],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(invalid("默认空间不存在"));
        }
        tx.execute("INSERT INTO system_setting VALUES ('default_space_id',?1,?2) ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json,updated_at=excluded.updated_at",params![json(identity)?,now()])?;
        tx.commit()?;
        Ok(())
    }
    /// 功能：列出本机已知容器与空间的关联，支持侧栏展示及唯一定位。
    /// 入参：self: &AppStore；没有额外筛选条件。
    /// 返回值：Ok(`Vec<Registration>`)，每项是 space_div 一行，按 space_id、sort_order、div_id 稳定排序。
    /// 异常：SQL 或字段读取/转换失败传播 VeilError。
    /// 注意事项：cached_name 是登记缓存，结果不保证容器目录存在或名称仍与加密库一致；不读取容器密码或内容。
    pub fn registrations(&self) -> Result<Vec<Registration>> {
        let mut stmt=self.conn.prepare("SELECT div_id,space_id,cached_name,sort_order,registered_at,updated_at FROM space_div ORDER BY space_id,sort_order,div_id")?;
        Ok(stmt
            .query_map([], |r| {
                Ok(Registration {
                    div_id: r.get(0)?,
                    space_id: r.get(1)?,
                    cached_name: r.get(2)?,
                    sort_order: r.get(3)?,
                    registered_at: r.get(4)?,
                    updated_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
    /// 功能：把容器登记在指定空间，或刷新同一空间内已有登记的名称缓存。
    /// 入参：self: &mut AppStore；space: &str 为已存在空间 ID；div: &str 为安全单段容器 ID；name: &str 为显示缓存名。
    /// 返回值：持共享容器生命周期锁、校验模式并提交 space_div 写入后 Ok(())。
    /// 异常：容器身份非法、已属于别的空间、专属空间另有容器返回校验错误；空间查询/SQL/事务失败传播 VeilError。
    /// 注意事项：空间模式检查和写入在同一即时事务，但不核验磁盘目录或真实加密库身份；name 在此不单独检查非空。
    /// 重复登记只更新 cached_name/updated_at，不重置 registered_at、sort_order 或更换所属空间。
    pub fn register(&mut self, space: &str, div: &str, name: &str) -> Result<()> {
        crate::work::validate_veil_id(div)?;
        // 共享生命周期锁：登记期间容器不能被并发删除。
        let _container_lock = self.acquire_container_shared_lock(div)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // 读目标空间的存储模式：dedicated 空间需要数量检查。
        let mode: String = tx.query_row(
            "SELECT storage_mode FROM space_info WHERE id=?1",
            [space],
            |r| r.get(0),
        )?;
        // 该容器是否已登记到别的空间。
        let existing: Option<String> = tx
            .query_row(
                "SELECT space_id FROM space_div WHERE div_id=?1",
                [div],
                |r| r.get(0),
            )
            .optional()?;
        if existing.as_deref().is_some_and(|s| s != space) {
            return Err(invalid("容器已登记到其他空间；请使用移动流程"));
        }
        // 专属空间的数量检查必须与登记在同一写事务内，防止并发双登记。
        if mode == "dedicated"
            && tx.query_row(
                "SELECT count(*) FROM space_div WHERE space_id=?1 AND div_id<>?2",
                params![space, div],
                |r| r.get::<_, i64>(0),
            )? > 0
        {
            return Err(invalid("专属空间只能登记一个容器"));
        }
        // 对新容器插入所属空间、初始排序和登记时间；同一容器已在此空间登记时只更新缓存名/修改时间，
        // 不通过冲突更新绕过空间迁移协议，也不重置首次登记时间。
        tx.execute("INSERT INTO space_div VALUES (?1,?2,?3,0,?4,?4) ON CONFLICT(div_id) DO UPDATE SET cached_name=excluded.cached_name,updated_at=excluded.updated_at",params![div,space,name,now()])?;
        tx.commit()?;
        Ok(())
    }
    /// 功能：取消容器在本机空间中的登记，保留磁盘内容以供后续重新登记。
    /// 入参：self: &AppStore；div: &str 为容器身份，会先取得其共享生命周期锁。
    /// 返回值：删除登记或原本无登记且没有移动意图时 Ok(())。
    /// 异常：记录存在 movement 字段而无法删除时返回 InvalidFormat，锁/SQL 失败传播 VeilError。
    /// 注意事项：DELETE 条件直接排除有移动意图记录，之后另查失败原因；不删除链接缓存、身份缓存、密文或容器目录。
    /// 检查使用 JSON 字段存在性，即显式 null 字段也与完全缺少字段不同。
    pub fn unregister(&self, div: &str) -> Result<()> {
        // 共享生命周期锁：取消登记期间容器不能被并发删除。
        let _container_lock = self.acquire_container_shared_lock(div)?;
        // 删除登记，但存在「移动意图」（div_state 里有 movement）时除外。
        let changed = self.conn.execute(
            "DELETE FROM space_div WHERE div_id=?1 AND NOT EXISTS (SELECT 1 FROM local_record WHERE record_type='div_state' AND record_key=?1 AND json_type(value_json,'$.movement') IS NOT NULL)",
            [div],
        )?;
        if changed == 0 {
            // 没删成：可能是没有登记记录，也可能是正在移动。
            let moving: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM local_record WHERE record_type='div_state' AND record_key=?1 AND json_type(value_json,'$.movement') IS NOT NULL)",
                [div],
                |row| row.get(0),
            )?;
            if moving {
                return Err(invalid("容器正在移动，不能取消登记"));
            }
        }
        Ok(())
    }
    /// 功能：保存解锁核对后的容器创建时间及可用状态，供未解锁列表和新链接使用。
    /// 入参：self: &AppStore；identity: &str 为 div_state 记录键；created_at: &str 须为可解析的 RFC 3339 时间。
    /// 返回值：成功插入或更新一行返回 Ok(())，正文为 created_at 与 availability=available。
    /// 异常：记录校验、序列化、SQL 失败传播 VeilError；已有 movement 字段导致条件更新为零时返回 InvalidFormat。
    /// 注意事项：不自行读取加密库；只允许无移动意图时替换整段身份缓存，不能把普通展示刷新覆盖待恢复移动状态。
    pub(crate) fn cache_container_identity(&self, identity: &str, created_at: &str) -> Result<()> {
        // 这份正文只代表身份与当前可用观察，不带移动恢复阶段；后面的条件 upsert 防止用它盖掉 movement。
        let value = serde_json::json!({"created_at": created_at, "availability": "available"});
        validate_record("div_state", identity, &value)?;
        let changed = self.conn.execute("INSERT INTO local_record VALUES ('div_state',?1,?2,?3) ON CONFLICT(record_type,record_key) DO UPDATE SET value_json=excluded.value_json,updated_at=excluded.updated_at WHERE json_type(local_record.value_json,'$.movement') IS NULL", params![identity,json(&value)?,now()])?;
        if changed != 1 {
            return Err(invalid("容器存在待恢复的移动，不能用展示缓存覆盖移动意图"));
        }
        Ok(())
    }
    /// 功能：记录容器目录当前不可访问的观察结果，同时保留待恢复移动等其他字段。
    /// 入参：self: &AppStore；identity: &str 为已保存的 div_state 记录键。
    /// 返回值：无记录直接 Ok(())；已有合法记录时仅更新 availability 和更新时间后 Ok(())。
    /// 异常：读取/解析/内容验证或 SQL 更新失败传播 VeilError。
    /// 注意事项：不创建缺失身份记录，不删除容器；json_set 对当前数据库正文定点修改，避免把旧快照的 movement 覆盖回去。
    pub(crate) fn observe_unavailable(&self, identity: &str) -> Result<()> {
        if let Some(value) = self.record::<serde_json::Value>("div_state", identity)? {
            validate_record("div_state", identity, &value)?;
            // 只更新观察值，不能将旧快照中的其他字段覆盖回当前记录。
            // json_set 只改 $.availability，保留 movement 等其他字段。
            self.conn.execute("UPDATE local_record SET value_json=json_set(value_json,'$.availability','unavailable'),updated_at=?1 WHERE record_type='div_state' AND record_key=?2",params![now(),identity])?;
        }
        Ok(())
    }
    /// 功能：读取并校验一项用户偏好，对尚未设置的已知键提供默认值。
    /// 入参：self: &AppStore；key: &str 只接受 hint_level、layout、view_mode、免密秒数、导入间隔/数量与默认打开应用映射。
    /// 返回值：Ok(Value)，为保存值或该键缺省值；缺省值只返回，不写回 user_setting。
    /// 异常：未知键、保存值语法/类型/范围不符返回校验错误，数据库访问失败传播 VeilError。
    /// 注意事项：无记录与坏数据不同，坏数据不会静默回退；每次查询当前库，不缓存为进程全局偏好。
    pub fn user_setting(&self, key: &str) -> Result<serde_json::Value> {
        // 服务统一定义的默认值表：未知键直接报错，不做猜测。
        let default: serde_json::Value = match key {
            "hint_level" => "full".into(),
            "layout" => "grid".into(),
            "view_mode" => "type".into(),
            UNLOCK_CACHE_SETTING => serde_json::json!(DEFAULT_UNLOCK_CACHE_SECONDS),
            IMPORT_COMMIT_INTERVAL_SETTING => {
                serde_json::json!(DEFAULT_IMPORT_COMMIT_INTERVAL_SECONDS)
            }
            IMPORT_COMMIT_COUNT_SETTING => serde_json::json!(DEFAULT_IMPORT_COMMIT_COUNT),
            EXTERNAL_OPEN_DEFAULTS_SETTING => serde_json::json!({}),
            _ => return Err(invalid("未知用户设置")),
        };
        // 读库：没有记录就用默认值，不把默认值写回表。
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT value_json FROM user_setting WHERE key=?1",
                [key],
                |r| r.get(0),
            )
            .optional()?;
        let value = value.map(|s| parse(&s)).transpose()?.unwrap_or(default);
        // 库里读出的值也必须通过校验：非法 JSON/越界值不能当默认值糊弄。
        validate_setting(key, &value)?;
        Ok(value)
    }
    /// 功能：为解锁会话确定内存中免密凭据的保留秒数。
    /// 入参：self: &AppStore，读取 unlock_cache_seconds 用户设置。
    /// 返回值：Ok(u64)，范围 0～86400，默认 3600，0 表示关闭免密保留。
    /// 异常：数据库、JSON 或值类型/范围校验失败传播 VeilError。
    /// 注意事项：只提供策略数值，不保存密码、不设置实际定时器，也不证明其他会话已应用该值。
    pub fn unlock_cache_seconds(&self) -> Result<u64> {
        let value = self.user_setting(UNLOCK_CACHE_SETTING)?;
        value.as_u64().ok_or_else(|| invalid("免密时长无效"))
    }
    /// 功能：提供导入监听攒批的时间阈值，供批量提交策略使用。
    /// 入参：self: &AppStore，读取 import_commit_interval_seconds。
    /// 返回值：Ok(u64)，1～60 秒，默认 1。
    /// 异常：存储读取或值校验失败传播 VeilError。
    /// 注意事项：是消息进入批次后的调度阈值，不是严格事务完成时限，也不会修改已启动监听器的策略。
    pub fn import_commit_interval_seconds(&self) -> Result<u64> {
        self.user_setting(IMPORT_COMMIT_INTERVAL_SETTING)?
            .as_u64()
            .ok_or_else(|| invalid("导入提交间隔无效"))
    }
    /// 功能：提供导入批量提交的累计文件条目阈值，减少逐条写事务。
    /// 入参：self: &AppStore，读取 import_commit_count。
    /// 返回值：Ok(usize)，1～100，默认 8。
    /// 异常：数据库、JSON、范围或转为 usize 失败传播 VeilError。
    /// 注意事项：数量由队列消息的业务权重表达，不必等于通道消息数；本方法不启动队列或立即提交。
    pub fn import_commit_count(&self) -> Result<usize> {
        self.user_setting(IMPORT_COMMIT_COUNT_SETTING)?
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| invalid("导入提交数量无效"))
    }
    /// 功能：校验用户偏好后保存或覆盖相同设置键的值和更新时间。
    /// 入参：self: &AppStore；key: &str 为已知键；value: &serde_json::Value 为该键允许的枚举、整数或应用映射。
    /// 返回值：校验、JSON 序列化及单条 upsert 成功后 Ok(())。
    /// 异常：未知键、值不合法、序列化或 SQL 失败传播 VeilError。
    /// 注意事项：写入不自动通知或重建已加载的界面/监听策略；调用方应显式重读，不能把保存成功当作所有任务已切换。
    pub fn set_user_setting(&self, key: &str, value: &serde_json::Value) -> Result<()> {
        // 保存前共用读取侧同一校验，避免把未知键、非法枚举或越界整数写成下一次无法解释的偏好。
        validate_setting(key, value)?;
        self.conn.execute("INSERT INTO user_setting VALUES (?1,?2,?3) ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json,updated_at=excluded.updated_at",params![key,json(value)?,now()])?;
        Ok(())
    }
    /// 功能：按键读取内部系统设置，并按调用方要求的模型解释 JSON。
    /// 入参：self: &AppStore；key: &str 为系统键，T: DeserializeOwned 指定期望值类型。
    /// 返回值：记录存在且解码成功为 Ok(Some(T))，缺失为 Ok(None)。
    /// 异常：SQL 或 JSON/类型转换失败传播 VeilError。
    /// 注意事项：此内部通用读取不限制键白名单，也不额外校验业务语义；面向客户端展示应走 system_setting_view。
    pub fn system<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT value_json FROM system_setting WHERE key=?1",
                [key],
                |r| r.get(0),
            )
            .optional()?;
        value.map(|s| parse(&s)).transpose()
    }
    /// 功能：向设置页提供允许公开的系统配置，避免暴露任意内部键。
    /// 入参：self: &AppStore；key: &str 仅接受 device_id 或 default_space_id。
    /// 返回值：Ok(Value)，已保存 JSON 原值，缺失为 JSON null。
    /// 异常：其他键返回 InvalidFormat；查询或 JSON 解析失败传播 VeilError。
    /// 注意事项：不初始化缺失默认引用，也不重新验证设备 UUID 或空间存在性；业务核验由对应专用入口完成。
    pub fn system_setting_view(&self, key: &str) -> Result<serde_json::Value> {
        // 白名单：目前只对外暴露设备身份与默认空间。
        if !matches!(key, "device_id" | "default_space_id") {
            return Err(invalid("未知系统设置"));
        }
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT value_json FROM system_setting WHERE key=?1",
                [key],
                |r| r.get(0),
            )
            .optional()?;
        value.map(|s| parse(&s)).transpose().map(|value| {
            value.unwrap_or(serde_json::Value::Null)
        })
    }
    /// 功能：读取一条本机缓存或状态记录，供恢复、展示和一次性行为判断。
    /// 入参：self: &AppStore；kind/key: &str 组成 local_record 的类型与记录键；T 指定调用方期望模型。
    /// 返回值：记录存在并反序列化成功为 Ok(Some(T))，不存在为 Ok(None)。
    /// 异常：数据库或 JSON/目标类型不匹配传播 VeilError。
    /// 注意事项：读取接口不调用完整 validate_record，也不限制 kind 白名单；结构能解码不等于所有业务字段已经校验。
    pub fn record<T: DeserializeOwned>(&self, kind: &str, key: &str) -> Result<Option<T>> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT value_json FROM local_record WHERE record_type=?1 AND record_key=?2",
                params![kind, key],
                |r| r.get(0),
            )
            .optional()?;
        value.map(|s| parse(&s)).transpose()
    }
    /// 功能：保存一条经过规定结构验证的本机记录，支持最近访问、链接缓存和恢复状态等场景。
    /// 入参：self: &AppStore；kind: &str 仅为 recent_div/link_cache/space_access/hint_state/div_state；key: &str 为该类型的业务键；value: &T 为可序列化正文。
    /// 返回值：校验及 INSERT ... ON CONFLICT 更新正文/时间成功时 Ok(())。
    /// 异常：未知类型、结构/字段/路径/时间校验、序列化或 SQL 失败传播 VeilError。
    /// 注意事项：明文应用库不应保存容器口令或文件密钥；这是整体正文覆盖，不自动合并 movement 等字段，调用方须遵守状态更新协议。
    pub fn set_record<T: Serialize>(&self, kind: &str, key: &str, value: &T) -> Result<()> {
        // 类型白名单：只有设计文档约定的五类记录可写。
        if !matches!(
            kind,
            "recent_div" | "link_cache" | "space_access" | "hint_state" | "div_state"
        ) {
            return Err(invalid("未知本机记录类型"));
        }
        // 以 (类型,业务键) 覆盖整段正文；validate_record 只核对允许的结构，不替调用方合并旧状态。
        validate_record(kind, key, value)?;
        self.conn.execute("INSERT INTO local_record VALUES (?1,?2,?3,?4) ON CONFLICT(record_type,record_key) DO UPDATE SET value_json=excluded.value_json,updated_at=excluded.updated_at",params![kind,key,json(value)?,now()])?;
        Ok(())
    }
    /// 功能：按类型读取最近更新的本机记录集合，供链接列表和最近访问等展示使用。
    /// 入参：self: &AppStore；kind: &str 为记录类型；limit: u32 为最多条数，0 返回空集合。
    /// 返回值：Ok(`Vec<T>`)，每项是一条 value_json 解码结果，按 updated_at 倒序、record_key 升序。
    /// 异常：查询、字段读取或任意记录反序列化失败都会使整个方法返回 VeilError，不跳过坏项。
    /// 注意事项：不执行类型白名单或 validate_record 完整语义检查，也不返回记录键；调用方需选对 T 和规模限制。
    pub fn records<T: DeserializeOwned>(&self, kind: &str, limit: u32) -> Result<Vec<T>> {
        let mut stmt=self.conn.prepare("SELECT value_json FROM local_record WHERE record_type=?1 ORDER BY updated_at DESC,record_key LIMIT ?2")?;
        stmt.query_map(params![kind, limit], |r| r.get::<_, String>(0))?
            .map(|s| parse(&s?))
            .collect()
    }
    /// 功能：原子登记某类提示已处理，让并发调用至多一个获得首次显示资格。
    /// 入参：self: &mut AppStore；key: &str 为提示场景键，例如 first_init/first_pack/first_unpack。
    /// 返回值：缺记录并插入 true 后返回 Ok(true)，已有可解码布尔记录则 Ok(false)，即便保存值本身为 false 也视为已存在。
    /// 异常：即时事务、JSON 布尔解析、插入或提交失败传播 VeilError。
    /// 注意事项：不在这里限制键名，也不真正显示提示；提交发生在调用方渲染之前，后续展示取消不会自动撤销标记。
    pub fn mark_hint(&mut self, key: &str) -> Result<bool> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value:Option<String>=tx.query_row("SELECT value_json FROM local_record WHERE record_type='hint_state' AND record_key=?1",[key],|r|r.get(0)).optional()?;
        if let Some(value) = value {
            // 已有标记：校验格式后按「已展示过」返回 false。
            let _: bool = parse(&value)?;
            tx.commit()?;
            return Ok(false);
        }
        // 首次：写入 'true' 标记并返回 true（调用方展示提示）。
        tx.execute(
            "INSERT INTO local_record VALUES ('hint_state',?1,'true',?2)",
            params![key, now()],
        )?;
        tx.commit()?;
        Ok(true)
    }
}
/// 功能：集中限定本机可保存的偏好键及对应类型/范围，防止读写两侧解释不一致。
/// 入参：key: &str 为设置名；value: &Value 为候选 JSON 值。
/// 返回值：合法时 Ok(())；枚举键须取精确小写值，免密为 0～86400 整数，提交间隔 1～60，数量 1～100。
/// 异常：未知键、类型/范围不符或打开应用映射超过 200 项、扩展名/应用标识不合法时返回 InvalidFormat。
/// 注意事项：只验证内存候选，不写库；external_open_defaults 每项是“小写无点扩展名→bundle ID 字符串”，不验证应用真的安装。
fn validate_setting(key: &str, value: &serde_json::Value) -> Result<()> {
    // 导入合并数量：必须是 1..=100 的整数。
    if key == IMPORT_COMMIT_COUNT_SETTING {
        if !value
            .as_u64()
            .is_some_and(|count| (1..=MAX_IMPORT_COMMIT_COUNT).contains(&count))
        {
            return Err(invalid("导入提交数量必须是 1 至 100 的整数"));
        }
        return Ok(());
    }
    // 导入合并间隔：必须是 1..=60 的整数秒。
    if key == IMPORT_COMMIT_INTERVAL_SETTING {
        if !value
            .as_u64()
            .is_some_and(|seconds| (1..=MAX_IMPORT_COMMIT_INTERVAL_SECONDS).contains(&seconds))
        {
            return Err(invalid("导入提交间隔必须是 1 至 60 的整数秒"));
        }
        return Ok(());
    }
    // 免密时长：0..=86400 秒（0 表示关闭）。
    if key == UNLOCK_CACHE_SETTING {
        if !value
            .as_u64()
            .is_some_and(|seconds| seconds <= MAX_UNLOCK_CACHE_SECONDS)
        {
            return Err(invalid("免密时长必须是 0 至 24 小时的整数秒"));
        }
        return Ok(());
    }
    // 打开方式登记：对象格式、条数上限，且每个扩展名/应用标识都要合法。
    if key == EXTERNAL_OPEN_DEFAULTS_SETTING {
        let entries = value
            .as_object()
            .ok_or_else(|| invalid("打开方式必须按扩展名登记"))?;
        if entries.len() > MAX_EXTERNAL_OPEN_DEFAULTS {
            return Err(invalid("打开方式条目过多"));
        }
        // 每项是一种扩展名及其默认应用标识；逐项验证键和值，防止合法对象外壳包住不可解释的条目。
        for (extension, bundle_id) in entries {
            if !valid_extension(extension) {
                return Err(invalid(
                    "扩展名只能包含小写字母和数字，长度 1 至 16，不含点号",
                ));
            }
            let bundle_id = bundle_id
                .as_str()
                .ok_or_else(|| invalid("应用标识必须是字符串"))?;
            if !valid_bundle_id(bundle_id) {
                return Err(invalid("应用标识格式无效"));
            }
        }
        return Ok(());
    }
    // 枚举型设置：白名单取值。
    let allowed: &[&str] = match key {
        "hint_level" => &["full", "brief", "off"],
        "layout" => &["grid", "list"],
        "view_mode" => &["type", "path"],
        _ => return Err(invalid("未知用户设置")),
    };
    if !value.as_str().is_some_and(|s| allowed.contains(&s)) {
        return Err(invalid("用户设置值无效"));
    }
    Ok(())
}

/// 功能：确认默认打开方式映射的键采用统一无点小写扩展名格式。
/// 入参：extension: &str 为待登记扩展名。
/// 返回值：1～16 字节且全为 ASCII 小写字母/数字时 true，其他 false。
/// 异常：只检查字符串，不返回错误。
/// 注意事项：不会转小写或去掉点，不确认此扩展名是否对应已支持媒体格式。
fn valid_extension(extension: &str) -> bool {
    (1..=MAX_EXTENSION_LENGTH).contains(&extension.len())
        && extension
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

/// 功能：限定默认打开应用标识的文本形状，拒绝非预期字符和过长值。
/// 入参：bundle_id: &str，为候选应用标识。
/// 返回值：1～255 字节、至少含一个点，且所有字节为 ASCII 字母数字/点/连字符/下划线时 true。
/// 异常：不访问系统或文件，不返回错误。
/// 注意事项：不是平台 bundle 标识完整语法验证，连续点、首尾点也未在此额外排除；不确认应用是否存在。
fn valid_bundle_id(bundle_id: &str) -> bool {
    (1..=MAX_BUNDLE_ID_LENGTH).contains(&bundle_id.len())
        && bundle_id.contains('.')
        && bundle_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// div_state 记录的结构：容器身份缓存＋最近观察的可用状态＋可选移动意图。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DivState {
    /// 解锁时核对到的容器原始创建时间（RFC3339）。
    created_at: String,
    /// 最近观察到的可用状态：available / unavailable / unknown。
    availability: Option<String>,
    /// 待恢复的「移动容器」意图（跨会话恢复用）。
    movement: Option<Movement>,
}
/// 容器移动意图：源目录、目标目录、暂存目录与发布进度。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Movement {
    /// 源容器目录（绝对路径）。
    source: PathBuf,
    /// 目标容器目录（绝对路径）。
    destination: PathBuf,
    /// 移动暂存目录（复制过程中存放半成品的位置）。
    #[serde(default)]
    staging: Option<PathBuf>,
    /// 是否已经完成验证、可以发布（改名到目标位置）。
    #[serde(default)]
    ready_to_publish: bool,
    /// 源空间 ID（用于跨空间移动时恢复登记）。
    #[serde(default)]
    source_space_id: Option<String>,
    /// 目标空间 ID。
    #[serde(default)]
    target_space_id: Option<String>,
}
/// space_access 记录的结构：平台目录访问授权引用。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryAccess {
    /// 平台授权引用（如移动端安全作用域书签）。
    reference: String,
}
/// 功能：按本机记录类型验证序列化正文的结构与必要业务字段，供保存及特定更新入口使用。
/// 入参：kind: &str 为已知五类记录；key: &str 为类型内业务身份；value: &T 为候选可序列化对象。
/// 返回值：正文可转换到对应模型且各项规则通过为 Ok(())。
/// 异常：序列化失败为 SerializationError，结构/类型/字段/路径/时间不合规则返回 InvalidFormat。
/// 注意事项：recent_div 核对 ID 与 key；link_cache 检查链接正文和绝对路径但不核对路径等于 key；hint_state 只要求布尔。
/// div_state 核对可用状态、RFC 3339 时间及移动绝对路径/暂存命名约定，不验证真实目录或完整移动阶段；space_access 仅要求引用非空。
fn validate_record<T: Serialize>(kind: &str, key: &str, value: &T) -> Result<()> {
    // 先统一编码，再按记录类型解成带字段约束的模型；每个分支针对不同本机业务实体而非任意 JSON。
    let text = json(value)?;
    match kind {
        // 最近访问：ID 必须与记录键一致、名称非空、路径必须是绝对路径。
        "recent_div" => {
            let r: RecentContainer = parse(&text)?;
            if r.id != key || r.name.trim().is_empty() || !Path::new(&r.path).is_absolute() {
                return Err(invalid("最近记录无效"));
            }
        }
        // 链接缓存：链接正文必须有效，缓存路径必须是绝对路径。
        "link_cache" => {
            let r: crate::config::LinkRecord = parse(&text)?;
            r.link.validate()?;
            if !r.link_path.is_absolute() {
                return Err(invalid("链接缓存路径无效"));
            }
        }
        // 提示状态：布尔值。
        "hint_state" => {
            let _: bool = parse(&text)?;
        }
        // 容器状态：可用状态枚举、创建时间格式、移动意图路径与暂存命名约定。
        "div_state" => {
            let r: DivState = parse(&text)?;
            if r.availability
                .as_deref()
                .is_some_and(|s| !matches!(s, "available" | "unavailable" | "unknown"))
            {
                return Err(invalid("容器可用状态无效"));
            }
            chrono::DateTime::parse_from_rfc3339(&r.created_at)
                .map_err(|_| invalid("容器身份缓存时间无效"))?;
            if let Some(m) = r.movement {
                // 源/目标必须是绝对路径。
                if !m.source.is_absolute() || !m.destination.is_absolute() {
                    return Err(invalid("容器移动意图路径无效"));
                }
                // 暂存目录必须与目标同父目录，且按约定前缀命名。
                if let Some(staging) = m.staging {
                    let prefix = format!(".veil-moving-{key}-");
                    if !staging.is_absolute()
                        || staging.parent() != m.destination.parent()
                        || !staging
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with(&prefix))
                    {
                        return Err(invalid("容器移动暂存路径无效"));
                    }
                }
                // 空间 ID 若给出则不能为空字符串。
                if m.source_space_id.as_deref().is_some_and(str::is_empty)
                    || m.target_space_id.as_deref().is_some_and(str::is_empty)
                {
                    return Err(invalid("容器移动空间身份无效"));
                }
                let _ = m.ready_to_publish;
            }
        }
        // 目录访问引用：非空字符串。
        "space_access" => {
            let r: DirectoryAccess = parse(&text)?;
            if r.reference.is_empty() {
                return Err(invalid("目录访问引用为空"));
            }
        }
        _ => return Err(invalid("未知本机记录类型")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 功能：验证系统设置视图只放行白名单键（device_id/default_space_id），未知键报错。
    /// 入参：无；测试自行构造空间、设置或路径样本。
    /// 返回值：无；所有断言通过即场景验证成功。
    /// 异常：准备数据或调用失败、断言不符时 panic，使测试失败。
    /// 注意事项：用于验证本场景边界，不代表其他设置或路径形状也被接受。
    #[test]
    fn system_setting_view_only_exposes_whitelisted_keys() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = AppStore::open(directory.path()).unwrap();
        let default_space = store.default_space().unwrap();
        let device = store.system_setting_view("device_id").unwrap();
        assert!(veil_core::metadata::is_uuid_v4(device.as_str().unwrap()));
        assert_eq!(
            store.system_setting_view("default_space_id").unwrap().as_str(),
            Some(default_space.id.as_str())
        );
        assert!(store.system_setting_view("unknown_key").is_err());
    }

    /// 功能：验证导入合并间隔：默认 1 秒；写入 5 持久化；0 与 61 被拒绝。
    /// 入参：无；测试自行构造空间、设置或路径样本。
    /// 返回值：无；所有断言通过即场景验证成功。
    /// 异常：准备数据或调用失败、断言不符时 panic，使测试失败。
    /// 注意事项：用于验证本场景边界，不代表其他设置或路径形状也被接受。
    #[test]
    fn import_commit_interval_is_persistent_and_validated() {
        let directory = tempfile::tempdir().unwrap();
        let store = AppStore::open(directory.path()).unwrap();
        assert_eq!(store.import_commit_interval_seconds().unwrap(), 1);
        store
            .set_user_setting(IMPORT_COMMIT_INTERVAL_SETTING, &serde_json::json!(5))
            .unwrap();
        assert!(
            store
                .set_user_setting(IMPORT_COMMIT_INTERVAL_SETTING, &serde_json::json!(0))
                .is_err()
        );
        assert!(
            store
                .set_user_setting(IMPORT_COMMIT_INTERVAL_SETTING, &serde_json::json!(61))
                .is_err()
        );
        drop(store);
        assert_eq!(
            AppStore::open(directory.path())
                .unwrap()
                .import_commit_interval_seconds()
                .unwrap(),
            5
        );
    }

    /// 功能：验证导入合并数量：默认 8；边界 1/100 可写；0 与 101 被拒绝。
    /// 入参：无；测试自行构造空间、设置或路径样本。
    /// 返回值：无；所有断言通过即场景验证成功。
    /// 异常：准备数据或调用失败、断言不符时 panic，使测试失败。
    /// 注意事项：用于验证本场景边界，不代表其他设置或路径形状也被接受。
    #[test]
    fn import_commit_count_is_persistent_and_validated() {
        let directory = tempfile::tempdir().unwrap();
        let store = AppStore::open(directory.path()).unwrap();
        assert_eq!(store.import_commit_count().unwrap(), 8);
        for boundary in [1, 100] {
            store
                .set_user_setting(IMPORT_COMMIT_COUNT_SETTING, &serde_json::json!(boundary))
                .unwrap();
            assert_eq!(store.import_commit_count().unwrap(), boundary as usize);
        }
        store
            .set_user_setting(IMPORT_COMMIT_COUNT_SETTING, &serde_json::json!(37))
            .unwrap();
        for invalid in [0, 101] {
            assert!(
                store
                    .set_user_setting(IMPORT_COMMIT_COUNT_SETTING, &serde_json::json!(invalid))
                    .is_err()
            );
        }
        drop(store);
        assert_eq!(
            AppStore::open(directory.path())
                .unwrap()
                .import_commit_count()
                .unwrap(),
            37
        );
    }

    /// 功能：验证打开方式登记：未设置默认空对象；合法条目写入后持久化。
    /// 入参：无；测试自行构造空间、设置或路径样本。
    /// 返回值：无；所有断言通过即场景验证成功。
    /// 异常：准备数据或调用失败、断言不符时 panic，使测试失败。
    /// 注意事项：用于验证本场景边界，不代表其他设置或路径形状也被接受。
    #[test]
    fn external_open_defaults_are_persistent_and_default_to_empty() {
        let directory = tempfile::tempdir().unwrap();
        let store = AppStore::open(directory.path()).unwrap();
        assert_eq!(
            store.user_setting(EXTERNAL_OPEN_DEFAULTS_SETTING).unwrap(),
            serde_json::json!({})
        );
        let valid = serde_json::json!({
            "mp4": "com.apple.QuickTimePlayerX",
            "pdf": "com.apple.Preview",
            "7z": "com.7z.7z",
        });
        store
            .set_user_setting(EXTERNAL_OPEN_DEFAULTS_SETTING, &valid)
            .unwrap();
        assert_eq!(
            store.user_setting(EXTERNAL_OPEN_DEFAULTS_SETTING).unwrap(),
            valid
        );
        drop(store);
        assert_eq!(
            AppStore::open(directory.path())
                .unwrap()
                .user_setting(EXTERNAL_OPEN_DEFAULTS_SETTING)
                .unwrap(),
            valid
        );
    }

    /// 功能：验证打开方式登记：拒绝各种非法条目与超量条目。
    /// 入参：无；测试自行构造空间、设置或路径样本。
    /// 返回值：无；所有断言通过即场景验证成功。
    /// 异常：准备数据或调用失败、断言不符时 panic，使测试失败。
    /// 注意事项：用于验证本场景边界，不代表其他设置或路径形状也被接受。
    #[test]
    fn external_open_defaults_reject_invalid_entries() {
        let directory = tempfile::tempdir().unwrap();
        let store = AppStore::open(directory.path()).unwrap();
        for invalid in [
            serde_json::json!("com.apple.Preview"),
            serde_json::json!(["mp4"]),
            serde_json::json!({"MP4": "com.apple.QuickTimePlayerX"}),
            serde_json::json!({".mp4": "com.apple.QuickTimePlayerX"}),
            serde_json::json!({"mp4": ""}),
            serde_json::json!({"mp4": "QuickTime Player"}),
            serde_json::json!({"mp4": 7}),
            serde_json::json!({"abcdefghijklmnopq": "com.apple.Preview"}),
        ] {
            assert!(
                store
                    .set_user_setting(EXTERNAL_OPEN_DEFAULTS_SETTING, &invalid)
                    .is_err(),
                "应拒绝无效登记：{invalid}"
            );
        }
        let too_many: serde_json::Map<String, serde_json::Value> = (0..=MAX_EXTERNAL_OPEN_DEFAULTS)
            .map(|index| (format!("e{index}"), serde_json::json!("com.apple.Preview")))
            .collect();
        assert!(
            store
                .set_user_setting(
                    EXTERNAL_OPEN_DEFAULTS_SETTING,
                    &serde_json::Value::Object(too_many)
                )
                .is_err()
        );
    }

    /// 功能：验证iOS 空间路径重绑：只接受本应用默认空间形状的迁移。
    /// 入参：无；测试自行构造空间、设置或路径样本。
    /// 返回值：无；所有断言通过即场景验证成功。
    /// 异常：准备数据或调用失败、断言不符时 panic，使测试失败。
    /// 注意事项：用于验证本场景边界，不代表其他设置或路径形状也被接受。
    #[test]
    fn ios_space_relocation_only_accepts_own_default_space() {
        let id = "aab695cd-3e2c-48d6-9d27-e2ec6770db17";
        let old = format!(
            "/private/var/mobile/Containers/Data/Application/62291DE1-522B-499F-A6A5-E8344D5B3A8C/Library/Application Support/Veil/spaces/{id}"
        );
        let root = Path::new(
            "/private/var/mobile/Containers/Data/Application/9DA3F8B1-D6DA-4804-936F-D85706175424/Library/Application Support/Veil",
        );
        assert_eq!(
            ios_relocated_space_path(Path::new(&old), root, id),
            Some(root.join("spaces").join(id))
        );
        assert_eq!(
            ios_relocated_space_path(Path::new(&old), root, "not-an-id"),
            None
        );
        assert_eq!(
            ios_relocated_space_path(Path::new(&old.replace("/spaces/", "/other/")), root, id),
            None
        );
        assert_eq!(
            ios_relocated_space_path(Path::new(&old), Path::new("/tmp/other"), id),
            None
        );
        assert_eq!(
            ios_relocated_space_path(&root.join("spaces").join(id), root, id),
            None
        );
    }

    /// 功能：验证免密时长：默认 3600 且不写回表；0/900 可写；负数、越界、小数、字符串被拒绝。
    /// 入参：无；测试自行构造空间、设置或路径样本。
    /// 返回值：无；所有断言通过即场景验证成功。
    /// 异常：准备数据或调用失败、断言不符时 panic，使测试失败。
    /// 注意事项：用于验证本场景边界，不代表其他设置或路径形状也被接受。
    #[test]
    fn unlock_cache_setting_defaults_and_validates() {
        let root = tempfile::TempDir::new().unwrap();
        let store = AppStore::open(root.path()).unwrap();
        assert_eq!(
            store.unlock_cache_seconds().unwrap(),
            DEFAULT_UNLOCK_CACHE_SECONDS
        );

        // 未设置时使用默认值，不把默认值写回设置表。
        store
            .conn
            .execute(
                "DELETE FROM user_setting WHERE key=?1",
                [UNLOCK_CACHE_SETTING],
            )
            .unwrap();
        drop(store);
        let store = AppStore::open(root.path()).unwrap();
        assert_eq!(store.unlock_cache_seconds().unwrap(), 3600);
        let stored: Option<String> = store
            .conn
            .query_row(
                "SELECT value_json FROM user_setting WHERE key=?1",
                [UNLOCK_CACHE_SETTING],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(stored, None);

        assert!(
            store
                .set_user_setting(UNLOCK_CACHE_SETTING, &serde_json::json!(0))
                .is_ok()
        );
        assert_eq!(store.unlock_cache_seconds().unwrap(), 0);
        assert!(
            store
                .set_user_setting(UNLOCK_CACHE_SETTING, &serde_json::json!(900))
                .is_ok()
        );
        assert_eq!(store.unlock_cache_seconds().unwrap(), 900);
        for invalid_value in [
            serde_json::json!(-1),
            serde_json::json!(MAX_UNLOCK_CACHE_SECONDS + 1),
            serde_json::json!(1.5),
            serde_json::json!("3600"),
        ] {
            assert!(
                store
                    .set_user_setting(UNLOCK_CACHE_SETTING, &invalid_value)
                    .is_err()
            );
        }
    }
}
