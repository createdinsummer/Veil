//! 本机数据库访问门面。公开视图用于展示，不序列化整体配置或读取旧 JSONC。
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HintLevel {
    Full,
    Brief,
    Off,
}
impl HintLevel {
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
pub struct UserSettings {
    pub hint_level: HintLevel,
}
#[derive(Debug, Clone, Default)]
pub struct WorkSection {
    pub default: Option<WorkConfig>,
    pub custom: HashMap<String, WorkConfig>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VeilConfig {
    pub veil_id: String,
    pub veil_name: String,
    pub created_at: String,
    pub last_accessed: Option<String>,
    pub links: Vec<PathBuf>,
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
#[serde(deny_unknown_fields)]
pub struct LinkRecord {
    pub link: VeilLink,
    pub link_path: PathBuf,
    pub seen_status: LinkStatus,
    pub last_seen_at: String,
}
#[derive(Debug, Clone)]
pub struct ResolvedVeil {
    pub veil_name: String,
    pub veil_id: String,
    pub veil_dir: PathBuf,
    pub link_path: Option<PathBuf>,
    pub recovered_link: bool,
}
#[derive(Clone, Debug)]
pub struct AppConfig {
    directory: PathBuf,
    pub work: WorkSection,
    pub veils: HashMap<String, VeilConfig>,
    pub links: Vec<LinkRecord>,
    pub user_settings: UserSettings,
}
#[derive(Serialize, Deserialize)]
struct IdentityCache {
    created_at: String,
}
impl AppConfig {
    pub fn config_path() -> Result<PathBuf> {
        Ok(crate::storage::app::app_data_dir()?.join(veil_core::database::APP_DB))
    }
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::config_path()?)
    }
    pub fn load_from(path: &Path) -> Result<Self> {
        if path.file_name().and_then(|s| s.to_str()) != Some("app.db") {
            return Err(invalid("仅支持 app.db，不读取旧配置"));
        }
        let directory = crate::storage::canonical_path(path)?
            .parent()
            .ok_or_else(|| invalid("数据库目录无效"))?
            .to_path_buf();
        let mut store = AppStore::open(&directory)?;
        let default = store.default_space()?;
        let spaces = store.spaces()?;
        let mut work = WorkSection::default();
        for space in spaces {
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
                if space.id == default.id {
                    work.default = Some(w);
                } else {
                    work.custom.insert(space.id, w);
                }
            }
        }
        let registrations = store.registrations()?;
        let mut veils = HashMap::new();
        for r in registrations {
            veils.insert(
                r.div_id.clone(),
                VeilConfig {
                    veil_id: r.div_id.clone(),
                    veil_name: r.cached_name,
                    created_at: store
                        .record::<IdentityCache>("div_state", &r.div_id)?
                        .map(|c| c.created_at)
                        .unwrap_or_default(),
                    last_accessed: None,
                    links: Vec::new(),
                },
            );
        }
        let level = store.user_setting("hint_level")?;
        let hint_level = HintLevel::parse(level.as_str().unwrap_or(""))
            .ok_or_else(|| invalid("提示级别无效"))?;
        let links = store.records("link_cache", u32::MAX)?;
        Ok(Self {
            directory,
            work,
            veils,
            links,
            user_settings: UserSettings { hint_level },
        })
    }
    fn store(&self) -> Result<AppStore> {
        AppStore::open(&self.directory)
    }
    pub fn source_path(&self) -> Result<PathBuf> {
        Ok(self.directory.join("app.db"))
    }
    pub fn config_dir(&self) -> Result<PathBuf> {
        Ok(self.directory.clone())
    }
    pub fn for_location(_location: &Path) -> Result<Self> {
        Self::load()
    }
    pub fn ensure_default_work(&mut self) -> Result<()> {
        self.store()?.default_space()?;
        Ok(())
    }
    pub fn creation_work(
        _location: &Path,
        name: Option<&str>,
        root: Option<&Path>,
        dedicated: bool,
    ) -> Result<(Self, WorkConfig)> {
        let mut config = Self::load()?;
        let mut store = config.store()?;
        let space = if let Some(root) = root {
            let root = std::path::absolute(root)?;
            crate::fsutil::create_dir_all_durable(&root)?;
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
        } else if let Some(name) = name.filter(|n| *n != "default") {
            find_space(&store.spaces()?, name)?
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
        config = Self::load_from(&config.source_path()?)?;
        Ok((config, w))
    }
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
    pub fn resolve_work_root(&self, work: &WorkConfig) -> Result<PathBuf> {
        work.resolve_root(&self.directory)
    }
    pub fn register_work_at(&mut self, path: &Path, dedicated: bool) -> Result<WorkConfig> {
        crate::fsutil::create_dir_all_durable(path)?;
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
    pub fn ensure_veil_id_available(&self, identity: &str) -> Result<()> {
        crate::work::validate_veil_id(identity)?;
        if self
            .store()?
            .registrations()?
            .iter()
            .any(|r| r.div_id == identity)
        {
            return Err(VeilError::VeilIdConflict(format!("容器 {identity} 已登记")));
        }
        for s in self.store()?.spaces()? {
            if s.local_path.is_some_and(|p| p.join(identity).exists()) {
                return Err(VeilError::VeilIdConflict("发现相同 ID 的实际副本".into()));
            }
        }
        Ok(())
    }
    pub fn register_directory(
        &mut self,
        directory: &Path,
        identity: &str,
        name: &str,
        created: &str,
    ) -> Result<()> {
        let directory = std::fs::canonicalize(directory)?;
        if directory.file_name().and_then(|n| n.to_str()) != Some(identity) {
            return Err(invalid("容器目录名称必须与真实 ID 一致"));
        }
        let parent = directory
            .parent()
            .ok_or_else(|| invalid("容器缺少空间目录"))?;
        let mut store = self.store()?;
        let existing = store
            .registrations()?
            .into_iter()
            .find(|r| r.div_id == identity);
        let spaces = store.spaces()?;
        for s in &spaces {
            if let Some(root) = &s.local_path {
                let candidate = root.join(identity);
                if candidate.is_dir() && std::fs::canonicalize(&candidate)? != directory {
                    return Err(VeilError::VeilIdConflict("同 ID 存在多个实际副本".into()));
                }
            }
        }
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
    pub fn cached_identity(&self, identity: &str) -> Result<String> {
        self.store()?
            .record::<IdentityCache>("div_state", identity)?
            .map(|c| c.created_at)
            .ok_or_else(|| invalid("本机缓存不足，请先解锁容器"))
    }
    pub fn find_veil_key(&self, input: &str) -> Option<String> {
        self.veils
            .values()
            .find(|v| v.veil_id == input || v.veil_name == input)
            .map(|v| v.veil_id.clone())
    }
    pub fn get_veil_dir(&self, input: &str) -> Result<PathBuf> {
        Ok(self.find_container(input)?.veil_dir)
    }
    fn find_container(&self, input: &str) -> Result<ResolvedVeil> {
        let store = self.store()?;
        let spaces = store.spaces()?;
        let mut matches = Vec::new();
        for r in store.registrations()? {
            if r.div_id != input && r.cached_name != input {
                continue;
            }
            let space = spaces
                .iter()
                .find(|s| s.id == r.space_id)
                .ok_or_else(|| invalid("空间引用无效"))?;
            let root = space
                .local_path
                .as_ref()
                .ok_or_else(|| VeilError::VolumeUnavailable("空间尚未绑定目录".into()))?;
            let directory = root.join(&r.div_id);
            if !directory.join(veil_core::database::CONTAINER_DB).is_file() {
                let _ = store.observe_unavailable(&r.div_id);
                return Err(VeilError::VolumeUnavailable("容器目录当前不可用".into()));
            }
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
        if matches.len() > 1 {
            return Err(VeilError::VeilNameAmbiguous(input.into()));
        }
        matches
            .pop()
            .ok_or_else(|| VeilError::VeilNotFound(input.into()))
    }
    pub fn resolve_directory(&self, path: &Path) -> Result<ResolvedVeil> {
        let path = std::fs::canonicalize(path)?;
        let identity = path
            .file_name()
            .and_then(|p| p.to_str())
            .ok_or_else(|| invalid("目录名称无效"))?;
        let resolved = self.find_container(identity)?;
        if std::fs::canonicalize(&resolved.veil_dir)? != path {
            return Err(VeilError::VeilIdConflict(
                "登记目录与实际目录不一致，请先解锁登记".into(),
            ));
        }
        Ok(resolved)
    }
    pub fn resolve_veil(&mut self, input: &str) -> Result<ResolvedVeil> {
        let path = Path::new(input);
        if path.extension().and_then(|p| p.to_str()) == Some(LINK_EXTENSION) {
            let absolute = crate::storage::canonical_path(path)?;
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
            let mut resolved = self.find_container(&link.veil.veil_id)?;
            if recovered {
                match link.save_new(&absolute) {
                    Ok(()) => {}
                    Err(VeilError::Io(error))
                        if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
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
        if path.is_dir() {
            return self.resolve_directory(path);
        }
        self.find_container(input)
    }
    pub fn resolve_link_target(&mut self, input: &str) -> Result<ResolvedVeil> {
        self.resolve_veil(input)
    }
    pub fn register_link(&mut self, identity: &str, name: &str, path: &Path) -> Result<()> {
        let directory = self.get_veil_dir(identity)?;
        self.register_link_at(identity, name, &directory, path)
    }
    pub fn register_link_at(
        &mut self,
        identity: &str,
        name: &str,
        _directory: &Path,
        path: &Path,
    ) -> Result<()> {
        let link = VeilLink::new(identity, name, self.cached_identity(identity)?);
        if path.exists() {
            return Err(invalid("链接已存在"));
        }
        link.save_new(path)?;
        if let Err(e) = self.cache_link(path) {
            let _ = std::fs::remove_file(path);
            return Err(e);
        }
        Ok(())
    }
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
    pub fn resolve_link_veil_dir(&mut self, path: &Path) -> Result<PathBuf> {
        Ok(self
            .resolve_veil(path.to_str().ok_or_else(|| invalid("路径无效"))?)?
            .veil_dir)
    }
}
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

#[derive(Serialize)]
pub struct ConfigSummary {
    pub spaces: Vec<crate::spaces::SpaceView>,
    pub default_space_id: String,
    pub container_count: usize,
    pub link_count: usize,
}
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
