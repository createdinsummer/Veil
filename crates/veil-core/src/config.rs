//! 全局配置管理模块
//!
//! 配置文件位置：~/.veil/config.toml
//! 管理工作区、容器映射、用户偏好等全局设置

use crate::error::VeilError;
use crate::link::{VeilLink, LINK_EXTENSION};
use crate::metadata::MetaHeader;
use crate::volume::{self, VolumeInfo};
use crate::workspace::WorkspaceConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// 全局配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalConfig {
    /// 配置版本
    #[serde(default = "default_version")]
    pub version: String,

    /// 系统信息
    #[serde(default)]
    pub system: SystemConfig,

    /// 工作区配置
    #[serde(default)]
    pub workspace: WorkspaceSection,

    /// 容器映射
    #[serde(default)]
    pub containers: HashMap<String, ContainerConfig>,

    /// 已发现的磁盘卷信息
    #[serde(default)]
    pub volumes: HashMap<String, VolumeRecord>,

    /// 已知 `.veil-link` 的完整字节副本
    #[serde(default)]
    pub links: Vec<LinkRecord>,

    /// 用户偏好
    #[serde(default)]
    pub preferences: PreferencesConfig,

    /// 加密默认配置
    #[serde(default)]
    pub encryption: EncryptionConfig,
}

impl Default for GlobalConfig {
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

fn default_version() -> String {
    "1.0".to_string()
}

/// 系统配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemConfig {
    /// 图标是否已配置
    #[serde(default)]
    pub icons_configured: bool,

    /// 图标配置时间
    pub icons_configured_at: Option<String>,

    /// 图标版本
    pub icons_version: Option<String>,

    /// 是否首次运行
    #[serde(default = "default_true")]
    pub first_run: bool,

    /// 各一次性提示是否已展示
    #[serde(default)]
    pub init_hint_shown: bool,

    #[serde(default)]
    pub pack_hint_shown: bool,

    #[serde(default)]
    pub unpack_hint_shown: bool,

    #[serde(default)]
    pub extension_hint_shown: bool,
}

impl Default for SystemConfig {
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

fn default_true() -> bool {
    true
}

/// 工作区配置段
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceSection {
    /// 默认工作区
    pub default: Option<WorkspaceConfig>,

    /// 自定义工作区
    #[serde(default)]
    pub custom: HashMap<String, WorkspaceConfig>,
}

/// 容器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerConfig {
    /// 容器稳定 ID；旧配置可能为空。
    pub veil_id: String,

    /// 展示名称，不再作为唯一键。
    #[serde(default)]
    pub container_name: String,

    /// 所属工作区名称（default 或自定义名称）
    pub workspace: Option<String>,

    /// 工作区中的容器目录名
    pub container_dir: Option<String>,

    /// 专属工作区路径（如果是专属工作区）
    pub workspace_path: Option<PathBuf>,

    /// 是否为专属工作区
    #[serde(default)]
    pub dedicated: bool,

    /// 创建时间
    pub created_at: String,

    /// 最后访问时间
    pub last_accessed: Option<String>,

