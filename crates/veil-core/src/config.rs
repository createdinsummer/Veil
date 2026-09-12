//! 全局配置与容器/链接解析。
//!
//! 配置文件位置：~/.veil/config.toml
//!
//! 配置同时维护工作区、容器、卷和链接索引，并保存每个 `.veil-link` 的原始字节副本：
//! - 链接被删除时，可以按字节原样恢复；
//! - 容器与工作区映射始终以稳定 `veil_id` 作为身份依据。

use crate::error::VeilError;
use crate::link::{LINK_EXTENSION, VeilLink};
use crate::volume::{self, VolumeInfo};
use crate::workspace::WorkspaceConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// `~/.veil/config.toml` 的根配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalConfig {
    /// 配置格式版本。
    #[serde(default = "default_version")]
    pub version: String,

    /// 系统状态和一次性提示记录。
    #[serde(default)]
    pub system: SystemConfig,

    /// 默认工作区和自定义工作区。
    #[serde(default)]
    pub workspace: WorkspaceSection,

    /// 以配置键索引的容器注册表。
    #[serde(default)]
    pub containers: HashMap<String, ContainerConfig>,

    /// 以稳定卷 ID 索引的磁盘卷缓存。
    #[serde(default)]
    pub volumes: HashMap<String, VolumeRecord>,

    /// 已知 `.veil-link` 的完整原始字节副本。
    ///
    /// 这是对实际链接文件的字节级镜像，而不只是路径索引；链接丢失后可据此恢复。
    #[serde(default)]
    pub links: Vec<LinkRecord>,

    /// 用户偏好设置。
    #[serde(default)]
    pub preferences: PreferencesConfig,

    /// 新容器使用的默认加密参数。
    #[serde(default)]
    pub encryption: EncryptionConfig,
}

impl Default for GlobalConfig {
    /// 创建版本、空索引和默认偏好组成的配置。
    fn default() -> Self {
        Self {
            version: default_version(),
            system: SystemConfig::default(),
            workspace: WorkspaceSection::default(),
            containers: HashMap::new(),
            volumes: HashMap::new(),
            links: Vec::new(),
            preferences: PreferencesConfig::default(),
            encryption: EncryptionConfig::default(),
        }
    }
}

/// 返回当前配置格式版本。
fn default_version() -> String {
    "1.0".to_string()
}

/// 与容器数据无关的系统状态和提示展示记录。
///
/// 图标与首次运行字段属于预留系统状态；当前 CLI 实际读写的是三个一次性提示标记。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemConfig {
    /// 预留的图标配置状态。
    #[serde(default)]
    pub icons_configured: bool,

    /// 预留的图标配置时间。
    pub icons_configured_at: Option<String>,

    /// 预留的图标版本。
    pub icons_version: Option<String>,

    /// 预留的首次运行标记。
    #[serde(default = "default_true")]
    pub first_run: bool,

    /// 首次创建容器提示是否已展示。
    #[serde(default)]
    pub init_hint_shown: bool,

    /// 首次打包提示是否已展示。
    #[serde(default)]
    pub pack_hint_shown: bool,

    /// 首次解包提示是否已展示。
    #[serde(default)]
    pub unpack_hint_shown: bool,

    /// 预留的文件类型歧义提示标记；当前逻辑不读取该值。
    #[serde(default)]
    pub extension_hint_shown: bool,
}

impl Default for SystemConfig {
    /// 创建未配置图标、启用首次运行且所有提示均未展示的默认状态。
    fn default() -> Self {
        Self {
            icons_configured: false,
            icons_configured_at: None,
            icons_version: None,
            first_run: true,
            init_hint_shown: false,
            pack_hint_shown: false,
            unpack_hint_shown: false,
            extension_hint_shown: false,
        }
    }
}

/// 作为 Serde 默认值返回 `true`。
fn default_true() -> bool {
    true
}

/// 配置文件中保存的工作区集合。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceSection {
    /// 默认工作区配置；首次初始化时可自动补全。
    pub default: Option<WorkspaceConfig>,

    /// 以工作区名称索引的自定义工作区。
    ///
    /// CLI 目前没有创建或注册自定义工作区的命令，调用方需在配置文件写入该映射后，
    /// 才能通过 `--workspace <名称>` 引用。
    #[serde(default)]
    pub custom: HashMap<String, WorkspaceConfig>,
}

