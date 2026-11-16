//! 本机数据库访问门面。公开视图用于展示，不序列化整体配置或读取旧 JSONC。
//!
//! 所属业务模块：veil-service 的配置（config）模块。它把本机应用数据库 app.db
//! （普通 SQLite，保存空间、容器登记、设置与本地记录）映射成内存中的 `AppConfig`
//! 视图；本模块为调用方提供空间、登记、链接缓存和设置的组合访问，其他 service 模块也可经 storage::app 访问业务表。
//! 旧版 JSONC 配置文件既不读取也不迁移，内存视图不会因数据库更新自动同步。
//!
//! 文件内核心清单：
//! - `HintLevel` / `UserSettings`：提示级别与用户偏好；
//! - `WorkSection` / `WorkConfig`（来自 work 模块）：工作区（空间）配置视图；
//! - `VeilConfig` / `LinkRecord` / `LinkStatus`：容器展示缓存与链接缓存记录；
//! - `ResolvedVeil`：解析出的容器身份（ID、名称、目录、链接路径、是否恢复链接）；
//! - `AppConfig`：整个本机配置门面（load 从 app.db 读，其余方法做业务操作）；
//! - `ConfigSummary`：给设置页展示的配置摘要。
//!
//! 依赖能力：storage::app 的 AppStore（SQLite 业务访问）、storage::invalid（统一
//! 非法参数错误）、work 模块的工作区类型、link 模块的链接模型。
//!
//! 整体业务目标：把「容器在哪、叫什么、登记在哪个空间」这类本机事实集中管理，
//! 并在已知登记范围内检查容器身份与目录冲突；检查不是对所有磁盘的穷举，也不能替代实际创建/删除的并发协议。
use crate::{
    error::{Result, VeilError},
    link::{LINK_EXTENSION, VeilLink},
    storage::{
        app::{AppStore, Space},
        invalid,
    },
    work::{WorkConfig, WorkType},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// 界面提示级别：Full 显示完整提示，Brief 只显示简略提示，Off 完全关闭。
/// 序列化为小写 "full"/"brief"/"off"，持久化在 app.db 的 user_setting 表。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HintLevel {
    Full,
    Brief,
    Off,
}
impl HintLevel {
    /// 功能：把提示策略转成用户设置使用的规范文本，便于保存或展示。
    /// 入参：self: HintLevel 为 Full、Brief 或 Off 之一。
    /// 返回值：&'static str，依次为 full、brief、off。
    /// 异常：枚举匹配，不返回错误。
    /// 注意事项：只转换表示，不写入 app.db，也不检查某条具体提示是否已经展示。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Brief => "brief",
            Self::Off => "off",
        }
    }
    /// 功能：解释用户输入或本机设置中的提示策略文本。
    /// 入参：value: &str 为级别文本，允许首尾空白和 ASCII 大小写变体。
    /// 返回值：识别 full/brief/off 为 Some(HintLevel)，其他文本或空文本为 None。
    /// 异常：不访问数据库，不返回 Result。
    /// 注意事项：未知值不会在这里默认成 Full，回退或拒绝由调用方决定。
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "full" => Some(Self::Full),
            "brief" => Some(Self::Brief),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}
