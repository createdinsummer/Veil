//! 全局配置管理模块
//!
//! 配置文件位置：~/.veil/config.toml
//! 管理工作区、容器映射、用户偏好等全局设置

use crate::error::VeilError;
use crate::link::{portable_workspace_path, VeilLink, LINK_EXTENSION};
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
            memory_kb: 65536,  // 64 MB
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

        let content = fs::read_to_string(&path).map_err(|e| {
            VeilError::ConfigError(format!("读取配置文件失败: {}", e))
        })?;

        let mut config: Self = toml::from_str(&content).map_err(|e| {
            VeilError::ConfigError(format!("解析配置文件失败: {}", e))
        })?;
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
            fs::create_dir_all(parent).map_err(|e| {
                VeilError::ConfigError(format!("创建配置目录失败: {}", e))
            })?;
        }

        let content = toml::to_string_pretty(self).map_err(|e| {
            VeilError::ConfigError(format!("序列化配置失败: {}", e))
        })?;

        // 原子写入
        let temp_path = path.with_extension("toml.tmp");
        fs::write(&temp_path, content).map_err(|e| {
            VeilError::ConfigError(format!("写入临时文件失败: {}", e))
        })?;

        fs::rename(&temp_path, &path).map_err(|e| {
            VeilError::ConfigError(format!("重命名配置文件失败: {}", e))
        })?;

        Ok(())
    }

    /// 获取容器的工作区路径
    pub fn get_container_workspace_path(&self, container_name: &str) -> Result<PathBuf, VeilError> {
        let container = self.containers.get(container_name).ok_or_else(|| {
            VeilError::ContainerNotFound(format!("容器 '{}' 不存在", container_name))
        })?;

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
                self.workspace.default.as_ref().ok_or_else(|| {
                    VeilError::ConfigError("默认工作区未配置".to_string())
                })?
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
    pub fn resolve_container(&self, input: &str) -> Result<ResolvedContainer, VeilError> {
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

        if self.containers.contains_key(input) {
            return Ok(ResolvedContainer {
                name: input.to_string(),
                workspace_path: self.get_container_workspace_path(input)?,
                link_path: None,
                missing_link_path: None,
                ambiguity: None,
            });
        }

        Err(VeilError::ContainerNotFound(format!(
            "找不到容器 '{}' 或链接文件 '{}.veil-link'",
            input, input
        )))
    }

    pub fn write_link(&self, container_name: &str, link_path: &Path) -> Result<VeilLink, VeilError> {
        let container = self.containers.get(container_name).ok_or_else(|| {
            VeilError::ContainerNotFound(format!("容器 '{}' 不存在", container_name))
        })?;

        let workspace_path = self.get_container_workspace_path(container_name)?;
        let workspace_type = if container.dedicated {
            "dedicated"
        } else {
            container.workspace.as_deref().unwrap_or("default")
        };

        let link = VeilLink::new(
            container_name,
            portable_workspace_path(&workspace_path),
            workspace_type,
            container.dedicated,
        );
        link.save(link_path)?;
        Ok(link)
    }

    pub fn register_link(
        &mut self,
        container_name: &str,
        link_path: &Path,
    ) -> Result<VeilLink, VeilError> {
        let link = self.write_link(container_name, link_path)?;
        let container = self.containers.get_mut(container_name).ok_or_else(|| {
            VeilError::ContainerNotFound(format!("容器 '{}' 不存在", container_name))
        })?;

        let stored_path = absolute_path(link_path);
        if !container.links.iter().any(|path| path == &stored_path) {
            container.links.push(stored_path);
        }
        self.save()?;

        Ok(link)
    }

    fn resolve_link_file(
        &self,
        link_path: &Path,
        ambiguity: Option<ContainerAmbiguity>,
    ) -> Result<ResolvedContainer, VeilError> {
        let link = VeilLink::load(link_path)?;
        let workspace_path = link.resolve_workspace_path(link_path)?;
        let name = link.container_name(link_path);

        Ok(ResolvedContainer {
            name,
            workspace_path,
            link_path: Some(link_path.to_path_buf()),
            missing_link_path: None,
            ambiguity,
        })
    }

    fn resolve_missing_link(&self, link_path: &Path) -> Result<ResolvedContainer, VeilError> {
        let missing_path = absolute_path(link_path);
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
            .or_else(|| self.containers.contains_key(&inferred_name).then_some(inferred_name))
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
        })
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