/// 容器在工作区和链接中的注册信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerConfig {
    /// 容器稳定 ID。
    pub veil_id: String,

    /// 展示名称，不再作为唯一键。
    pub container_name: String,

    /// 共享工作区名称，值为 `default` 或 [`WorkspaceSection::custom`] 的键。
    pub workspace: Option<String>,

    /// 容器在共享工作区根目录下的目录名。
    pub container_dir: Option<String>,

    /// 无法仅靠命名工作区和 `container_dir` 还原时记录的最终容器根路径。
    ///
    /// 通过 `--workspace-path` 创建时会自动写入此字段；该路径不会新增到命名工作区表。
    pub workspace_path: Option<PathBuf>,

    /// 是否使用由该容器独占的工作区。
    #[serde(default)]
    pub dedicated: bool,

    /// RFC 3339 格式的注册创建时间。
    pub created_at: String,

    /// 预留的最后访问时间；当前初始化写入 `None`。
    pub last_accessed: Option<String>,

    /// 指向该容器的链接文件路径，可保存多个副本。
    #[serde(default)]
    pub links: Vec<PathBuf>,
}

/// 配置中缓存的磁盘卷记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeRecord {
    /// 稳定卷 ID。
    pub volume_id: String,
    /// 面向用户展示的卷名称。
    pub volume_label: String,
    /// 最近一次检测到的卷根路径。
    pub mount_path: PathBuf,
    /// 是否被识别为外部或可移动卷。
    pub is_external: bool,
    /// 最近一次检测到该卷的时间。
    pub last_seen_at: String,
}

/// `.veil-link` 在当前系统中的可解析状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LinkStatus {
    /// 链接文件存在且已成功解析。
    Present,
    /// 链接文件缺失，但配置中可能保存可恢复内容。
    Missing,
    /// 链接存在，但其目标卷当前不可用。
    Unavailable,
    /// 链接文件存在但无法解析。
    Invalid,
}

/// 已知 `.veil-link` 的索引和原始内容副本。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkRecord {
    /// 链接文件的绝对路径。
    pub link_path: PathBuf,
    /// 链接指向的稳定容器 ID。
    pub veil_id: String,
    /// 链接记录的容器展示名称。
    #[serde(default)]
    pub container_name: String,
    /// 链接记录的目标卷 ID。
    #[serde(default)]
    pub volume_id: String,
    /// 链接记录的目标卷展示名称。
    #[serde(default)]
    pub volume_label: String,
    /// 工作区相对于目标卷根的路径。
    #[serde(default)]
    pub relative_path: PathBuf,
    /// 最近一次观察到链接时的状态。
    pub status: LinkStatus,
    /// 对原始链接字节计算的 blake3 十六进制摘要。
    #[serde(default)]
    pub content_hash: String,
    /// 与 `.veil-link` 文件逐字节一致的十六进制副本。
    ///
    /// 用户删除链接后可直接写回这些字节完成原样恢复。
    #[serde(default)]
    pub raw_hex: String,
    /// 最近一次观察或恢复链接的时间。
    pub last_seen_at: String,
}

/// 用户偏好设置。
///
/// 当前运行路径读取提示级别；其余字段会被配置读写流程保留，但尚未参与行为分支。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreferencesConfig {
    /// 提示级别
    #[serde(default = "default_hints_level")]
    pub hints_level: HintsLevel,

    /// 自动同步
    #[serde(default)]
    pub auto_sync: bool,

    /// 默认密钥派生算法
    #[serde(default = "default_kdf")]
    pub default_key_derivation: String,

    /// 是否缓存密钥
    #[serde(default)]
    pub cache_keys: bool,

    /// 缓存超时（秒）
    #[serde(default = "default_cache_timeout")]
    pub cache_timeout_seconds: u64,

    /// 是否使用系统密钥链
    #[serde(default)]
    pub use_system_keychain: bool,

    /// 日志级别
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

impl Default for PreferencesConfig {
    /// 使用完整提示、禁用缓存与自动同步，并采用信息级日志。
    fn default() -> Self {
        Self {
            hints_level: HintsLevel::Full,
            auto_sync: false,
            default_key_derivation: "Argon2id".to_string(),
            cache_keys: false,
            cache_timeout_seconds: 300,
            use_system_keychain: false,
            log_level: "info".to_string(),
        }
    }
}