    /// 指向该容器的链接文件（支持多个）
    #[serde(default)]
    pub links: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeRecord {
    pub volume_id: String,
    pub volume_label: String,
    pub mount_path: PathBuf,
    pub is_external: bool,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LinkStatus {
    Present,
    Missing,
    Unavailable,
    Invalid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkRecord {
    pub link_path: PathBuf,
    pub veil_id: String,
    #[serde(default)]
    pub container_name: String,
    #[serde(default)]
    pub volume_id: String,
    #[serde(default)]
    pub volume_label: String,
    #[serde(default)]
    pub relative_path: PathBuf,
    pub status: LinkStatus,
    #[serde(default)]
    pub content_hash: String,
    /// 原始 UTF-8 TOML 内容的十六进制副本，可字节级恢复。
    #[serde(default)]
    pub raw_hex: String,
    pub last_seen_at: String,
}

/// 用户偏好配置
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

fn default_hints_level() -> HintsLevel {
    HintsLevel::Full
}

fn default_kdf() -> String {
    "Argon2id".to_string()
}

fn default_cache_timeout() -> u64 {
    300
}

fn default_log_level() -> String {
    "info".to_string()
}

/// 提示级别
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HintsLevel {
    /// 完整提示（新手）
    Full,
    /// 简要提示（熟悉后）
    Brief,
    /// 关闭提示（高级用户）
    Off,
}

impl HintsLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Brief => "brief",
            Self::Off => "off",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "full" => Some(Self::Full),
            "brief" => Some(Self::Brief),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedContainer {
    pub name: String,
    pub workspace_path: PathBuf,
    pub link_path: Option<PathBuf>,
    pub missing_link_path: Option<PathBuf>,
    pub ambiguity: Option<ContainerAmbiguity>,
    pub recovered_link: bool,
}

#[derive(Debug, Clone)]
pub struct ContainerAmbiguity {
    pub link_path: PathBuf,
    pub container_path: PathBuf,
}

/// 加密配置
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
    fn default() -> Self {
        Self {
            defaults: EncryptionDefaults::default(),
            argon2id: Argon2idParams::default(),
        }
    }
}

/// 默认加密参数
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptionDefaults {
    #[serde(default = "default_algorithm")]
    pub algorithm: String,

    #[serde(default = "default_kdf")]
    pub key_derivation: String,
}

impl Default for EncryptionDefaults {
    fn default() -> Self {
        Self {
            algorithm: "AES-256-GCM".to_string(),
            key_derivation: "Argon2id".to_string(),
        }
    }
}

fn default_algorithm() -> String {
    "AES-256-GCM".to_string()
}

/// Argon2id 参数
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Argon2idParams {
    /// 内存使用（KB）
    #[serde(default = "default_memory_kb")]
    pub memory_kb: u32,

    /// 迭代次数
    #[serde(default = "default_iterations")]
    pub iterations: u32,

    /// 并行度
    #[serde(default = "default_parallelism")]
    pub parallelism: u32,
}

impl Default for Argon2idParams {
    fn default() -> Self {
        Self {
            memory_kb: 65536, // 64 MB
            iterations: 3,
            parallelism: 4,
        }
    }
}

fn default_memory_kb() -> u32 {
    65536
}

fn default_iterations() -> u32 {
    3
}

fn default_parallelism() -> u32 {
    4
}

impl GlobalConfig {
    /// 获取配置文件路径
    pub fn config_path() -> Result<PathBuf, VeilError> {
        let home = dirs::home_dir()
            .ok_or_else(|| VeilError::ConfigError("无法获取用户主目录".to_string()))?;
        Ok(home.join(".veil/config.toml"))
    }

    /// 加载配置文件
    pub fn load() -> Result<Self, VeilError> {
        let path = Self::config_path()?;

        if !path.exists() {
            // 配置文件不存在，返回默认配置
            return Ok(Self::default());
        }

        let content = fs::read_to_string(&path)
            .map_err(|e| VeilError::ConfigError(format!("读取配置文件失败: {}", e)))?;

        let mut config: Self = toml::from_str(&content)
            .map_err(|e| VeilError::ConfigError(format!("解析配置文件失败: {}", e)))?;
        if config.version.is_empty() {
            config.version = default_version();
        }

        Ok(config)
    }

    /// 保存配置文件
    pub fn save(&self) -> Result<(), VeilError> {
        let path = Self::config_path()?;

        // 确保目录存在
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| VeilError::ConfigError(format!("创建配置目录失败: {}", e)))?;
        }

        let content = toml::to_string_pretty(self)
            .map_err(|e| VeilError::ConfigError(format!("序列化配置失败: {}", e)))?;

        // 原子写入
        let temp_path = path.with_extension("toml.tmp");
        fs::write(&temp_path, content)
            .map_err(|e| VeilError::ConfigError(format!("写入临时文件失败: {}", e)))?;

        fs::rename(&temp_path, &path)
            .map_err(|e| VeilError::ConfigError(format!("重命名配置文件失败: {}", e)))?;

        Ok(())
    }