/// 用户偏好集合（当前只有提示级别；未来可扩展更多偏好项）。
#[derive(Debug, Clone)]
pub struct UserSettings {
    /// 界面提示级别：full / brief / off。
    pub hint_level: HintLevel,
}
/// 工作区（空间）配置视图：默认空间与自定义空间的集合。
///
/// 每个工作区对应 app.db space_info 里的一条空间记录；WorkConfig 记录其本地路径、
/// 类型（默认/专属/自定义）与说明。map 的 key 是空间 ID。
#[derive(Debug, Clone, Default)]
pub struct WorkSection {
    /// 默认空间（system_setting.default_space_id 指向的空间），最多一个。
    pub default: Option<WorkConfig>,
    /// 其余空间，key 为空间稳定 ID。
    pub custom: HashMap<String, WorkConfig>,
}
/// 单个容器的展示缓存：来自本机登记（space_div），未解锁时也能列出容器。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VeilConfig {
    /// 容器稳定 ID（对应容器库 div_info.veil_id）。
    pub veil_id: String,
    /// 容器展示名称（登记时的缓存名，解锁后会被权威名称刷新）。
    pub veil_name: String,
    /// 容器原始创建时间（来自 div_state 身份缓存，缓存不足时为空串）。
    pub created_at: String,
    /// 最近访问时间（本结构中恒为 None，仅保留字段兼容展示）。
    pub last_accessed: Option<String>,
    /// 与该容器关联的链接文件路径列表（本结构中恒为空，展示层自行填充）。
    pub links: Vec<PathBuf>,
}
/// 链接文件在本机的观察状态：Present 存在、Missing 丢失、Unavailable 不可用、
/// Invalid 内容无效。序列化为小写形式。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LinkStatus {
    Present,
    Missing,
    Unavailable,
    Invalid,
}
/// link_cache 本地记录的结构化正文：链接内容 + 位置 + 最近观察状态与时间。
/// 用于链接文件丢失时凭缓存恢复，以及展示链接清单。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkRecord {
    /// 链接文件的完整结构化正文（身份与创建时间）。
    pub link: VeilLink,
    /// 链接文件在本机的绝对路径（缓存 key 相同）。
    pub link_path: PathBuf,
    /// 最近一次观察到的链接状态。
    pub seen_status: LinkStatus,
    /// 最近观察时间（RFC 3339）。
    pub last_seen_at: String,
}
/// 解析完成的容器：选择器（ID/名称/目录/链接）经过校验后得到的容器事实集合。
#[derive(Debug, Clone)]
pub struct ResolvedVeil {
    /// 容器展示名称。
    pub veil_name: String,
    /// 容器稳定 ID。
    pub veil_id: String,
    /// 容器目录的绝对路径（内含 veil.db 与 .enc 密文）。
    pub veil_dir: PathBuf,
    /// 该容器已登记的链接文件路径（若存在）。
    pub link_path: Option<PathBuf>,
    /// 本次解析是否从本机缓存恢复了丢失的链接文件。
    pub recovered_link: bool,
}
/// 本机配置组合门面：directory 保存数据源位置，work/veils/links/user_settings 是加载时的展示快照。
/// 用于命令和客户端选择空间、定位容器及管理链接；写操作访问数据库但多数不自动刷新这些公开视图。
#[derive(Clone, Debug)]
pub struct AppConfig {
    /// app.db 所在目录（也是容器默认存放的上级目录之一）。
    directory: PathBuf,
    /// 空间（工作区）视图。
    pub work: WorkSection,
    /// 已登记容器，key 为容器 ID。
    pub veils: HashMap<String, VeilConfig>,
    /// 链接缓存记录列表。
    pub links: Vec<LinkRecord>,
    /// 用户偏好。
    pub user_settings: UserSettings,
}
/// div_state 身份缓存里实际使用的字段子集：只关心容器原始创建时间。
#[derive(Serialize, Deserialize)]
struct IdentityCache {
    /// 解锁登记时保存的容器原始创建时间文本，用于随后创建链接，不是缓存更新时间。
    created_at: String,
}
impl AppConfig {
    /// 功能：定位当前进程应使用的本机应用数据库文件。
    /// 入参：无，应用根目录由 storage::app 的平台注入、环境变量或系统目录策略决定。
    /// 返回值：Ok(PathBuf)，为应用根目录下固定文件 app.db。
    /// 异常：应用目录定位或注入路径校验失败传播 VeilError。
    /// 注意事项：这里只拼接路径，不打开或创建数据库；文件路径不是旧版 JSONC 配置位置。
    pub fn config_path() -> Result<PathBuf> {
        Ok(crate::storage::app::app_data_dir()?.join(veil_core::database::APP_DB))
    }
    /// 功能：加载本机登记、空间和提示偏好的内存视图，供命令与界面组织后续业务。
    /// 入参：无，从 config_path 得到当前应用库位置。
    /// 返回值：Ok(AppConfig)，为 load_from 当前数据库产生的视图快照。
    /// 异常：定位、路径、数据库打开、设置/缓存解析或时间转换失败传播 VeilError。
    /// 注意事项：加载会打开可初始化的应用库，并可能确保默认空间存在，不是绝对只读；之后数据库变化不会自动刷新已返回视图。
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::config_path()?)
    }
    /// 功能：从指定本机 SQLite 应用库组装空间视图、容器展示缓存、提示设置和链接正文缓存。
    /// 入参：path: &Path，最终文件名必须精确为 app.db，可为相对或尚未存在的路径，旧配置名称被拒绝。
    /// 返回值：Ok(AppConfig)，保存规范化的库目录；只把已绑定本机路径的空间放入 work，登记按容器 ID 放入 veils。
    /// 异常：文件名/目录/提示级别非法为格式错误；应用库打开、记录解析或时间转换失败传播相应 VeilError。
    /// 注意事项：可能创建应用库及默认空间；空间、登记和缓存分次查询，不承诺是一个跨表一致读事务。
    /// 视图不包含容器密码或文件密钥；本机名称是缓存，真正身份与权威名称仍须解锁容器库核对。
    pub fn load_from(path: &Path) -> Result<Self> {
        // 严格限定文件名：旧 JSONC 配置（如 config.json）会被这里直接拒绝。
        if path.file_name().and_then(|s| s.to_str()) != Some("app.db") {
            return Err(invalid("仅支持 app.db，不读取旧配置"));
        }
        // 保存规范化数据库父目录，作为本实例的数据源；需要相对配置解释的工作区路径以它为基准。
        let directory = crate::storage::canonical_path(path)?
            .parent()
            .ok_or_else(|| invalid("数据库目录无效"))?
            .to_path_buf();
        let mut store = AppStore::open(&directory)?;
        // 默认空间 + 全部空间：逐条转成 WorkConfig 视图（跳过未绑定本机目录的空间）。
        let default = store.default_space()?;
        let spaces = store.spaces()?;
        let mut work = WorkSection::default();
        // 遍历空间登记：每条对应 app.db space_info 的一行。
        for space in spaces {
            // 未绑定本机目录的空间（未来跨设备同步来的空间）暂不进入工作区视图。
            if let Some(path) = space.local_path {
                let w = WorkConfig {
                    path,
                    work_type: if space.storage_mode == "dedicated" {
                        WorkType::Dedicated
                    } else if space.id == default.id {
                        WorkType::Default
                    } else {
                        WorkType::Custom(space.name.clone())
                    },
                    description: space.description,
                    created_at: crate::storage::container::timestamp(space.created_at)?,
                };
                // 默认空间单独存放，其余按空间 ID 进 custom 集合。
                if space.id == default.id {
                    work.default = Some(w);
                } else {
                    work.custom.insert(space.id, w);
                }
            }
        }
        // 容器登记（space_div）：逐条组装展示缓存；key 是容器 ID。
        let registrations = store.registrations()?;
        let mut veils = HashMap::new();
        // 遍历登记记录：每条代表本机已知的一个容器入口。
        for r in registrations {
            veils.insert(
                r.div_id.clone(),
                VeilConfig {
                    veil_id: r.div_id.clone(),
                    veil_name: r.cached_name,
                    // 创建时间来自 div_state 身份缓存（解锁后写入）；缓存不足时留空串。
                    created_at: store
                        .record::<IdentityCache>("div_state", &r.div_id)?
                        .map(|c| c.created_at)
                        .unwrap_or_default(),
                    last_accessed: None,
                    links: Vec::new(),
                },
            );
        }
        // 读取并校验提示级别：非法值直接报错，不静默回退默认值。
        let level = store.user_setting("hint_level")?;
        let hint_level = HintLevel::parse(level.as_str().unwrap_or(""))
            .ok_or_else(|| invalid("提示级别无效"))?;
        // 按最大 u32 条数读取链接缓存，实际仍受该数值与可用内存限制；每项是一份 LinkRecord 正文。
        let links = store.records("link_cache", u32::MAX)?;
        Ok(Self {
            directory,
            work,
            veils,
            links,
            user_settings: UserSettings { hint_level },
        })
    }
    /// 功能：为当前配置目录上的一次本机登记或设置操作取得存储访问对象。
    /// 入参：self: &AppConfig，为保存 directory 的配置门面。
    /// 返回值：Ok(AppStore)，绑定 directory 下的 app.db。
    /// 异常：打开、初始化或校验应用库失败传播 VeilError。
    /// 注意事项：每次返回独立访问包装，底层连接可能来自连接池，不能说每次都新建并关闭原生连接；不会自动重载 self 的视图字段。
    fn store(&self) -> Result<AppStore> {
        AppStore::open(&self.directory)
    }
    /// 功能：提供此配置视图对应的数据库路径，供显式重载和诊断使用。
    /// 入参：self: &AppConfig，包含加载时保存的库目录。
    /// 返回值：Ok(PathBuf)，directory 拼接 app.db。
    /// 异常：当前实现只拼路径，没有主动 Err 分支；保留 Result 形式供接口兼容。
    /// 注意事项：不检查文件仍存在、不打开数据库，也不重新执行全局目录定位策略。
    pub fn source_path(&self) -> Result<PathBuf> {
        Ok(self.directory.join("app.db"))
    }
    /// 功能：提供此配置实例的应用库所在目录，供相对工作区路径解析。
    /// 入参：self: &AppConfig。
    /// 返回值：Ok(PathBuf)，为保存的 directory 副本。
    /// 异常：当前实现没有主动错误分支。
    /// 注意事项：是路径快照，不创建目录、不验证可写性，也不返回某个容器目录。
    pub fn config_dir(&self) -> Result<PathBuf> {
        Ok(self.directory.clone())
    }
    /// 功能：为旧调用入口提供配置加载兼容行为，统一使用当前进程的默认应用库。
    /// 入参：_location: &Path 为兼容保留参数，当前实现完全忽略。
    /// 返回值：与 AppConfig::load 相同的本机配置视图。
    /// 异常：传播默认配置加载路径上的 VeilError。
    /// 注意事项：不会根据传入位置切换配置，调用方不得依赖它读取该目录下的 app.db。
    pub fn for_location(_location: &Path) -> Result<Self> {
        Self::load()
    }
    /// 功能：确保当前应用库里能找到或初始化默认空间，满足首次使用的前置要求。
    /// 入参：self: &mut AppConfig，使用它保存的库目录打开存储。
    /// 返回值：default_space 成功后返回 Ok(())。
    /// 异常：应用库打开、默认空间查找或初始化失败传播 VeilError。
    /// 注意事项：可能写入空间与默认引用，但不刷新 self.work；需要最新视图时应显式重载。
    pub fn ensure_default_work(&mut self) -> Result<()> {
        self.store()?.default_space()?;
        Ok(())
    }
    /// 功能：为创建容器选择并必要时登记空间，返回可用目录配置。
    /// 入参：_location: &Path 当前忽略；name: `Option<&str>` 为空间 ID/名称，default 或 None 指向默认空间；
    /// root: `Option<&Path>` 为显式目录，优先于 name；dedicated: bool 决定新建目录空间是否专属。
    /// 返回值：Ok((AppConfig,WorkConfig))，配置在选定空间后重载，工作区含本机绑定路径及保存模式。
    /// 异常：目录创建/规范化、空间查询/创建、名称歧义、未绑定路径、时间转换或重载失败传播 VeilError。
    /// 注意事项：显式 root 已登记且 dedicated=true 时拒绝共享空间；按 name 或默认空间选择的分支不额外强制专属模式。
    /// 创建目录或空间后后续步骤仍可失败，不提供整体回滚；最终 WorkType 对非专属选择返回 Custom，包括默认选择。
    pub fn creation_work(
        _location: &Path,
        name: Option<&str>,
        root: Option<&Path>,
        dedicated: bool,
    ) -> Result<(Self, WorkConfig)> {
        let mut config = Self::load()?;
        let mut store = config.store()?;
        // 分支 1：用户显式给了目录——目录不存在先创建；已登记为空间则复用，
        // 否则按目录名新建空间。专属模式要求空间必须是 dedicated 类型。
        let space = if let Some(root) = root {
            let root = std::path::absolute(root)?;
            crate::fsutil::create_dir_all_durable(&root)?;
            // 规范化路径再比较：避免相对路径/别名把同一目录重复登记成多个空间。
            let canonical = std::fs::canonicalize(&root)?;
            match store
                .spaces()?
                .into_iter()
                .find(|s| s.local_path.as_ref() == Some(&canonical))
            {
                Some(s) => {
                    if dedicated && s.storage_mode != "dedicated" {
                        return Err(invalid("共享空间不能作为专属空间"));
                    }
                    s
                }
                None => store.create_space(
                    root.file_name().and_then(|s| s.to_str()).unwrap_or("空间"),
                    Some(&root),
                    dedicated,
                )?,
            }
        // 分支 2：按名称找空间（"default" 是保留字，转成默认空间）。
        } else if let Some(name) = name.filter(|n| *n != "default") {
            find_space(&store.spaces()?, name)?
        // 分支 3：未指定任何信息，使用默认空间。
        } else {
            store.default_space()?
        };
        let w = WorkConfig {
            path: space
                .local_path
                .ok_or_else(|| invalid("空间尚未绑定本机目录"))?,
            work_type: if space.storage_mode == "dedicated" {
                WorkType::Dedicated
            } else {
                WorkType::Custom(space.name)
            },
            description: space.description,
            created_at: crate::storage::container::timestamp(space.created_at)?,
        };
        // 空间可能刚新建：从 app.db 重载一次，确保视图与磁盘一致。
        config = Self::load_from(&config.source_path()?)?;
        Ok((config, w))
    }
    /// 功能：把指定目录登记为可放多个容器的共享空间，供后续在该位置创建或查找容器。
    /// 入参：self: &mut AppConfig；name: String 为空间名；path: PathBuf 为位置，相对路径以本配置目录为基准，绝对路径覆盖该基准；
    /// _description: `Option<String>` 当前忽略，不持久化传入说明。
    /// 返回值：create_space 成功后为 Ok(())。
    /// 异常：空间名、路径校验或数据库/文件系统登记失败传播 VeilError。
    /// 注意事项：只委托存储层创建共享空间，不刷新 self.work 等内存视图；调用方不要把传入 description 当作已经保存。
    pub fn register_custom_work(
        &mut self,
        name: String,
        path: PathBuf,
        _description: Option<String>,
    ) -> Result<()> {
        self.store()?
            .create_space(&name, Some(&self.directory.join(path)), false)?;
        Ok(())
    }
    /// 功能：将选定工作区的位置解释成容器创建和定位可使用的绝对目录。
    /// 入参：self: &AppConfig 提供配置基准目录；work: &WorkConfig 提供绝对或相对的工作区路径。
    /// 返回值：Ok(PathBuf)，为 WorkConfig::resolve_root 的路径结果。
    /// 异常：工作区路径语法或配置目录不合规返回 WorkError。
    /// 注意事项：只拼接路径，不消除所有 ..、不解析符号链接，也不验证目录存在或权限。
    pub fn resolve_work_root(&self, work: &WorkConfig) -> Result<PathBuf> {
        work.resolve_root(&self.directory)
    }
    /// 功能：按目录位置复用已有空间或登记一个新空间，供创建/导入流程取得工作区视图。
    /// 入参：self: &mut AppConfig；path: &Path 为目录，会先创建并规范化；dedicated: bool 仅用于未登记时的新空间模式。
    /// 返回值：Ok(WorkConfig)，包含规范化目录及实际保存的共享/专属模式、说明和创建时间。
    /// 异常：目录创建/规范化、应用库、空间创建或时间转换失败传播 VeilError。
    /// 注意事项：已登记同路径直接复用，不按 dedicated 改模式或拒绝模式差异；不会重载 self.work。
    /// 目录创建与本机登记不是一个可整体回滚的事务，失败后目录可能仍存在。
    pub fn register_work_at(&mut self, path: &Path, dedicated: bool) -> Result<WorkConfig> {
        crate::fsutil::create_dir_all_durable(path)?;
        // 规范化后比较：同一实际目录不会登记成两个空间。
        let path = std::fs::canonicalize(path)?;
        let mut store = self.store()?;
        let space = match store
            .spaces()?
            .into_iter()
            .find(|s| s.local_path.as_ref() == Some(&path))
        {
            Some(s) => s,
            None => store.create_space(
                path.file_name().and_then(|p| p.to_str()).unwrap_or("空间"),
                Some(&path),
                dedicated,
            )?,
        };
        Ok(WorkConfig {
            path,
            work_type: if space.storage_mode == "dedicated" {
                WorkType::Dedicated
            } else {
                WorkType::Custom(space.name)
            },
            description: space.description,
            created_at: crate::storage::container::timestamp(space.created_at)?,
        })
    }
    /// 功能：创建或导入前检查容器身份是否已在本机登记或已知空间下被占用。
    /// 入参：self: &AppConfig；identity: &str 为计划使用的容器 ID，须能作为单个安全目录段。
    /// 返回值：格式通过且未发现登记或同名路径时 Ok(())。
    /// 异常：身份非法为 InvalidFormat；已登记或空间下路径存在为 VeilIdConflict；存储访问失败传播 VeilError。
    /// 注意事项：只扫描已登记并绑定目录的空间，exists 的结果也不是原子预留；后续创建仍须生命周期锁和排他发布协议避免竞争。
    pub fn ensure_veil_id_available(&self, identity: &str) -> Result<()> {
        crate::work::validate_veil_id(identity)?;
        // 登记表冲突：同 ID 已在本机登记过。
        if self
            .store()?
            .registrations()?
            .iter()
            .any(|r| r.div_id == identity)
        {
            return Err(VeilError::VeilIdConflict(format!("容器 {identity} 已登记")));
        }
        // 磁盘冲突：任何空间目录下已存在同名容器目录（可能来自其他设备的副本）。
        for s in self.store()?.spaces()? {
            if s.local_path.is_some_and(|p| p.join(identity).exists()) {
                return Err(VeilError::VeilIdConflict("发现相同 ID 的实际副本".into()));
            }
        }
        Ok(())
    }
    /// 功能：在创建、登记等业务期间阻止遵守同协议的删除流程独占该容器生命周期。
    /// 入参：self: &AppConfig；identity: &str 为稳定容器 ID，锁文件位于应用数据目录的独立命名空间。
    /// 返回值：Ok(File)，为已取得共享锁的句柄，持有至本次操作无需再保护容器为止。
    /// 异常：应用库、身份、锁文件或锁竞争失败传播 VeilError。
    /// 注意事项：关闭句柄才释放本地文件锁；不是容器库 operation_locks 中的文件内容租约，也不会阻止不遵守协议的外部删除。
    pub(crate) fn container_lifecycle_lock(&self, identity: &str) -> Result<std::fs::File> {
        self.store()?.acquire_container_shared_lock(identity)
    }

    /// 功能：把已核对身份的容器目录与本机空间关联，更新展示名、身份时间缓存及最近访问入口。
    /// 入参：self: &mut AppConfig；directory: &Path 为存在的容器目录；identity/name/created: &str 分别为调用方解锁后获得的容器 ID、权威名称、原始创建时间。
    /// 返回值：空间关联登记、身份缓存和最近记录均成功时为 Ok(())。
    /// 异常：目录名不等于 ID、父级/空间引用非法、同 ID 多实际副本或登记位置变化返回校验/身份冲突；文件系统与数据库失败传播 VeilError。
    /// 注意事项：先持容器生命周期共享锁再规范化和检查；不自行打开加密容器库，真实身份由调用方保证。
    /// 空间补登、关联和缓存分多次持久化，后续失败不承诺整体回滚；self.work/veils/links 不自动刷新。
    pub fn register_directory(
        &mut self,
        directory: &Path,
        identity: &str,
        name: &str,
        created: &str,
    ) -> Result<()> {
        // 生命周期锁：登记期间防止该容器被删除流程并发移除。
        let _container_lock = self.container_lifecycle_lock(identity)?;
        let directory = std::fs::canonicalize(directory)?;
        // 目录名必须等于容器 ID——这是本机定位容器的约定（<space>/<veil_id>）。
        if directory.file_name().and_then(|n| n.to_str()) != Some(identity) {
            return Err(invalid("容器目录名称必须与真实 ID 一致"));
        }
        // parent 是所属空间目录。
        let parent = directory
            .parent()
            .ok_or_else(|| invalid("容器缺少空间目录"))?;
        let mut store = self.store()?;
        // 查询是否已有该 ID 的登记（重新登记场景）。
        let existing = store
            .registrations()?
            .into_iter()
            .find(|r| r.div_id == identity);
        let spaces = store.spaces()?;
        // 防多副本：任何空间下若已存在同名目录但规范化路径不同，说明同一 ID 有
        // 多个实际副本，必须报冲突而不是随便选一个。
        for s in &spaces {
            if let Some(root) = &s.local_path {
                let candidate = root.join(identity);
                if candidate.is_dir() && std::fs::canonicalize(&candidate)? != directory {
                    return Err(VeilError::VeilIdConflict("同 ID 存在多个实际副本".into()));
                }
            }
        }
        // 已有登记：新目录必须仍位于原登记的空间目录下，否则视为位置漂移。
        if let Some(existing) = existing {
            let s = spaces
                .iter()
                .find(|s| s.id == existing.space_id)
                .ok_or_else(|| invalid("容器空间引用无效"))?;
            if s.local_path.as_deref() != Some(parent) {
                return Err(VeilError::VeilIdConflict(
                    "容器登记位置不同；请先处理原登记".into(),
                ));
            }
        }
        // 父目录尚未登记为空间时，自动按目录名补登一个共享空间。
        let space = match spaces
            .into_iter()
            .find(|s| s.local_path.as_deref() == Some(parent))
        {
            Some(s) => s,
            None => store.create_space(
                parent
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("空间"),
                Some(parent),
                false,
            )?,
        };
        // 分别提交空间关联、身份创建时间缓存与最近访问记录；后一步失败不会自动撤销前面已完成写入。
        store.register(&space.id, identity, name)?;
        store.cache_container_identity(identity, created)?;
        store.set_record(
            "recent_div",
            identity,
            &crate::storage::app::RecentContainer {
                id: identity.into(),
                name: name.into(),
                path: directory.to_string_lossy().into_owned(),
            },
        )?;
        Ok(())
    }
    /// 功能：为免解锁创建链接提供上次保存的容器原始创建时间。
    /// 入参：self: &AppConfig；identity: &str 为 div_state 本机记录键。
    /// 返回值：Ok(String)，为缓存字段 created_at；不是当前读取容器库所得权威时间。
    /// 异常：缓存缺失返回 InvalidFormat，提示先解锁；存储或反序列化失败传播 VeilError。
    /// 注意事项：不检查缓存时间的 RFC 3339 格式，也不验证当前容器仍在原位置；身份核验和缓存刷新由解锁登记流程完成。
    pub fn cached_identity(&self, identity: &str) -> Result<String> {
        self.store()?
            .record::<IdentityCache>("div_state", identity)?
            .map(|c| c.created_at)
            .ok_or_else(|| invalid("本机缓存不足，请先解锁容器"))
    }
    /// 功能：从已经加载的内存展示缓存中找出一个匹配 ID 或名称的容器键。
    /// 入参：self: &AppConfig；input: &str 为精确匹配的容器 ID 或展示名称，不做大小写或空白归一化。
    /// 返回值：任一首次匹配项的 Some(String) ID，找不到为 None。
    /// 异常：只遍历 HashMap，不返回 Result。
    /// 注意事项：不会拒绝重名歧义，也不优先精确 ID；HashMap 顺序不稳定，多匹配时结果不确定。需唯一定位应走 find_container/resolve_veil。
    pub fn find_veil_key(&self, input: &str) -> Option<String> {
        self.veils
            .values()
            .find(|v| v.veil_id == input || v.veil_name == input)
            .map(|v| v.veil_id.clone())
    }
    /// 功能：按本机唯一容器选择器取得实际登记目录，供链接创建等业务使用。
    /// 入参：self: &AppConfig；input: &str 为容器 ID 或缓存展示名。
    /// 返回值：Ok(PathBuf)，来自 find_container 的 veil_dir。
    /// 异常：未找到、名称歧义、空间未绑定、目录不可用、同 ID 多副本或存储/路径失败传播 VeilError。
    /// 注意事项：不解锁数据库；仍可能更新“观察不可用”本机状态，结果只反映此次定位。
    pub fn get_veil_dir(&self, input: &str) -> Result<PathBuf> {
        Ok(self.find_container(input)?.veil_dir)
    }
    /// 功能：用本机登记唯一定位容器，并拒绝不可访问空间或同 ID 多副本，保护后续操作不指向错误位置。
    /// 入参：self: &AppConfig；input: &str，与每条登记的 div_id 或 cached_name 精确比较。
    /// 返回值：唯一匹配时 Ok(ResolvedVeil)，link_path=None、recovered_link=false，目录由空间根加容器 ID 得到。
    /// 异常：匹配零条为 VeilNotFound、多条为 VeilNameAmbiguous；空间引用非法、未绑定/库文件缺失、重复实际副本或数据库/I/O 错误也会失败。
    /// 注意事项：不优先 ID 匹配，不读取加密库核验内容；遇到不可用匹配会直接返回，不等收集完全部重名项。
    /// 检查分多次读取且不持长事务；缺库时尽力写 observe_unavailable，写观察失败不会替代原不可用结果。
    fn find_container(&self, input: &str) -> Result<ResolvedVeil> {
        let store = self.store()?;
        let spaces = store.spaces()?;
        // 收集所有匹配（按 ID 或缓存名）的登记。
        // matches 暂存全部可成功定位的匹配入口；每项对应一个容器登记，用数量判断唯一性，
        // 并不按 ID 精确匹配优先于同名匹配排序。
        let mut matches = Vec::new();
        // 遍历登记：每条代表一个本机已知容器入口。
        for r in store.registrations()? {
            if r.div_id != input && r.cached_name != input {
                continue;
            }
            let space = spaces
                .iter()
                .find(|s| s.id == r.space_id)
                .ok_or_else(|| invalid("空间引用无效"))?;
            // 空间未绑定本机目录时无法定位容器文件。
            let root = space
                .local_path
                .as_ref()
                .ok_or_else(|| VeilError::VolumeUnavailable("空间尚未绑定目录".into()))?;
            let directory = root.join(&r.div_id);
            // 容器数据库文件不存在：目录当前不可用（如移动硬盘未挂载），
            // 顺手把观察状态写回，供界面提示而不是误判为已删除。
            if !directory.join(veil_core::database::CONTAINER_DB).is_file() {
                let _ = store.observe_unavailable(&r.div_id);
                return Err(VeilError::VolumeUnavailable("容器目录当前不可用".into()));
            }
            // 复查所有空间下的同名目录：任何其他位置出现同 ID 实际副本都拒绝。
            for s in &spaces {
                if let Some(other) = &s.local_path {
                    let candidate = other.join(&r.div_id);
                    if candidate.is_dir()
                        && std::fs::canonicalize(candidate)? != std::fs::canonicalize(&directory)?
                    {
                        return Err(VeilError::VeilIdConflict("同 ID 存在多个实际副本".into()));
                    }
                }
            }
            matches.push(ResolvedVeil {
                veil_id: r.div_id,
                veil_name: r.cached_name,
                veil_dir: directory,
                link_path: None,
                recovered_link: false,
            });
        }
        // 名称匹配到多个容器：要求用户用 ID 或目录消除歧义。
        if matches.len() > 1 {
            return Err(VeilError::VeilNameAmbiguous(input.into()));
        }
        matches
            .pop()
            .ok_or_else(|| VeilError::VeilNotFound(input.into()))
    }
    /// 功能：核对用户给定目录是否就是本机登记的同一容器位置，避免目录漂移或副本混淆。
    /// 入参：self: &AppConfig；path: &Path 为现有目录路径，会解析符号链接并规范化。
    /// 返回值：目录名查到的唯一登记与实际路径相同后返回 Ok(ResolvedVeil)。
    /// 异常：路径不可解析、目录名非 UTF-8、登记查找失败或路径不一致时返回相应 VeilError。
    /// 注意事项：依赖已有本机登记，不会自动将陌生容器导入登记；不解密或验证 veil.db 内身份。
    pub fn resolve_directory(&self, path: &Path) -> Result<ResolvedVeil> {
        let path = std::fs::canonicalize(path)?;
        // 目录名即容器稳定 ID。
        let identity = path
            .file_name()
            .and_then(|p| p.to_str())
            .ok_or_else(|| invalid("目录名称无效"))?;
        let resolved = self.find_container(identity)?;
        // 登记目录与实际目录不一致时拒绝：避免把新副本当成已登记容器打开。
        if std::fs::canonicalize(&resolved.veil_dir)? != path {
            return Err(VeilError::VeilIdConflict(
                "登记目录与实际目录不一致，请先解锁登记".into(),
            ));
        }
        Ok(resolved)
    }
    /// 功能：统一解释链接、目录或 ID/名称选择器，必要时依据本机缓存恢复丢失链接。
    /// 入参：self: &mut AppConfig；input: &str 为用户选择文本，扩展名精确为 veil-link 时按链接解释，现有目录次之，其余按 ID/名称查找。
    /// 返回值：Ok(ResolvedVeil)，链接分支附绝对 link_path；走缓存恢复分支时 recovered_link=true。
    /// 异常：路径/链接内容、缓存、容器定位或链接创建失败传播 VeilError；恢复位置被另一容器链接占用为 VeilIdConflict。
    /// 注意事项：解析会刷新链接缓存或写回缺失链接，不是纯查找；恢复使用禁止覆盖写入，若被并发占用仍重新读链接并比对 ID。
    /// recovered_link 表示使用缓存恢复路径，即使并发写者已生成同 ID 链接也可能为 true，不证明一定由本次创建文件。
    pub fn resolve_veil(&mut self, input: &str) -> Result<ResolvedVeil> {
        let path = Path::new(input);
        // 分支 1：输入是 .veil-link 文件路径。
        if path.extension().and_then(|p| p.to_str()) == Some(LINK_EXTENSION) {
            let absolute = crate::storage::canonical_path(path)?;
            // 文件还在：直接读并刷新缓存；文件丢失：从本机 link_cache 恢复正文。
            let (link, recovered) = if path.exists() {
                (self.cache_link(path)?, false)
            } else {
                let record: LinkRecord = self
                    .store()?
                    .record(
                        "link_cache",
                        absolute.to_str().ok_or_else(|| invalid("链接路径无效"))?,
                    )?
                    .ok_or_else(|| VeilError::VeilNotFound(input.into()))?;
                (record.link, true)
            };
            // 链接只是入口：真正的容器位置仍按 veil_id 从登记中解析。
            let mut resolved = self.find_container(&link.veil.veil_id)?;
            if recovered {
                // 恢复：把缓存正文重新写到原路径（原位置已被别人占用则不覆盖）。
                match link.save_new(&absolute) {
                    Ok(()) => {}
                    Err(VeilError::Io(error))
                        if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                // 写回后立刻复核该位置的链接属于同一个容器，防止并发占用。
                let observed = self.cache_link(&absolute)?;
                if observed.veil.veil_id != link.veil.veil_id {
                    return Err(VeilError::VeilIdConflict(
                        "链接位置已被另一个入口占用".into(),
                    ));
                }
            }
            resolved.link_path = Some(absolute);
            resolved.recovered_link = recovered;
            return Ok(resolved);
        }
        // 分支 2：输入是实际存在的目录。
        if path.is_dir() {
            return self.resolve_directory(path);
        }
        // 分支 3：按 ID 或展示名查登记。
        self.find_container(input)
    }
    /// 功能：为创建链接的调用方沿用统一容器选择器解析与恢复规则。
    /// 入参：self: &mut AppConfig；input: &str 为 ID、名称、现有目录或 veil-link 路径。
    /// 返回值：resolve_veil 成功的 ResolvedVeil，含容器位置和可选链接恢复信息。
    /// 异常：原样传播 resolve_veil 的格式、定位、缓存和文件系统错误。
    /// 注意事项：是兼容转发，不增加额外校验；解析可能写链接文件或缓存，不应当成只读字符串转换。
    pub fn resolve_link_target(&mut self, input: &str) -> Result<ResolvedVeil> {
        self.resolve_veil(input)
    }
    /// 功能：为本机可唯一定位的容器创建一个新的链接入口并登记其正文缓存。
    /// 入参：self: &mut AppConfig；identity: &str 为容器选择键，通常传稳定 ID；name: &str 为写入链接的展示名；path: &Path 为新链接位置。
    /// 返回值：定位及 register_link_at 完成时 Ok(())。
    /// 异常：容器不可用/歧义、创建时间缓存不足、已有目标、写入或缓存失败传播 VeilError。
    /// 注意事项：实际定位仍走 get_veil_dir；名称由调用方传入，并不在此从加密库读取；更新后内存 links 不自动补项。
    pub fn register_link(&mut self, identity: &str, name: &str, path: &Path) -> Result<()> {
        let directory = self.get_veil_dir(identity)?;
        self.register_link_at(identity, name, &directory, path)
    }
    /// 功能：依据已知容器身份和本机创建时间缓存，在指定位置创建禁止覆盖的新链接并缓存正文。
    /// 入参：self: &mut AppConfig；identity/name: &str 为容器 ID 与展示名；_directory: &Path 当前忽略，不参与位置核验；path: &Path 为输出链接路径。
    /// 返回值：链接写入并成功缓存后 Ok(())。
    /// 异常：创建时间缓存缺失、内容非法、目标已存在、文件写入/同步或缓存存储失败传播 VeilError。
    /// 注意事项：缓存失败后仅尽力 remove_file，新链接可能因清理失败残留；save_new 发布后同步失败也可能已有文件。
    /// 文件与 app.db 更新不构成一个原子事务；此方法不自动检查输出扩展名或将 self.links 重载。
    pub fn register_link_at(
        &mut self,
        identity: &str,
        name: &str,
        _directory: &Path,
        path: &Path,
    ) -> Result<()> {
        // 容器创建时间来自本机身份缓存（div_state），不需要解锁。
        let link = VeilLink::new(identity, name, self.cached_identity(identity)?);
        if path.exists() {
            return Err(invalid("链接已存在"));
        }
        link.save_new(path)?;
        // 缓存失败后尽力删除新链接以补偿；删除结果被忽略，文件仍可能残留，不能承诺跨资源原子性。
        if let Err(e) = self.cache_link(path) {
            let _ = std::fs::remove_file(path);
            return Err(e);
        }
        Ok(())
    }
    /// 功能：把成功读取的链接正文和当前观察时间保存到本机缓存，支持以后缺失恢复。
    /// 入参：self: &mut AppConfig；path: &Path 为存在的链接文件位置。
    /// 返回值：Ok(VeilLink) 为解析并验证的正文，数据库 link_cache 以规范化绝对 UTF-8 路径为键保存 LinkRecord。
    /// 异常：链接读取/格式/身份校验、路径规范化、非 UTF-8 缓存键或记录写入失败传播 VeilError。
    /// 注意事项：只写本机缓存，不验证容器密码或实际目录；读取文件与落库之间并非文件系统快照，链接可被外部修改。
    /// 只更新数据库，不刷新 self.links 视图。
    pub fn cache_link(&mut self, path: &Path) -> Result<VeilLink> {
        let link = VeilLink::load(path)?;
        let absolute = crate::storage::canonical_path(path)?;
        self.store()?.set_record(
            "link_cache",
            absolute.to_str().ok_or_else(|| invalid("链接路径无效"))?,
            &LinkRecord {
                link: link.clone(),
                link_path: absolute.clone(),
                seen_status: LinkStatus::Present,
                last_seen_at: chrono::Utc::now().to_rfc3339(),
            },
        )?;
        Ok(link)
    }
    /// 功能：从一个链接入口获得其当前登记容器目录，沿用链接缓存恢复逻辑。
    /// 入参：self: &mut AppConfig；path: &Path 为要交给统一选择器的入口路径，须可转为 UTF-8。
    /// 返回值：Ok(PathBuf)，为 resolve_veil 结果的 veil_dir。
    /// 异常：路径无法转字符串、选择器解析、缓存恢复或容器定位失败传播 VeilError。
    /// 注意事项：方法名并不强制路径扩展名，实际仍按 resolve_veil 规则分支；可能写回缺失链接或刷新缓存。
    pub fn resolve_link_veil_dir(&mut self, path: &Path) -> Result<PathBuf> {
        Ok(self
            .resolve_veil(path.to_str().ok_or_else(|| invalid("路径无效"))?)?
            .veil_dir)
    }
}
/// 功能：在空间记录集合中按 ID 或名称定位唯一一条空间，供创建容器选择空间使用。
/// 入参：spaces: &[Space] 为本机空间快照，每项是一条空间登记；name: &str 为精确匹配 ID 或名称的选择器。
/// 返回值：恰好一条匹配时返回 Ok(Space) 副本。
/// 异常：零条或多条均返回 InvalidFormat，统一说明不存在或名称不唯一。
/// 注意事项：ID 不比名称优先，未绑定目录的空间仍可匹配；是否可使用该目录由后续流程检查。
fn find_space(spaces: &[Space], name: &str) -> Result<Space> {
    let found = spaces
        .iter()
        .filter(|s| s.id == name || s.name == name)
        .cloned()
        .collect::<Vec<_>>();
    if found.len() != 1 {
        return Err(invalid("空间不存在或名称不唯一"));
    }
    Ok(found[0].clone())
}

