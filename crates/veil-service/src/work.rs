//! 工作区只是存放容器的目录，不具有独立身份。

use crate::error::VeilError;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

/// 工作区的组织类型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkType {
    /// 默认工作区，可存放多个容器。
    Default,
    /// 命名工作区，可存放多个容器。
    Custom(String),
    /// 专属工作区，只允许一个容器。
    Dedicated,
}

/// 工作区目录及其配置；相对路径基于所属配置文件的目录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkConfig {
    pub path: PathBuf,
    pub work_type: WorkType,
    pub description: Option<String>,
    pub created_at: String,
}

impl WorkConfig {
    pub fn default_work() -> Result<Self, VeilError> {
        Self::from_path(PathBuf::from("works/default"), WorkType::Default, None)
    }

    pub fn custom_work(
        name: String,
        path: PathBuf,
        description: Option<String>,
    ) -> Result<Self, VeilError> {
        Self::from_path(path, WorkType::Custom(name), description)
    }

    pub fn dedicated_work(path: PathBuf) -> Result<Self, VeilError> {
        Self::from_path(path, WorkType::Dedicated, None)
    }

    fn from_path(
        path: PathBuf,
        work_type: WorkType,
        description: Option<String>,
    ) -> Result<Self, VeilError> {
        let work = Self {
            path,
            work_type,
            description,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        work.validate_path()?;
        Ok(work)
    }

    pub(crate) fn validate_path(&self) -> Result<(), VeilError> {
        if self.path.as_os_str().is_empty()
            || (!self.path.is_absolute()
                && self
                    .path
                    .components()
                    .any(|part| matches!(part, Component::Prefix(_) | Component::RootDir)))
        {
            return Err(VeilError::WorkError(
                "工作区路径必须是绝对路径或相对于配置目录的路径".to_string(),
            ));
        }
        Ok(())
    }

    /// 不读取当前目录，不创建目录；允许绝对路径及相对于配置目录的路径。
    pub fn resolve_root(&self, config_dir: &Path) -> Result<PathBuf, VeilError> {
        self.validate_path()?;
        if !config_dir.is_absolute() {
            return Err(VeilError::WorkError("配置目录必须是绝对路径".to_string()));
        }
        Ok(config_dir.join(&self.path))
    }

    pub fn default_work_root() -> Result<PathBuf, VeilError> {
        Ok(crate::config::GlobalConfig::config_path()?
            .parent()
            .unwrap()
            .join("works/default"))
    }
}

/// 容器 ID 同时作为目录名，必须是单个安全路径段。
pub fn validate_veil_id(veil_id: &str) -> Result<(), VeilError> {
    let mut parts = Path::new(veil_id).components();
    if veil_id.trim().is_empty()
        || !matches!(parts.next(), Some(Component::Normal(_)))
        || parts.next().is_some()
        || veil_id.contains(['/', '\\'])
    {
        return Err(VeilError::InvalidFormat(
            "容器 ID 必须是单个安全目录名".to_string(),
        ));
    }
    Ok(())
}

/// 将 Veil 名称转换为可安全用于文件名和展示后缀的形式。
///
/// 名称最多保留 80 个字符，控制字符和常见路径保留字符替换为 `-`，并移除首尾的
/// 空白、点或连字符；清理后为空时回退为 `veil`。
fn safe_veil_name(veil_name: &str) -> String {
    // 先限制长度并逐字符替换文件系统不适合保留的符号。
    let mut safe_name: String = veil_name
        .chars()
        .take(80)
        .map(|character| {
            // 控制字符和路径保留字符不能进入默认链接文件名。
            if character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
            {
                '-'
            } else {
                character
            }
        })
        .collect();
    // 清理首尾分隔符号，避免生成隐藏文件或以分隔符结尾的名称。
    safe_name = safe_name.trim_matches(['-', '.', ' ']).to_string();
    if safe_name.is_empty() {
        safe_name = "veil".to_string();
    }

    safe_name
}

/// 为同名 Veil 生成包含清理后名称和创建时间的名称段。
pub fn veil_name_with_suffix(veil_name: &str, creation_time: &str) -> String {
    format!("{}-{}", safe_veil_name(veil_name), creation_time)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_relative_to_config_directory() {
        let dir = tempfile::TempDir::new().unwrap();
        let work = WorkConfig::default_work().unwrap();
        assert_eq!(work.path, Path::new("works/default"));
        assert_eq!(
            work.resolve_root(dir.path()).unwrap(),
            dir.path().join("works/default")
        );
        assert!(!dir.path().join("works").exists());
        let custom =
            WorkConfig::custom_work("archive".into(), PathBuf::from("../archive"), None).unwrap();
        assert_eq!(
            custom.resolve_root(dir.path()).unwrap(),
            dir.path().join("../archive")
        );
        let absolute = WorkConfig::dedicated_work(dir.path().to_path_buf()).unwrap();
        assert_eq!(absolute.resolve_root(dir.path()).unwrap(), dir.path());
    }
}