    pub fn find_container_key(&self, name_or_id: &str) -> Option<String> {
        if self.containers.contains_key(name_or_id) {
            return Some(name_or_id.to_string());
        }

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

    /// 获取容器的工作区路径
    pub fn get_container_workspace_path(&self, container_name: &str) -> Result<PathBuf, VeilError> {
        let key = self.find_container_key(container_name).ok_or_else(|| {
            VeilError::ContainerNotFound(format!("容器 '{}' 不存在", container_name))
        })?;
        let container = self.containers.get(&key).ok_or_else(|| {
            VeilError::ContainerNotFound(format!("容器 '{}' 不存在", container_name))
        })?;

        if let Some(path) = &container.workspace_path {
            return Ok(path.clone());
        }

        if container.dedicated {
            // 专属工作区
            container.workspace_path.clone().ok_or_else(|| {
                VeilError::ConfigError(format!("专属工作区路径未配置: {}", container_name))
            })
        } else {
            // 共享工作区
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
    pub fn resolve_container(&mut self, input: &str) -> Result<ResolvedContainer, VeilError> {
        let input_path = PathBuf::from(input);
        if input_path.exists() {
            if input_path.is_file() && is_link_path(&input_path) {
                return self.resolve_link_file(&input_path, None);
            }

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
                .and_then(|container| {
                    (!container.container_name.is_empty())
                        .then_some(container.container_name.clone())
                })
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

    pub fn register_link(
        &mut self,
        container_name: &str,
        link_path: &Path,
    ) -> Result<VeilLink, VeilError> {
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

    pub fn register_link_at(
        &mut self,
        veil_id: &str,
        container_name: &str,
        workspace_path: &Path,
        link_path: &Path,
    ) -> Result<VeilLink, VeilError> {
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
        link.save(link_path)?;
        let raw = fs::read(link_path)?;
        self.cache_link_content(link_path, &link, &raw);

        let container_key = self
            .containers
            .iter()
            .find_map(|(key, container)| {
                (container.veil_id == veil_id || container.container_name == container_name)
                    .then(|| key.clone())
            })
            .ok_or_else(|| {
                VeilError::ContainerNotFound(format!("容器 '{}' 不存在", container_name))
            })?;
        let container = self.containers.get_mut(&container_key).ok_or_else(|| {
            VeilError::ContainerNotFound(format!("容器 '{}' 不存在", container_name))
        })?;

        let stored_path = absolute_path(link_path);
        if !container.links.iter().any(|path| path == &stored_path) {
            container.links.push(stored_path);
        }
        self.save()?;

        Ok(link)
    }

    pub fn cache_link(&mut self, link_path: &Path) -> Result<VeilLink, VeilError> {
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

    pub fn resolve_link_workspace_path(
        &self,
        link: &VeilLink,
        link_path: &Path,
    ) -> Result<PathBuf, VeilError> {
        let mount_path = self
            .volumes
            .get(&link.workspace.volume_id)
            .filter(|volume| volume.mount_path.exists())
            .map(|volume| volume.mount_path.as_path());
        link.resolve_workspace_path_with_mount(link_path, mount_path)
    }

    /// 扫描容器所在工作区，把所有同级容器的链接记录同步进配置。
    pub fn sync_workspace_links(&mut self, container_dir: &Path) -> Result<usize, VeilError> {
        let workspace_root = container_dir.parent().unwrap_or(container_dir);
        let mut changed = 0usize;

        for entry in fs::read_dir(workspace_root)? {
            let entry = entry?;
            let sibling = entry.path();
            let meta_path = sibling.join(".veil-meta");
            if !sibling.is_dir() || !meta_path.exists() {
                continue;
            }

            let bytes = fs::read(&meta_path)?;
            let header = match MetaHeader::from_bytes(&bytes) {
                Ok(header) => header,
                Err(_) => continue,
            };
            if header.veil_id.is_empty() {
                continue;
            }

            if !self
                .containers
                .values()
                .any(|container| container.veil_id == header.veil_id)
            {
                self.containers.insert(
                    header.veil_id.clone(),
                    ContainerConfig {
                        veil_id: header.veil_id.clone(),
                        container_name: if header.container_name.is_empty() {
                            sibling
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("container")
                                .to_string()
                        } else {
                            header.container_name.clone()
                        },
                        workspace: None,
                        container_dir: sibling
                            .file_name()
                            .and_then(|name| name.to_str())
                            .map(ToOwned::to_owned),
                        workspace_path: Some(sibling.clone()),
                        dedicated: header.workspace_type == "dedicated",
                        created_at: chrono::Utc::now().to_rfc3339(),
                        last_accessed: None,
                        links: Vec::new(),
                    },
                );
            }

            let existing_records: Vec<LinkRecord> = self
                .links
                .iter()
                .filter(|record| record.veil_id == header.veil_id)
                .cloned()
                .collect();

            if existing_records.is_empty() {
                let link_dir = workspace_root.join(".veil/links");
                let link_path = link_dir.join(format!("{}.veil-link", header.veil_id));
                self.write_link_for_container_path(&sibling, &header, &link_path)?;
                changed += 1;
            } else {
                for record in existing_records {
                    if record.link_path.exists() {
                        self.cache_link(&record.link_path)?;
                        changed += 1;
                    } else if self.restore_cached_link(&record.link_path)? {
                        changed += 1;
                    } else {
                        self.write_link_for_container_path(&sibling, &header, &record.link_path)?;
                        changed += 1;
                    }
                }
            }
        }

        if changed > 0 {
            self.save()?;
        }
        Ok(changed)
    }

    fn register_volume(&mut self, volume: &VolumeInfo) {
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

    fn write_link_for_container_path(
        &mut self,
        container_path: &Path,
        header: &MetaHeader,
        link_path: &Path,
    ) -> Result<(), VeilError> {
        let volume = volume::volume_for_path(container_path)?;
        self.register_volume(&volume);
        let relative_path = container_path
            .strip_prefix(&volume.mount_path)
            .unwrap_or(container_path)
            .to_path_buf();
        let container_name = if header.container_name.is_empty() {
            container_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("container")
                .to_string()
        } else {
            header.container_name.clone()
        };
        let link = VeilLink::new(
            header.veil_id.clone(),
            container_name,
            relative_path,
            volume.volume_id,
            volume.volume_label,
        );
        link.save(link_path)?;
        let raw = fs::read(link_path)?;
        self.cache_link_content(link_path, &link, &raw);
        Ok(())
    }

    fn cache_link_content(&mut self, link_path: &Path, link: &VeilLink, raw: &[u8]) {
        let link_path = absolute_path(link_path);
        let now = chrono::Utc::now().to_rfc3339();
        let content_hash = blake3::hash(raw).to_hex().to_string();
        let raw_hex = hex::encode(raw);

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

    fn resolve_link_file(
        &mut self,
        link_path: &Path,
        ambiguity: Option<ContainerAmbiguity>,
    ) -> Result<ResolvedContainer, VeilError> {
        let raw = fs::read(link_path)?;
        let link = VeilLink::load(link_path)?;
        self.cache_link_content(link_path, &link, &raw);
        let mount_path = self
            .volumes
            .get(&link.workspace.volume_id)
            .map(|volume| volume.mount_path.as_path());
        let workspace_path = link.resolve_workspace_path_with_mount(link_path, mount_path)?;
        let name = link.container_name(link_path);

        Ok(ResolvedContainer {
            name,
            workspace_path,
            link_path: Some(link_path.to_path_buf()),
            missing_link_path: None,
            ambiguity,
            recovered_link: false,
        })
    }

    fn resolve_missing_link(&mut self, link_path: &Path) -> Result<ResolvedContainer, VeilError> {
        let missing_path = absolute_path(link_path);
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

fn is_link_path(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some(LINK_EXTENSION)
}

fn with_extension(input: &str, extension: &str) -> PathBuf {
    let path = PathBuf::from(input);
    if path.extension().is_some() {
        path.with_extension(extension)
    } else {
        PathBuf::from(format!("{}.{}", input, extension))
    }
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}