/// 返回提示级别的 Serde 默认值。
fn default_hints_level() -> HintsLevel {
    HintsLevel::Full
}

/// 返回默认密钥派生函数名称。
fn default_kdf() -> String {
    "Argon2id".to_string()
}

/// 返回密钥缓存默认超时秒数。
fn default_cache_timeout() -> u64 {
    300
}

/// 返回默认日志级别。
fn default_log_level() -> String {
    "info".to_string()
}

/// 一次性使用提示的展示级别。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HintsLevel {
    /// 展示全部提示。
    Full,
    /// 仅展示解包、链接恢复和文件类型歧义提示。
    Brief,
    /// 关闭提示。
    Off,
}

impl HintsLevel {
    /// 返回可写入配置的稳定小写字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Brief => "brief",
            Self::Off => "off",
        }
    }

    /// 解析配置值或环境变量，忽略首尾空白并忽略大小写。
    ///
    /// 无法识别时返回 `None`，由调用方决定回退策略。
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "full" => Some(Self::Full),
            "brief" => Some(Self::Brief),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// 将用户输入解析后得到的容器位置及链接状态。
#[derive(Debug, Clone)]
pub struct ResolvedContainer {
    /// 容器展示名称。
    pub name: String,
    /// 容器工作区根路径。
    pub workspace_path: PathBuf,
    /// 成功解析到的链接文件路径。
    pub link_path: Option<PathBuf>,
    /// 输入指定但当前缺失、需要恢复的链接路径。
    pub missing_link_path: Option<PathBuf>,
    /// 同名链接和打包文件同时存在时的歧义信息。
    pub ambiguity: Option<ContainerAmbiguity>,
    /// 本次解析是否从配置缓存恢复了链接。
    pub recovered_link: bool,
}

/// 同名 `.veil-link` 与 `.veil` 打包文件同时存在时的路径组合。
#[derive(Debug, Clone)]
pub struct ContainerAmbiguity {
    /// 检测到的链接文件路径。
    pub link_path: PathBuf,
    /// 检测到的打包文件路径。
    pub container_path: PathBuf,
}

/// 新容器的加密默认值配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptionConfig {
    /// 默认加密参数
    #[serde(default)]
    pub defaults: EncryptionDefaults,

    /// Argon2id 参数
    #[serde(default)]
    pub argon2id: Argon2idParams,
}

impl Default for EncryptionConfig {
    /// 使用默认算法参数和 Argon2id 参数。
    fn default() -> Self {
        Self {
            defaults: EncryptionDefaults::default(),
            argon2id: Argon2idParams::default(),
        }
    }
}

/// 新容器默认使用的算法名称。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptionDefaults {
    /// 默认内容加密算法名称。
    #[serde(default = "default_algorithm")]
    pub algorithm: String,

    /// 默认密钥派生函数名称。
    #[serde(default = "default_kdf")]
    pub key_derivation: String,
}

impl Default for EncryptionDefaults {
    /// 使用 ChaCha20-Poly1305 和 Argon2id 作为配置默认值。
    fn default() -> Self {
        Self {
            algorithm: "ChaCha20-Poly1305".to_string(),
            key_derivation: "Argon2id".to_string(),
        }
    }
}

/// 返回默认内容加密算法名称。
fn default_algorithm() -> String {
    "ChaCha20-Poly1305".to_string()
}

/// 配置文件中的 Argon2id 参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Argon2idParams {
    /// 内存使用量，单位为 KiB。
    #[serde(default = "default_memory_kb")]
    pub memory_kb: u32,

    /// Argon2 迭代轮数。
    #[serde(default = "default_iterations")]
    pub iterations: u32,

    /// Argon2 并行度。
    #[serde(default = "default_parallelism")]
    pub parallelism: u32,
}

impl Default for Argon2idParams {
    /// 使用 64 MiB 内存、3 次迭代和并行度 4。
    fn default() -> Self {
        Self {
            memory_kb: 65536,
            iterations: 3,
            parallelism: 4,
        }
    }
}

/// 返回默认 Argon2 内存量，单位为 KiB。
fn default_memory_kb() -> u32 {
    65536
}

/// 返回默认 Argon2 迭代次数。
fn default_iterations() -> u32 {
    3
}

/// 返回默认 Argon2 并行度。
fn default_parallelism() -> u32 {
    4
}