/// 设置页展示用的配置摘要：空间列表、默认空间、容器数与链接数。
#[derive(Serialize)]
pub struct ConfigSummary {
    /// 空间及其容器列表的展示视图。
    pub spaces: Vec<crate::spaces::SpaceView>,
    /// 默认空间 ID。
    pub default_space_id: String,
    /// 已登记容器总数。
    pub container_count: usize,
    /// 链接缓存（.veil-link）总数。
    pub link_count: usize,
}
/// 功能：组装设置页需要的空间列表、默认引用和容器/链接缓存数量。
/// 入参：无，使用当前全局应用库和 spaces 展示入口。
/// 返回值：Ok(ConfigSummary)，含 spaces、default_space_id、登记数及 link_cache 条数。
/// 异常：应用库打开、默认空间初始化、空间展示或记录读取/解析失败传播 VeilError。
/// 注意事项：分次查询并可能初始化默认空间，不保证列表与计数来自同一个事务快照；链接数是缓存数量，不保证每个文件仍存在。
pub(crate) fn summary() -> Result<ConfigSummary> {
    let mut store = AppStore::global()?;
    let default = store.default_space()?;
    Ok(ConfigSummary {
        spaces: crate::spaces::list_spaces()?,
        default_space_id: default.id,
        container_count: store.registrations()?.len(),
        link_count: store.records::<LinkRecord>("link_cache", u32::MAX)?.len(),
    })
}
