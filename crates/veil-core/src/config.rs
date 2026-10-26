//! 本机及移动盘配置；通过已登记的工作区扫描、核验容器身份。

use crate::error::VeilError;
use crate::link::{LINK_EXTENSION, VeilLink};
use crate::metadata::{MetaHeader, VEIL_INFO_FILE};
use crate::volume::{self, VolumeInfo};
use crate::work::{WorkConfig, WorkType};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// 本机或单块移动盘的配置，不保存容器位置绑定。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalConfig {
    /// 配置格式版本。
    #[serde(default = "default_version")]
    pub version: String,

    /// 系统状态和一次性提示记录。
    #[serde(default)]
    pub system: SystemConfig,

    /// 默认 Work 和自定义 Work。
    #[serde(default)]
    pub work: WorkSection,

    /// 以配置键索引的 Veil 注册表。
    #[serde(default)]
    pub veils: HashMap<String, VeilConfig>,

    /// 已知 `.veil-link` 的结构化正文及本机索引，链接丢失后可据此重新生成。
    #[serde(default)]
    pub links: Vec<LinkRecord>,

    /// 用户设置。
    #[serde(default)]
    pub user_settings: UserSettings,

    /// 配置来源仅用于运行时解析，不写入文件。
    #[serde(skip)]
    source_path: Option<PathBuf>,
}

impl Default for GlobalConfig {
    /// 创建版本、空索引和默认用户设置组成的配置。
    fn default() -> Self {
        Self {
            version: default_version(),
            system: SystemConfig::default(),
            work: WorkSection::default(),
            veils: HashMap::new(),
            links: Vec::new(),
            user_settings: UserSettings::default(),
            source_path: None,
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
        }
    }
}

/// 作为 Serde 默认值返回 `true`。
fn default_true() -> bool {
    true
}

/// 配置文件中保存的 Work 集合。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkSection {
    /// 默认 Work 配置；首次初始化时可自动补全。
    pub default: Option<WorkConfig>,

    /// 以名称索引的自定义工作区；显式路径使用目录路径作为配置键。
    #[serde(default)]
    pub custom: HashMap<String, WorkConfig>,
}

/// 容器信息缓存，不参与容器位置查找。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VeilConfig {
    /// Veil 稳定 ID。
    pub veil_id: String,

    /// 展示名称，不再作为唯一键。
    pub veil_name: String,

    /// RFC 3339 格式的注册创建时间。
    pub created_at: String,

    /// 预留的最后访问时间；当前初始化写入 `None`。
    pub last_accessed: Option<String>,

    /// 指向该容器的链接文件路径，可保存多个副本。
    #[serde(default)]
    pub links: Vec<PathBuf>,
}

/// `.veil-link` 在当前系统中的可解析状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LinkStatus {
    /// 链接文件存在且已成功解析。
    Present,
    /// 链接文件缺失，但配置中可能保存可恢复内容。
    Missing,
    /// 链接存在，但当前未找到目标容器。
    Unavailable,
    /// 链接文件存在但无法解析。
    Invalid,
}

/// 已知 `.veil-link` 的结构化正文及本机索引。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkRecord {
    /// 与独立 `.veil-link` 共用的完整正文，保存为独立的 `link` 对象。
    pub link: VeilLink,
    /// 链接文件的绝对路径。
    pub link_path: PathBuf,
    /// 最近一次观察到链接时的状态。
    pub seen_status: LinkStatus,
    /// 最近一次观察或恢复链接的时间。
    pub last_seen_at: String,
}

/// 用户设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSettings {
    /// 操作提示的展示级别。
    #[serde(default = "default_hint_level")]
    pub hint_level: HintLevel,
}

impl Default for UserSettings {
    /// 默认展示完整操作提示。
    fn default() -> Self {
        Self {
            hint_level: HintLevel::Full,
        }
    }
}

/// 返回提示级别的 Serde 默认值。
fn default_hint_level() -> HintLevel {
    HintLevel::Full
}

/// 一次性使用提示的展示级别。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HintLevel {
    /// 展示全部提示。
    Full,
    /// 仅展示解包和链接恢复提示。
    Brief,
    /// 关闭提示。
    Off,
}