impl GlobalConfig {
    /// 返回 `~/.veil/config.toml` 的路径。
    ///
    /// # 错误
    /// 无法确定用户主目录时返回 [`VeilError::ConfigError`]。
    pub fn config_path() -> Result<PathBuf, VeilError> {
        let home = dirs::home_dir()
            .ok_or_else(|| VeilError::ConfigError("无法获取用户主目录".to_string()))?;
        Ok(home.join(".veil/config.toml"))
    }

    /// 从默认路径加载配置；文件不存在时返回默认配置。
    ///
    /// 反序列化后会把空的版本字段补成当前默认版本。
    ///
    /// # 错误
    /// 路径解析、文件读取或 TOML 解析失败时返回 [`VeilError::ConfigError`]。
    pub fn load() -> Result<Self, VeilError> {
        // 未初始化配置是合法状态，首次运行直接使用内存默认值。
        let path = Self::config_path()?;

        if !path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(&path)
            .map_err(|e| VeilError::ConfigError(format!("读取配置文件失败: {}", e)))?;

        // 旧配置可能没有版本字段，解析后统一补成当前版本。
        let mut config: Self = toml::from_str(&content)
            .map_err(|e| VeilError::ConfigError(format!("解析配置文件失败: {}", e)))?;
        if config.version.is_empty() {
            config.version = default_version();
        }

        Ok(config)
    }

