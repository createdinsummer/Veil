//! `.veil-link` 链接文件模型。
//!
//! 链接文件是用户日常操作的轻量入口，指向真正保存加密数据的工作区目录。

use crate::error::VeilError;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const LINK_EXTENSION: &str = "veil-link";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VeilLink {
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default = "now")]
    pub created_at: String,
    pub workspace: LinkWorkspace,
    #[serde(default)]
    pub encryption: LinkEncryption,
    #[serde(default)]
    pub metadata: LinkMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkWorkspace {
    #[serde(default)]
    pub container_name: String,
    pub path: PathBuf,
    #[serde(default = "default_workspace_type")]
    pub workspace_type: String,
    #[serde(default)]
    pub dedicated: bool,
    #[serde(default = "now")]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkEncryption {
    #[serde(default = "default_algorithm")]
    pub algorithm: String,
    #[serde(default = "default_key_derivation")]
    pub key_derivation: String,
}

impl Default for LinkEncryption {
    fn default() -> Self {
        Self {
            algorithm: default_algorithm(),
            key_derivation: default_key_derivation(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LinkMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl VeilLink {
    pub fn new(
        container_name: impl Into<String>,
        workspace_path: PathBuf,
        workspace_type: impl Into<String>,
        dedicated: bool,
    ) -> Self {
        let now = now();
        Self {
            version: default_version(),
            created_at: now.clone(),
            workspace: LinkWorkspace {
                container_name: container_name.into(),
                path: workspace_path,
                workspace_type: workspace_type.into(),
                dedicated,
                created_at: now,
            },
            encryption: LinkEncryption::default(),
            metadata: LinkMetadata::default(),
        }
    }

    pub fn load(path: &Path) -> Result<Self, VeilError> {
        let content = fs::read_to_string(path).map_err(|error| {
            VeilError::ConfigError(format!("读取链接文件失败 {}: {}", path.display(), error))
        })?;

        toml::from_str(&content).map_err(|error| {
            VeilError::InvalidFormat(format!("链接文件格式错误 {}: {}", path.display(), error))
        })
    }

    pub fn save(&self, path: &Path) -> Result<(), VeilError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = toml::to_string_pretty(self).map_err(|error| {
            VeilError::SerializationError(format!("序列化链接文件失败: {}", error))
        })?;

        fs::write(path, content)?;
        Ok(())
    }

    pub fn resolve_workspace_path(&self, link_path: &Path) -> Result<PathBuf, VeilError> {
        let path = expand_tilde(&self.workspace.path)?;
        if path.is_absolute() {
            Ok(path)
        } else {
            let parent = link_path.parent().unwrap_or_else(|| Path::new("."));
            Ok(parent.join(path))
        }
    }

    pub fn container_name(&self, link_path: &Path) -> String {
        if !self.workspace.container_name.trim().is_empty() {
            return self.workspace.container_name.clone();
        }

        link_path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("container")
            .to_string()
    }
}

pub fn portable_workspace_path(path: &Path) -> PathBuf {
    let Some(home) = dirs::home_dir() else {
        return path.to_path_buf();
    };

    match path.strip_prefix(&home) {
        Ok(relative) => PathBuf::from("~").join(relative),
        Err(_) => path.to_path_buf(),
    }
}

fn expand_tilde(path: &Path) -> Result<PathBuf, VeilError> {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return dirs::home_dir()
            .ok_or_else(|| VeilError::ConfigError("无法获取用户主目录".to_string()));
    }

    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        let home = dirs::home_dir()
            .ok_or_else(|| VeilError::ConfigError("无法获取用户主目录".to_string()))?;
        return Ok(home.join(rest));
    }

    Ok(path.to_path_buf())
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn default_version() -> String {
    "1.0".to_string()
}

fn default_workspace_type() -> String {
    "default".to_string()
}

fn default_algorithm() -> String {
    "AES-256-GCM".to_string()
}

fn default_key_derivation() -> String {
    "Argon2id".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_link_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let link_path = dir.path().join("photos.veil-link");
        let link = VeilLink::new(
            "photos",
            PathBuf::from("~/.veil/workspaces/default/photos"),
            "default",
            false,
        );

        link.save(&link_path).unwrap();
        let loaded = VeilLink::load(&link_path).unwrap();

        assert_eq!(loaded.container_name(&link_path), "photos");
        assert_eq!(loaded.workspace.workspace_type, "default");
    }

    #[test]
    fn resolves_relative_workspace_from_link_directory() {
        let dir = tempfile::TempDir::new().unwrap();
        let link_path = dir.path().join("photos.veil-link");
        let link = VeilLink::new(
            "photos",
            PathBuf::from("workspaces/photos"),
            "default",
            false,
        );

        assert_eq!(
            link.resolve_workspace_path(&link_path).unwrap(),
            dir.path().join("workspaces/photos")
        );
    }
}