impl HintLevel {
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
pub struct ResolvedVeil {
    /// Veil 展示名称。
    pub veil_name: String,
    /// 从实际 `.veil-info` 明文头确认的稳定 Veil ID。
    pub veil_id: String,
    /// Veil 实际目录 `veil_dir`。
    pub veil_dir: PathBuf,
    /// 成功解析到的链接文件路径。
    pub link_path: Option<PathBuf>,
    /// 本次解析是否从配置缓存恢复了链接。
    pub recovered_link: bool,
}

impl GlobalConfig {
    /// 本机配置固定在用户主目录下。
    pub fn config_path() -> Result<PathBuf, VeilError> {
        Ok(dirs::home_dir()
            .ok_or_else(|| VeilError::ConfigError("无法获取用户主目录".into()))?
            .join(".veil/config.jsonc"))
    }

    pub fn load() -> Result<Self, VeilError> {
        Self::load_from(&Self::config_path()?)
    }

    /// 记录配置来源；缺少文件时只返回内存默认值。
    pub fn load_from(path: &Path) -> Result<Self, VeilError> {
        let path = std::path::absolute(path)?;
        let mut config: Self = match fs::read_to_string(&path) {
            Ok(content) => crate::jsonc::from_str(&content).map_err(|error| {
                VeilError::ConfigError(format!("解析配置失败 {}: {error}", path.display()))
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                return Err(VeilError::ConfigError(format!(
                    "读取配置失败 {}: {error}",
                    path.display()
                )));
            }
        };
        config.source_path = Some(path);
        config.validate()?;
        Ok(config)
    }

    pub fn source_path(&self) -> Result<PathBuf, VeilError> {
        self.source_path
            .clone()
            .map(Ok)
            .unwrap_or_else(Self::config_path)
    }

    pub fn config_dir(&self) -> Result<PathBuf, VeilError> {
        Ok(self.source_path()?.parent().unwrap().to_path_buf())
    }

    /// 只保存当前配置，不把其他盘的工作区合并写入本机配置。
    pub fn save(&self) -> Result<(), VeilError> {
        self.validate()?;
        crate::jsonc::save(&self.source_path()?, self)
    }

    fn validate(&self) -> Result<(), VeilError> {
        let mut seen = HashSet::new();
        for veil in self.veils.values() {
            crate::work::validate_veil_id(&veil.veil_id)?;
            if !seen.insert(&veil.veil_id) {
                return Err(VeilError::VeilIdConflict(format!(
                    "容器 ID '{}' 在配置中重复",
                    veil.veil_id
                )));
            }
        }
        for work in self.work.default.iter().chain(self.work.custom.values()) {
            work.validate_path()?;
        }
        Ok(())
    }

    /// 输入当前挂载卷快照，便于复用实际发现流程及验证换盘场景。
    /// 配置不存在时跳过；发现过程不创建文件或工作区。
    pub fn discover_from(
        local_path: &Path,
        volumes: &[VolumeInfo],
    ) -> Result<Vec<Self>, VeilError> {
        let mut configs = vec![Self::load_from(local_path)?];
        let mut seen = HashSet::from([comparable_path(local_path)]);
        for volume in volumes.iter().filter(|volume| volume.if_out_volume) {
            let path = volume.root_path.join(".veil/config.jsonc");
            if volume.root_path.is_dir()
                && path.try_exists()?
                && seen.insert(comparable_path(&path))
            {
                configs.push(Self::load_from(&path)?);
            }
        }
        Ok(configs)
    }

    /// 每次寻址重新枚举挂载卷，使用调用方尚未保存的当前配置。
    fn all_configs(&self) -> Result<Vec<Self>, VeilError> {
        let own_path = comparable_path(&self.source_path()?);
        let mut configs = Self::discover_from(&Self::config_path()?, &volume::mounted_volumes()?)?;
        configs.retain(|config| comparable_path(&config.source_path().unwrap()) != own_path);
        configs.push(self.clone());
        Ok(configs)
    }

    /// 创建位置所属盘的配置；本机统一使用用户主目录配置。
    pub fn for_location(location: &Path) -> Result<Self, VeilError> {
        let volume = volume::volume_for_path(location)?;
        if volume.if_out_volume {
            Self::load_from(&volume.root_path.join(".veil/config.jsonc"))
        } else {
            Self::load()
        }
    }

    /// 显式路径按路径所属盘登记；名称优先在创建位置所属配置中解析。
    pub fn creation_work(
        location: &Path,
        name: Option<&str>,
        root: Option<&Path>,
        dedicated: bool,
    ) -> Result<(Self, WorkConfig), VeilError> {
        let mut config = Self::for_location(root.unwrap_or(location))?;
        if let Some(root) = root {
            let work = config.register_work_at(&std::path::absolute(root)?, dedicated)?;
            return Ok((config, work));
        }
        let name = name.unwrap_or("default");
        if name == "default" {
            config.ensure_default_work()?;
            let work = config.work.default.clone().unwrap();
            return Ok((config, work));
        }
        if let Some(work) = config.work.custom.get(name).cloned() {
            return Ok((config, work));
        }
        let matches: Vec<_> = config
            .all_configs()?
            .into_iter()
            .filter_map(|config| {
                config
                    .work
                    .custom
                    .get(name)
                    .cloned()
                    .map(|work| (config, work))
            })
            .collect();
        let mut matches = matches.into_iter();
        let found = matches
            .next()
            .ok_or_else(|| VeilError::WorkError(format!("工作区 '{name}' 不存在")))?;
        if matches.next().is_some() {
            return Err(VeilError::WorkError(format!(
                "工作区名称 '{name}' 不唯一，请使用 --work-root 指定目录"
            )));
        }
        Ok(found)
    }

    pub fn ensure_default_work(&mut self) -> Result<(), VeilError> {
        if self.work.default.is_none() {
            self.work.default = Some(WorkConfig::default_work()?);
        }
        Ok(())
    }

    pub fn register_custom_work(
        &mut self,
        name: String,
        path: PathBuf,
        description: Option<String>,
    ) -> Result<(), VeilError> {
        let mut work = WorkConfig::custom_work(name.clone(), path, description)?;
        if let Some(existing) = self.work.custom.get(&name) {
            work.created_at = existing.created_at.clone();
        }
        self.work.custom.insert(name, work);
        Ok(())
    }

    pub fn resolve_work_root(&self, work: &WorkConfig) -> Result<PathBuf, VeilError> {
        work.resolve_root(&self.config_dir()?)
    }

    /// 显式路径在登记时固定到当前目录；配置中的相对路径只相对于配置目录。
    pub fn register_work_at(
        &mut self,
        path: &Path,
        dedicated: bool,
    ) -> Result<WorkConfig, VeilError> {
        let path = comparable_path(path);
        for existing in self.work.default.iter().chain(self.work.custom.values()) {
            if comparable_path(&self.resolve_work_root(existing)?) == path {
                if dedicated && !matches!(existing.work_type, WorkType::Dedicated) {
                    return Err(VeilError::WorkError(
                        "不能把已登记的共享工作区改为专属工作区".into(),
                    ));
                }
                return Ok(existing.clone());
            }
        }
        // 同盘目录尽量相对于配置目录存储，便于换机或更换挂载点。
        let config_dir = comparable_path(&self.config_dir()?);
        let stored = relative_path(&config_dir, &path).unwrap_or_else(|| path.clone());
        let key = stored.to_string_lossy().into_owned();
        let work = if dedicated {
            WorkConfig::dedicated_work(stored)?
        } else {
            WorkConfig::custom_work(key.clone(), stored, None)?
        };
        self.work.custom.insert(key, work.clone());
        Ok(work)
    }

    /// 根据实际容器判断 ID 是否已被使用；缓存和旧链接不占用容器 ID。
    pub fn ensure_veil_id_available(&self, veil_id: &str) -> Result<(), VeilError> {
        crate::work::validate_veil_id(veil_id)?;
        match find_by_id(&self.all_configs()?, veil_id) {
            Ok(_) => Err(VeilError::VeilIdConflict(format!(
                "容器 ID '{veil_id}' 已存在"
            ))),
            Err(VeilError::VeilNotFound(_)) => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub fn register_veil(&mut self, veil: VeilConfig) -> Result<(), VeilError> {
        crate::work::validate_veil_id(&veil.veil_id)?;
        if self
            .veils
            .values()
            .any(|existing| existing.veil_id == veil.veil_id)
        {
            return Err(VeilError::VeilIdConflict(format!(
                "容器 ID '{}' 已登记",
                veil.veil_id
            )));
        }
        self.veils.insert(veil.veil_id.clone(), veil);
        Ok(())
    }

    pub fn find_veil_key(&self, name_or_id: &str) -> Option<String> {
        if self.veils.contains_key(name_or_id) {
            return Some(name_or_id.to_string());
        }
        let mut matches = self
            .veils
            .iter()
            .filter(|(_, veil)| veil.veil_name == name_or_id || veil.veil_id == name_or_id);
        let (key, _) = matches.next()?;
        matches.next().is_none().then(|| key.clone())
    }

    pub fn get_veil_dir(&self, name_or_id: &str) -> Result<PathBuf, VeilError> {
        Ok(find_container(&self.all_configs()?, name_or_id)?.veil_dir)
    }

    /// 普通命令仅接受 ID、名称、链接；所有入口共用工作区扫描。
    pub fn resolve_veil(&mut self, input: &str) -> Result<ResolvedVeil, VeilError> {
        let path = Path::new(input);
        if is_link_path(path) {
            return if path.try_exists()? {
                self.resolve_link_file(path)
            } else {
                self.resolve_missing_link(path)
            };
        }
        if path.is_dir() {
            return Err(VeilError::InvalidFormat(format!(
                "工作区目录不能直接作为容器参数: {}；请使用 .veil-link 路径",
                path.display()
            )));
        }
        if path.is_file() {
            return Err(VeilError::InvalidFormat(
                if path.extension().and_then(|ext| ext.to_str()) == Some("veil") {
                    format!(
                        "{} 是打包文件，请先运行 veil unpack {}",
                        path.display(),
                        path.display()
                    )
                } else {
                    format!("不支持的容器参数: {input}")
                },
            ));
        }
        find_container(&self.all_configs()?, input)
    }

    /// 重建链接时允许直接输入容器目录，但仍检查已登记工作区中有无副本。
    pub fn resolve_link_target(&mut self, input: &str) -> Result<ResolvedVeil, VeilError> {
        let path = Path::new(input);
        if path.is_dir() && path.join(VEIL_INFO_FILE).is_file() {
            let header = read_veil_header(path)?;
            match find_by_id(&self.all_configs()?, &header.veil_id) {
                Ok(resolved) if comparable_path(&resolved.veil_dir) == comparable_path(path) => {
                    return Ok(resolved);
                }
                Ok(_) => {
                    return Err(VeilError::VeilIdConflict(format!(
                        "容器 ID '{}' 存在多个副本",
                        header.veil_id
                    )));
                }
                Err(VeilError::VeilNotFound(_)) => {}
                Err(error) => return Err(error),
            }
            return Ok(resolved_header(header, std::path::absolute(path)?));
        }
        self.resolve_veil(input)
    }

    pub fn register_link(
        &mut self,
        veil_id: &str,
        link_path: &Path,
    ) -> Result<VeilLink, VeilError> {
        let resolved = find_by_id(&self.all_configs()?, veil_id)?;
        self.register_link_at(veil_id, &resolved.veil_name, &resolved.veil_dir, link_path)
    }

    pub fn register_link_at(
        &mut self,
        veil_id: &str,
        _veil_name: &str,
        veil_dir: &Path,
        link_path: &Path,
    ) -> Result<VeilLink, VeilError> {
        let resolved = find_by_id(&self.all_configs()?, veil_id)?;
        if comparable_path(&resolved.veil_dir) != comparable_path(veil_dir) {
            return Err(VeilError::VeilIdConflict(format!(
                "容器 ID '{veil_id}' 的实际位置不一致"
            )));
        }
        let header = read_veil_header(veil_dir)?;
        let link = VeilLink::new(veil_id, &header.veil_name, &header.created_at);
        link.save(link_path)?;
        self.cache_link_content(link_path, &link);
        self.save()?;
        Ok(link)
    }

    pub fn cache_link(&mut self, link_path: &Path) -> Result<VeilLink, VeilError> {
        let link = VeilLink::load(link_path)?;
        self.cache_link_content(link_path, &link);
        self.save()?;
        Ok(link)
    }

    pub fn resolve_link_veil_dir(
        &self,
        link: &VeilLink,
        _link_path: &Path,
    ) -> Result<PathBuf, VeilError> {
        link.validate()?;
        Ok(find_by_id(&self.all_configs()?, &link.veil.veil_id)?.veil_dir)
    }

    fn cache_link_content(&mut self, link_path: &Path, link: &VeilLink) {
        let link_path = absolute_path(link_path);
        let now = chrono::Utc::now().to_rfc3339();
        if let Some(record) = self
            .links
            .iter_mut()
            .find(|record| record.link_path == link_path)
        {
            record.link = link.clone();
            record.seen_status = LinkStatus::Present;
            record.last_seen_at = now;
        } else {
            self.links.push(LinkRecord {
                link: link.clone(),
                link_path: link_path.clone(),
                seen_status: LinkStatus::Present,
                last_seen_at: now,
            });
        }
        let veil = self
            .veils
            .entry(link.veil.veil_id.clone())
            .or_insert_with(|| VeilConfig {
                veil_id: link.veil.veil_id.clone(),
                veil_name: link.veil.veil_name.clone(),
                created_at: link.veil.created_at.clone(),
                last_accessed: None,
                links: Vec::new(),
            });
        veil.veil_name = link.veil.veil_name.clone();
        if !veil.links.contains(&link_path) {
            veil.links.push(link_path);
        }
    }

    fn resolve_link_file(&mut self, link_path: &Path) -> Result<ResolvedVeil, VeilError> {
        let link = VeilLink::load(link_path)?;
        let mut resolved = find_by_id(&self.all_configs()?, &link.veil.veil_id)?;
        self.cache_link_content(link_path, &link);
        resolved.link_path = Some(link_path.to_path_buf());
        Ok(resolved)
    }

    fn resolve_missing_link(&mut self, link_path: &Path) -> Result<ResolvedVeil, VeilError> {
        let path = absolute_path(link_path);
        let records: Vec<_> = self
            .all_configs()?
            .into_iter()
            .flat_map(|config| config.links)
            .filter(|record| record.link_path == path)
            .collect();
        let record = records.first().ok_or_else(|| {
            VeilError::VeilNotFound(format!(
                "当前未找到容器：链接不存在且没有缓存正文: {}",
                path.display()
            ))
        })?;
        if records
            .iter()
            .any(|other| other.link.veil.veil_id != record.link.veil.veil_id)
        {
            return Err(VeilError::VeilIdConflict(
                "同一链接路径缓存了不同容器 ID".into(),
            ));
        }
        record.link.validate()?;
        // 先确认唯一目标，未挂载或歧义时不恢复链接、不创建任何目录。
        let mut resolved = find_by_id(&self.all_configs()?, &record.link.veil.veil_id)?;
        record.link.save(&path)?;
        self.cache_link_content(&path, &record.link);
        self.save()?;
        resolved.link_path = Some(path);
        resolved.recovered_link = true;
        Ok(resolved)
    }
}

/// 汇总工作区，消除别名及符号链接产生的同目录重复登记。
fn work_roots(configs: &[GlobalConfig]) -> Result<Vec<PathBuf>, VeilError> {
    let mut seen = HashSet::new();
    let mut roots = Vec::new();
    for config in configs {
        for work in config
            .work
            .default
            .iter()
            .chain(config.work.custom.values())
        {
            let root = comparable_path(&config.resolve_work_root(work)?);
            if matches!(work.work_type, WorkType::Dedicated) {
                let entries = match fs::read_dir(&root) {
                    Ok(entries) => Some(entries),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => return Err(error.into()),
                };
                let mut containers = 0;
                if let Some(entries) = entries {
                    for entry in entries {
                        let path = entry?.path();
                        if path.is_dir() && path.join(VEIL_INFO_FILE).try_exists()? {
                            containers += 1;
                        }
                        if containers > 1 {
                            return Err(VeilError::WorkError(format!(
                                "专属工作区只能保存一个容器: {}",
                                root.display()
                            )));
                        }
                    }
                }
            }
            if seen.insert(root.clone()) {
                roots.push(root);
            }
        }
    }
    Ok(roots)
}

fn find_by_id(configs: &[GlobalConfig], veil_id: &str) -> Result<ResolvedVeil, VeilError> {
    crate::work::validate_veil_id(veil_id)?;
    let mut matches = Vec::new();
    let mut seen = HashSet::new();
    for root in work_roots(configs)? {
        let path = root.join(veil_id);
        if !path.try_exists()? {
            continue;
        }
        let header = read_veil_header(&path)?;
        if header.veil_id != veil_id {
            return Err(VeilError::VeilIdConflict(format!(
                "目录 {} 与元数据 ID '{}' 不一致",
                path.display(),
                header.veil_id
            )));
        }
        if seen.insert(comparable_path(&path)) {
            matches.push(resolved_header(header, path));
        }
    }
    match matches.len() {
        0 => Err(VeilError::VeilNotFound(format!(
            "当前未找到容器 '{veil_id}'"
        ))),
        1 => Ok(matches.remove(0)),
        _ => Err(VeilError::VeilIdConflict(format!(
            "容器 ID '{veil_id}' 匹配到多个副本，无法确定目标: {}",
            matches
                .iter()
                .map(|item| item.veil_dir.display().to_string())
                .collect::<Vec<_>>()
                .join("、")
        ))),
    }
}

fn find_container(configs: &[GlobalConfig], input: &str) -> Result<ResolvedVeil, VeilError> {
    if crate::work::validate_veil_id(input).is_ok() {
        match find_by_id(configs, input) {
            Err(VeilError::VeilNotFound(_)) => {}
            result => return result,
        }
    }
    let mut matches = Vec::new();
    let mut seen = HashSet::new();
    for root in work_roots(configs)? {
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let path = entry?.path();
            if !path.is_dir() || !path.join(VEIL_INFO_FILE).try_exists()? {
                continue;
            }
            let header = read_veil_header(&path)?;
            if header.veil_name == input && seen.insert(comparable_path(&path)) {
                matches.push(resolved_header(header, path));
            }
        }
    }
    // 同名副本先报告 ID 歧义，其他同名容器提示使用 ID。
    let mut ids = HashSet::new();
    for item in &matches {
        if !ids.insert(&item.veil_id) {
            return Err(VeilError::VeilIdConflict(format!(
                "容器 ID '{}' 匹配到多个副本",
                item.veil_id
            )));
        }
    }
    match matches.len() {
        0 => Err(VeilError::VeilNotFound(format!("当前未找到容器 '{input}'"))),
        1 => find_by_id(configs, &matches[0].veil_id),
        _ => Err(VeilError::VeilNameAmbiguous(format!(
            "容器名称 '{input}' 匹配到多个容器，请使用容器 ID"
        ))),
    }
}

fn resolved_header(header: MetaHeader, veil_dir: PathBuf) -> ResolvedVeil {
    ResolvedVeil {
        veil_name: header.veil_name,
        veil_id: header.veil_id,
        veil_dir,
        link_path: None,
        recovered_link: false,
    }
}

/// 仅读取有界的明文头，不加载或解密文件清单。
fn read_veil_header(veil_dir: &Path) -> Result<MetaHeader, VeilError> {
    use std::io::Read;
    let mut file = fs::File::open(veil_dir.join(VEIL_INFO_FILE))?;
    let mut prefix = [0u8; 10];
    file.read_exact(&mut prefix)?;
    if &prefix[..8] != crate::metadata::MAGIC {
        return Err(VeilError::InvalidFormat("元数据魔数无效".into()));
    }
    let mut bytes = vec![0; 10 + u16::from_le_bytes([prefix[8], prefix[9]]) as usize];
    bytes[..10].copy_from_slice(&prefix);
    file.read_exact(&mut bytes[10..])?;
    let header = MetaHeader::from_bytes(&bytes)?;
    crate::work::validate_veil_id(&header.veil_id)?;
    if veil_dir.file_name().and_then(|name| name.to_str()) != Some(&header.veil_id) {
        return Err(VeilError::VeilIdConflict(format!(
            "容器目录名必须与元数据 veil_id 一致: {}，实际 ID '{}'",
            veil_dir.display(),
            header.veil_id
        )));
    }
    Ok(header)
}

fn comparable_path(path: &Path) -> PathBuf {
    let absolute = absolute_path(path);
    if let Ok(path) = fs::canonicalize(&absolute) {
        return path;
    }
    // 尚未创建的目录也消除符号链接祖先，并规范化 . 和 ..。
    for ancestor in absolute.ancestors() {
        if let Ok(base) = fs::canonicalize(ancestor) {
            let mut resolved = base;
            for component in absolute.strip_prefix(ancestor).unwrap().components() {
                match component {
                    std::path::Component::ParentDir => {
                        resolved.pop();
                    }
                    std::path::Component::CurDir => {}
                    other => resolved.push(other.as_os_str()),
                }
            }
            return resolved;
        }
    }
    absolute
}

fn relative_path(base: &Path, target: &Path) -> Option<PathBuf> {
    let base: Vec<_> = base.components().collect();
    let target: Vec<_> = target.components().collect();
    if base.first() != target.first() {
        return None;
    }
    let common = base.iter().zip(&target).take_while(|(a, b)| a == b).count();
    let mut result = PathBuf::new();
    for _ in common..base.len() {
        result.push("..");
    }
    for part in &target[common..] {
        result.push(part.as_os_str());
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    Some(result)
}

fn absolute_path(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}
fn is_link_path(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some(LINK_EXTENSION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::AlgorithmId;

    fn config_at(root: &Path) -> GlobalConfig {
        let mut config = GlobalConfig::load_from(&root.join(".veil/config.jsonc")).unwrap();
        config.ensure_default_work().unwrap();
        config
    }

    fn container(root: &Path, id: &str, name: &str) -> PathBuf {
        let path = root.join(id);
        fs::create_dir_all(&path).unwrap();
        let header = MetaHeader::new(
            [8; 32],
            [9; 12],
            AlgorithmId::ChaCha20Poly1305,
            id.into(),
            name.into(),
            "2020-01-02T03:04:05Z".into(),
        );
        fs::write(path.join(VEIL_INFO_FILE), header.to_bytes().unwrap()).unwrap();
        fs::write(path.join("random.enc"), b"unchanged ciphertext").unwrap();
        path
    }

    fn volume(root: &Path) -> VolumeInfo {
        VolumeInfo {
            volume_name: "external".into(),
            root_path: root.into(),
            if_out_volume: true,
        }
    }

    #[test]
    fn discovery_skips_missing_configs_without_creating_files() {
        let temp = tempfile::TempDir::new().unwrap();
        let local = temp.path().join("home/.veil/config.jsonc");
        let disk = temp.path().join("disk");
        fs::create_dir(&disk).unwrap();
        let configs = GlobalConfig::discover_from(&local, &[volume(&disk)]).unwrap();
        assert_eq!(configs.len(), 1);
        assert!(matches!(
            find_by_id(&configs, "veil-missing"),
            Err(VeilError::VeilNotFound(_))
        ));
        assert!(!local.exists());
        assert!(!disk.join(".veil").exists());
    }

    #[test]
    fn disk_config_follows_new_mount_and_new_computer() {
        let temp = tempfile::TempDir::new().unwrap();
        let first_mount = temp.path().join("disk-one");
        let new_mount = temp.path().join("disk-two");
        let disk = config_at(&first_mount);
        let root = disk
            .resolve_work_root(disk.work.default.as_ref().unwrap())
            .unwrap();
        container(&root, "veil-portable", "photos");
        disk.save().unwrap();
        let link_path = temp.path().join("original.veil-link");
        let link = VeilLink::new("veil-portable", "photos", "2020-01-02T03:04:05Z");
        link.save(&link_path).unwrap();
        let original = fs::read(&link_path).unwrap();
        fs::rename(&first_mount, &new_mount).unwrap();
        let local = temp.path().join("new-computer/.veil/config.jsonc");
        let configs = GlobalConfig::discover_from(&local, &[volume(&new_mount)]).unwrap();
        let target =
            find_by_id(&configs, &VeilLink::load(&link_path).unwrap().veil.veil_id).unwrap();
        assert_eq!(
            target.veil_dir,
            comparable_path(&new_mount.join(".veil/works/default/veil-portable"))
        );
        assert_eq!(
            find_container(&configs, "photos").unwrap().veil_dir,
            target.veil_dir
        );
        assert_eq!(fs::read(link_path).unwrap(), original);
        assert!(!local.exists());
        let offline = GlobalConfig::discover_from(&local, &[]).unwrap();
        assert!(matches!(
            find_by_id(&offline, &link.veil.veil_id),
            Err(VeilError::VeilNotFound(_))
        ));
        assert!(!local.exists());
        assert!(!first_mount.exists());
    }

    #[test]
    fn container_moves_across_configs_without_changing_link_or_ciphertext() {
        let temp = tempfile::TempDir::new().unwrap();
        let home = config_at(&temp.path().join("home"));
        let disk_root = temp.path().join("disk");
        let disk = config_at(&disk_root);
        let local_root = home
            .resolve_work_root(home.work.default.as_ref().unwrap())
            .unwrap();
        let remote_root = disk
            .resolve_work_root(disk.work.default.as_ref().unwrap())
            .unwrap();
        let source = container(&local_root, "veil-moving", "photos");
        home.save().unwrap();
        disk.save().unwrap();
        let link = VeilLink::new("veil-moving", "photos", "2020-01-02T03:04:05Z");
        let before = fs::read(source.join(VEIL_INFO_FILE)).unwrap();
        let path = temp.path().join("photos.veil-link");
        link.save(&path).unwrap();
        let original_link = fs::read(&path).unwrap();
        fs::create_dir_all(&remote_root).unwrap();
        fs::rename(source, remote_root.join(&link.veil.veil_id)).unwrap();
        let configs =
            GlobalConfig::discover_from(&home.source_path().unwrap(), &[volume(&disk_root)])
                .unwrap();
        let found = find_by_id(&configs, &link.veil.veil_id).unwrap();
        assert_eq!(
            found.veil_dir,
            comparable_path(&remote_root.join("veil-moving"))
        );
        assert_eq!(
            fs::read(found.veil_dir.join(VEIL_INFO_FILE)).unwrap(),
            before
        );
        assert_eq!(
            fs::read(found.veil_dir.join("random.enc")).unwrap(),
            b"unchanged ciphertext"
        );
        assert_eq!(fs::read(&path).unwrap(), original_link);
    }

    #[test]
    fn duplicates_are_ambiguous_and_aliases_do_not_duplicate_matches() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut config = config_at(temp.path());
        let root = config
            .resolve_work_root(config.work.default.as_ref().unwrap())
            .unwrap();
        container(&root, "veil-one", "photos");
        config
            .register_custom_work("alias".into(), PathBuf::from("works/default"), None)
            .unwrap();
        assert!(find_by_id(&[config.clone()], "veil-one").is_ok());
        config
            .register_custom_work("archive".into(), PathBuf::from("works/archive"), None)
            .unwrap();
        let archive = config.config_dir().unwrap().join("works/archive");
        container(&archive, "veil-two", "photos");
        assert!(matches!(
            find_container(&[config.clone()], "photos"),
            Err(VeilError::VeilNameAmbiguous(_))
        ));
        container(&archive, "veil-one", "photos");
        assert!(matches!(
            find_by_id(&[config.clone()], "veil-one"),
            Err(VeilError::VeilIdConflict(_))
        ));
        assert!(matches!(
            find_container(&[config], "photos"),
            Err(VeilError::VeilIdConflict(_))
        ));
    }

    #[test]
    fn metadata_identity_must_match_directory_name() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut config = config_at(temp.path());
        let root = config
            .resolve_work_root(config.work.default.as_ref().unwrap())
            .unwrap();
        let path = container(&root, "veil-right", "photos");
        fs::rename(path, root.join("veil-wrong")).unwrap();
        assert!(matches!(
            find_by_id(&[config.clone()], "veil-wrong"),
            Err(VeilError::VeilIdConflict(_))
        ));
        assert!(matches!(
            find_container(&[config.clone()], "photos"),
            Err(VeilError::VeilIdConflict(_))
        ));
        assert!(
            config
                .resolve_link_target(root.join("veil-wrong").to_str().unwrap())
                .is_err()
        );
    }

    #[test]
    fn configuration_has_no_work_or_volume_identity() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut config = config_at(temp.path());
        config
            .register_work_at(&temp.path().join("archive"), false)
            .unwrap();
        config.save().unwrap();
        let content = fs::read_to_string(config.source_path().unwrap()).unwrap();
        for field in [
            "work_id",
            "volume_id",
            "volumes",
            "root_path",
            "source_path",
        ] {
            assert!(!content.contains(field));
        }
        let loaded = GlobalConfig::load_from(&config.source_path().unwrap()).unwrap();
        assert_eq!(
            loaded.work.default.unwrap().path,
            Path::new("works/default")
        );
        assert!(!temp.path().join("archive").exists());
    }
    #[test]
    fn dedicated_work_rejects_multiple_containers() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut config = config_at(temp.path());
        let root = temp.path().join("dedicated");
        config.register_work_at(&root, true).unwrap();
        container(&root, "veil-one", "photos");
        assert!(find_by_id(&[config.clone()], "veil-one").is_ok());
        container(&root, "veil-two", "documents");
        assert!(matches!(
            find_by_id(&[config], "veil-one"),
            Err(VeilError::WorkError(_))
        ));
    }

    #[test]
    fn name_lookup_uses_header_instead_of_stale_cache() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut config = config_at(temp.path());
        let root = config
            .resolve_work_root(config.work.default.as_ref().unwrap())
            .unwrap();
        container(&root, "veil-one", "new-name");
        config
            .register_veil(VeilConfig {
                veil_id: "veil-one".into(),
                veil_name: "old-name".into(),
                created_at: "2020-01-02T03:04:05Z".into(),
                last_accessed: None,
                links: Vec::new(),
            })
            .unwrap();
        assert_eq!(
            find_container(&[config.clone()], "new-name")
                .unwrap()
                .veil_id,
            "veil-one"
        );
        assert!(matches!(
            find_container(&[config], "old-name"),
            Err(VeilError::VeilNotFound(_))
        ));
    }
}