    /// 将配置序列化后写入默认路径。
    ///
    /// 写入先落到同目录的 `config.toml.tmp`，再用重命名替换正式文件。
    ///
    /// # 错误
    /// 目录创建、序列化、临时文件写入、同步或原子替换失败时返回
    /// [`VeilError::ConfigError`]。
    pub fn save(&self) -> Result<(), VeilError> {
        let path = Self::config_path()?;

        // 配置目录可能尚未存在，保存前按需创建。
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| VeilError::ConfigError(format!("创建配置目录失败: {}", e)))?;
        }

        let content = toml::to_string_pretty(self)
            .map_err(|e| VeilError::ConfigError(format!("序列化配置失败: {}", e)))?;

        // 同目录临时文件加原子替换，既避免半截配置，也兼容 Windows 覆盖语义。
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let mut temp_file = tempfile::NamedTempFile::new_in(parent)
            .map_err(|e| VeilError::ConfigError(format!("创建临时配置文件失败: {}", e)))?;
        temp_file
            .write_all(content.as_bytes())
            .map_err(|e| VeilError::ConfigError(format!("写入临时文件失败: {}", e)))?;
        temp_file
            .as_file()
            .sync_all()
            .map_err(|e| VeilError::ConfigError(format!("同步临时文件失败: {}", e)))?;
        temp_file
            .persist(&path)
            .map_err(|e| VeilError::ConfigError(format!("替换配置文件失败: {}", e.error)))?;

        Ok(())
    }

    /// 按配置键、展示名称、稳定 ID 或目录名查找容器。
    ///
    /// 展示名称等非唯一字段只有恰好匹配一个容器时才返回其配置键。
    pub fn find_container_key(&self, name_or_id: &str) -> Option<String> {
        // 配置键是最直接的匹配方式，避免先遍历再判断。
        if self.containers.contains_key(name_or_id) {
            return Some(name_or_id.to_string());
        }

        // 展示名、稳定 ID 和目录名都可以作为用户输入；只有唯一命中才安全返回。
        let matches: Vec<String> = self
            .containers
            .iter()
            .filter(|(_, container)| {
                container.container_name == name_or_id
                    || container.veil_id == name_or_id
                    || container.container_dir.as_deref() == Some(name_or_id)
            })
            .map(|(key, _)| key.clone())
            .collect();
        (matches.len() == 1).then(|| matches[0].clone())
    }

    /// 根据容器配置计算工作区根路径。
    ///
    /// 专属工作区直接使用 `workspace_path`；共享工作区则拼接工作区配置中的根路径
    /// 和容器的 `container_dir`。
    ///
    /// # 错误
    /// 容器、工作区或目录名未注册，或专属工作区路径缺失时返回错误。
    pub fn get_container_workspace_path(&self, container_name: &str) -> Result<PathBuf, VeilError> {
        let key = self.find_container_key(container_name).ok_or_else(|| {
            VeilError::ContainerNotFound(format!("容器 '{}' 不存在", container_name))
        })?;
        let container = self.containers.get(&key).ok_or_else(|| {
            VeilError::ContainerNotFound(format!("容器 '{}' 不存在", container_name))
        })?;

        // 显式 workspace_path 优先于共享或专属工作区推导。
        if let Some(path) = &container.workspace_path {
            return Ok(path.clone());
        }

        if container.dedicated {
            container.workspace_path.clone().ok_or_else(|| {
                VeilError::ConfigError(format!("专属工作区路径未配置: {}", container_name))
            })
        } else {
            let workspace_name = container.workspace.as_ref().ok_or_else(|| {
                VeilError::ConfigError(format!("容器 '{}' 的工作区未配置", container_name))
            })?;

            let workspace = if workspace_name == "default" {
                self.workspace
                    .default
                    .as_ref()
                    .ok_or_else(|| VeilError::ConfigError("默认工作区未配置".to_string()))?
            } else {
                self.workspace.custom.get(workspace_name).ok_or_else(|| {
                    VeilError::ConfigError(format!("工作区 '{}' 不存在", workspace_name))
                })?
            };

            let container_dir = container.container_dir.as_ref().ok_or_else(|| {
                VeilError::ConfigError(format!("容器 '{}' 的目录名未配置", container_name))
            })?;

            Ok(workspace.path.join(container_dir))
        }
    }

    /// 将用户输入解析成容器名、工作区路径和链接文件。
    ///
    /// 支持 `.veil-link`、配置中的容器名，以及直接指向工作区目录的路径。
    /// 输入指向 `.veil` 打包文件时不会自动解包，而是返回包含操作建议的错误。
    ///
    /// # 错误
    /// 链接、配置或工作区无法解析，或输入只能识别为打包文件时返回相应错误。
    pub fn resolve_container(&mut self, input: &str) -> Result<ResolvedContainer, VeilError> {
        let input_path = PathBuf::from(input);
        if input_path.exists() {
            // 已存在的文件只在扩展名匹配时按链接处理。
            if input_path.is_file() && is_link_path(&input_path) {
                return self.resolve_link_file(&input_path, None);
            }

            // 目录必须包含 .veil-meta，避免把任意目录误认成容器。
            if input_path.is_dir() && input_path.join(".veil-meta").exists() {
                let name = input_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("container")
                    .to_string();
                return Ok(ResolvedContainer {
                    name,
                    workspace_path: input_path,
                    link_path: None,
                    missing_link_path: None,
                    ambiguity: None,
                    recovered_link: false,
                });
            }

            if input_path.is_file()
                && input_path.extension().and_then(|ext| ext.to_str()) == Some("veil")
            {
                // 打包文件不能直接当工作区使用，提示用户先执行解包。
                return Err(VeilError::InvalidFormat(format!(
                    "{} 是打包文件，请先运行 veil unpack {}",
                    input_path.display(),
                    input_path.display()
                )));
            }
        }

        if is_link_path(&input_path) {
            return self.resolve_missing_link(&input_path);
        }

        let link_path = with_extension(input, LINK_EXTENSION);
        let container_path = with_extension(input, "veil");
        let link_exists = link_path.exists();
        let container_exists = container_path.exists();

        if link_exists {
            // 同名 .veil 同时存在时仍优先链接，但把歧义交给上层提示。
            let ambiguity = container_exists.then_some(ContainerAmbiguity {
                link_path: link_path.clone(),
                container_path,
            });
            return self.resolve_link_file(&link_path, ambiguity);
        }

        if container_exists {
            return Err(VeilError::InvalidFormat(format!(
                "检测到打包文件 {}，请运行 veil unpack {}",
                container_path.display(),
                container_path.display()
            )));
        }

        if let Some(key) = self.find_container_key(input) {
            let name = self
                .containers
                .get(&key)
                .map(|container| container.container_name.clone())
                .unwrap_or_else(|| input.to_string());
            return Ok(ResolvedContainer {
                name,
                workspace_path: self.get_container_workspace_path(&key)?,
                link_path: None,
                missing_link_path: None,
                ambiguity: None,
                recovered_link: false,
            });
        }

        Err(VeilError::ContainerNotFound(format!(
            "找不到容器 '{}' 或链接文件 '{}.veil-link'",
            input, input
        )))
    }

    /// 为已注册容器生成并保存新的 `.veil-link`。
    ///
    /// 链接使用容器的稳定 ID、当前工作区路径和所在卷信息生成，并加入配置索引。
    ///
    /// # 错误
    /// 容器或工作区不存在、缺少 `veil_id`、链接写入或配置保存失败时返回错误。
    pub fn register_link(
        &mut self,
        container_name: &str,
        link_path: &Path,
    ) -> Result<VeilLink, VeilError> {
        // 先由容器名解析配置键，再取得当前工作区和稳定身份。
        let container_key = self.find_container_key(container_name).ok_or_else(|| {
            VeilError::ContainerNotFound(format!("容器 '{}' 不存在", container_name))
        })?;
        let workspace_path = self.get_container_workspace_path(&container_key)?;
        let veil_id = {
            let container = self.containers.get(&container_key).ok_or_else(|| {
                VeilError::ContainerNotFound(format!("容器 '{}' 不存在", container_name))
            })?;
            if container.veil_id.is_empty() {
                return Err(VeilError::ConfigError(format!(
                    "容器 '{}' 缺少 veil_id",
                    container_name
                )));
            }
            container.veil_id.clone()
        };

        self.register_link_at(&veil_id, container_name, &workspace_path, link_path)
    }

    /// 使用调用方提供的容器身份和工作区路径创建链接。
    ///
    /// 方法会登记目标卷、写入链接文件、缓存原始内容，并把新链接追加到对应容器记录。
    ///
    /// # 错误
    /// 卷识别、链接序列化、文件写入、容器查找或配置保存失败时返回错误。
    pub fn register_link_at(
        &mut self,
        veil_id: &str,
        container_name: &str,
        workspace_path: &Path,
        link_path: &Path,
    ) -> Result<VeilLink, VeilError> {
        // 先登记卷，再计算相对路径；链接文件只保存相对卷根的路径。
        let volume = volume::volume_for_path(workspace_path)?;
        self.register_volume(&volume);
        let relative_path = workspace_path
            .strip_prefix(&volume.mount_path)
            .unwrap_or(workspace_path)
            .to_path_buf();
        let link = VeilLink::new(
            veil_id,
            container_name,
            relative_path,
            volume.volume_id,
            volume.volume_label,
        );
        // 链接内容和配置中的原始字节缓存必须来自同一份序列化结果。
        link.save(link_path)?;
        let raw = fs::read(link_path)?;
        self.cache_link_content(link_path, &link, &raw);

        let container_key = self
            .containers
            .iter()
            .find_map(|(key, container)| (container.veil_id == veil_id).then(|| key.clone()))
            .ok_or_else(|| {
                VeilError::ContainerNotFound(format!("容器 '{}' 不存在", container_name))
            })?;
        let container = self.containers.get_mut(&container_key).ok_or_else(|| {
            VeilError::ContainerNotFound(format!("容器 '{}' 不存在", container_name))
        })?;

        // 容器记录保存绝对路径，便于后续不依赖当前目录定位链接。
        let stored_path = absolute_path(link_path);
        if !container.links.iter().any(|path| path == &stored_path) {
            container.links.push(stored_path);
        }
        self.save()?;

        Ok(link)
    }

    /// 读取链接并把解析结果及原始字节缓存到全局配置。
    ///
    /// 当链接所在卷与链接记录一致时会同步更新卷缓存。
    ///
    /// # 错误
    /// 链接读取、解析或配置保存失败时返回错误。
    pub fn cache_link(&mut self, link_path: &Path) -> Result<VeilLink, VeilError> {
        // 同时保留解析结构和原始字节，前者用于查询，后者用于精确恢复。
        let raw = fs::read(link_path)?;
        let link = VeilLink::load(link_path)?;
        self.cache_link_content(link_path, &link, &raw);
        if let Ok(link_volume) = volume::volume_for_path(link_path) {
            if link.workspace.volume_id == link_volume.volume_id {
                self.register_volume(&link_volume);
            }
        }
        self.save()?;
        Ok(link)
    }

    /// 使用配置缓存的卷挂载路径解析链接目标。
    ///
    /// 仅当缓存卷路径仍存在时才把该路径作为挂载提示，否则回退到链接自身的卷校验。
    ///
    /// # 错误
    /// 链接目标卷不可用或工作区路径无法解析时返回错误。
    pub fn resolve_link_workspace_path(
        &self,
        link: &VeilLink,
        link_path: &Path,
    ) -> Result<PathBuf, VeilError> {
        // 缓存挂载点仍存在时才使用，否则把校验交回链接的卷 ID。
        let mount_path = self
            .volumes
            .get(&link.workspace.volume_id)
            .filter(|volume| volume.mount_path.exists())
            .map(|volume| volume.mount_path.as_path());
        link.resolve_workspace_path_with_mount(link_path, mount_path)
    }

    /// 新增或更新卷缓存，并记录本次探测时间。
    fn register_volume(&mut self, volume: &VolumeInfo) {
        // 同一 volume_id 重新探测时直接覆盖旧路径和标签。
        self.volumes.insert(
            volume.volume_id.clone(),
            VolumeRecord {
                volume_id: volume.volume_id.clone(),
                volume_label: volume.volume_label.clone(),
                mount_path: volume.mount_path.clone(),
                is_external: volume.is_external,
                last_seen_at: chrono::Utc::now().to_rfc3339(),
            },
        );
    }

    /// 缓存 `.veil-link` 的原始字节和解析结果。
    ///
    /// `raw_hex` 保存与文件完全相同的字节，`content_hash` 用于检查恢复副本是否损坏。
    fn cache_link_content(&mut self, link_path: &Path, link: &VeilLink, raw: &[u8]) {
        // 路径统一转为绝对形式，保证同一链接不会产生两条缓存记录。
        let link_path = absolute_path(link_path);
        let now = chrono::Utc::now().to_rfc3339();
        let content_hash = blake3::hash(raw).to_hex().to_string();
        let raw_hex = hex::encode(raw);

        // 已存在记录就地更新，避免链接被反复解析后缓存无限增长。
        if let Some(existing) = self
            .links
            .iter_mut()
            .find(|record| record.link_path == link_path)
        {
            existing.veil_id = link.workspace.veil_id.clone();
            existing.container_name = link.workspace.container_name.clone();
            existing.volume_id = link.workspace.volume_id.clone();
            existing.volume_label = link.workspace.volume_label.clone();
            existing.relative_path = link.workspace.path.clone();
            existing.status = LinkStatus::Present;
            existing.content_hash = content_hash;
            existing.raw_hex = raw_hex;
            existing.last_seen_at = now;
        } else {
            self.links.push(LinkRecord {
                link_path,
                veil_id: link.workspace.veil_id.clone(),
                container_name: link.workspace.container_name.clone(),
                volume_id: link.workspace.volume_id.clone(),
                volume_label: link.workspace.volume_label.clone(),
                relative_path: link.workspace.path.clone(),
                status: LinkStatus::Present,
                content_hash,
                raw_hex,
                last_seen_at: now,
            });
        }

        // 链接中的展示信息是当前可见来源，可刷新卷和容器的用户界面字段。
        if let Some(volume) = self.volumes.get_mut(&link.workspace.volume_id) {
            volume.volume_label = link.workspace.volume_label.clone();
            volume.last_seen_at = chrono::Utc::now().to_rfc3339();
        }

        if let Some(container) = self
            .containers
            .values_mut()
            .find(|container| container.veil_id == link.workspace.veil_id)
        {
            container.container_name = link.workspace.container_name.clone();
        }
    }

    /// 读取链接、更新缓存并解析其工作区路径。
    ///
    /// `ambiguity` 会原样带回，供上层提示同名打包文件的存在。
    ///
    /// # 错误
    /// 链接读取、解析、卷校验或工作区路径解析失败时返回错误。
    fn resolve_link_file(
        &mut self,
        link_path: &Path,
        ambiguity: Option<ContainerAmbiguity>,
    ) -> Result<ResolvedContainer, VeilError> {
        // 先读取原始字节，随后解析和缓存都使用同一份内容。
        let raw = fs::read(link_path)?;
        let link = VeilLink::load(link_path)?;
        self.cache_link_content(link_path, &link, &raw);
        // 优先使用已缓存且仍存在的挂载路径，否则回退到链接自身的卷校验。
        let mount_path = self
            .volumes
            .get(&link.workspace.volume_id)
            .map(|volume| volume.mount_path.as_path());
        let workspace_path = link.resolve_workspace_path_with_mount(link_path, mount_path)?;
        let name = link.container_name();

        Ok(ResolvedContainer {
            name,
            workspace_path,
            link_path: Some(link_path.to_path_buf()),
            missing_link_path: None,
            ambiguity,
            recovered_link: false,
        })
    }

    /// 在链接文件缺失时，通过配置缓存或注册信息恢复容器定位。
    ///
    /// 恢复优先级为：字节级缓存副本、容器记录中的链接路径、与文件名同名的容器键。
    ///
    /// # 错误
    /// 缓存副本损坏，且配置中也没有可用容器映射时返回错误。
    fn resolve_missing_link(&mut self, link_path: &Path) -> Result<ResolvedContainer, VeilError> {
        let missing_path = absolute_path(link_path);
        // 第一优先级是配置保存的原始链接字节，能够完整恢复文件。
        if self.restore_cached_link(&missing_path)? {
            let mut resolved = self.resolve_link_file(&missing_path, None)?;
            resolved.recovered_link = true;
            return Ok(resolved);
        }

        let registered_name = self.containers.iter().find_map(|(name, container)| {
            container
                .links
                .iter()
                .any(|registered| registered == &missing_path)
                .then(|| name.clone())
        });
        let inferred_name = link_path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("container")
            .to_string();
        let name = registered_name
            // 其次使用容器记录中登记过的链接路径，最后尝试与文件名同名的容器键。
            .or_else(|| {
                self.containers
                    .contains_key(&inferred_name)
                    .then_some(inferred_name)
            })
            .ok_or_else(|| {
                VeilError::ContainerNotFound(format!(
                    "链接文件不存在，且配置中找不到对应容器: {}",
                    link_path.display()
                ))
            })?;

        Ok(ResolvedContainer {
            name: name.clone(),
            workspace_path: self.get_container_workspace_path(&name)?,
            link_path: None,
            missing_link_path: Some(link_path.to_path_buf()),
            ambiguity: None,
            recovered_link: false,
        })
    }

    /// 从 `config.toml` 中的原始字节副本恢复缺失的 `.veil-link`。
    ///
    /// 写入前校验内容哈希。返回 `false` 表示配置中没有可恢复副本。
    ///
    /// # 错误
    /// 十六进制副本损坏、哈希不匹配、目录创建、文件写入或配置保存失败时返回错误。
    fn restore_cached_link(&mut self, link_path: &Path) -> Result<bool, VeilError> {
        let link_path = absolute_path(link_path);
        let Some(record) = self
            .links
            .iter()
            .find(|record| record.link_path == link_path)
            .cloned()
        else {
            return Ok(false);
        };

        if record.raw_hex.is_empty() {
            return Ok(false);
        }

        let raw = hex::decode(&record.raw_hex)
            .map_err(|error| VeilError::ConfigError(format!("链接缓存副本损坏: {}", error)))?;
        // 哈希用于确认缓存副本没有被改写，缺失旧哈希时仍允许恢复。
        let actual_hash = blake3::hash(&raw).to_hex().to_string();
        if !record.content_hash.is_empty() && actual_hash != record.content_hash {
            return Err(VeilError::ConfigError(format!(
                "链接缓存副本哈希不匹配: {}",
                link_path.display()
            )));
        }

        if let Some(parent) = link_path.parent() {
            fs::create_dir_all(parent)?;
        }
        // 恢复的是捕获时的原始字节，避免 TOML 重新序列化造成内容漂移。
        fs::write(&link_path, raw)?;

        if let Some(existing) = self
            .links
            .iter_mut()
            .find(|item| item.link_path == link_path)
        {
            existing.status = LinkStatus::Present;
            existing.last_seen_at = chrono::Utc::now().to_rfc3339();
            existing.content_hash = actual_hash;
        }
        self.save()?;
        Ok(true)
    }
}

/// 判断路径扩展名是否严格等于 `.veil-link`。
fn is_link_path(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some(LINK_EXTENSION)
}

/// 将输入路径的扩展名替换为目标扩展名；无扩展名时直接追加。
fn with_extension(input: &str, extension: &str) -> PathBuf {
    let path = PathBuf::from(input);
    // 已有扩展名时视为用户显式路径，只替换后缀；否则追加标准后缀。
    if path.extension().is_some() {
        path.with_extension(extension)
    } else {
        PathBuf::from(format!("{}.{}", input, extension))
    }
}

/// 将路径转换为绝对形式；当前目录不可用时以 `.` 为基准回退。
fn absolute_path(path: &Path) -> PathBuf {
    // 绝对路径原样返回，避免不必要的当前目录依赖。
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}
