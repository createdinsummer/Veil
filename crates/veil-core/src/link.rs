//! `.veil-link` 链接文件模型。
//!
//! `.veil-link` 是当前有效的便携定位文件：它指向工作区内的容器目录，
//! 并携带容器 ID、展示名称、卷 ID 和基础加密信息。链接本身不保存文件内容。

use crate::error::VeilError;
use crate::volume;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const LINK_EXTENSION: &str = "veil-link";

/// 便携定位文件，可复制到其他位置或由 `config.toml` 原样恢复。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VeilLink {
    pub version: String,
    pub created_at: String,
    pub workspace: LinkWorkspace,
    pub encryption: LinkEncryption,
    pub metadata: LinkMetadata,
}

/// 容器目录的位置和基础身份信息。
///
/// `path` 相对卷根目录保存，不写入容器自己的 `.veil-meta`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkWorkspace {
    pub veil_id: String,
    pub container_name: String,

    /// 工作区相对于卷根目录的路径。
    pub path: PathBuf,

    /// 稳定卷 ID。
    pub volume_id: String,

    pub volume_label: String,

    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkEncryption {
    pub algorithm: String,
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
        veil_id: impl Into<String>,
        container_name: impl Into<String>,
        relative_path: PathBuf,
        volume_id: impl Into<String>,
        volume_label: impl Into<String>,
    ) -> Self {
        let now = now();
        Self {
            version: default_version(),
            created_at: now.clone(),
            workspace: LinkWorkspace {
                veil_id: veil_id.into(),
                container_name: container_name.into(),
                path: relative_path,
                volume_id: volume_id.into(),
                volume_label: volume_label.into(),
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
        let link_volume = volume::volume_for_path(link_path)?;
        if link_volume.volume_id == self.workspace.volume_id {
            return Ok(link_volume.mount_path.join(&self.workspace.path));
        }

        Err(VeilError::VolumeUnavailable(format!(
            "卷 {} ({}) 未挂载；链接位于卷 {}",
            self.workspace.volume_id, self.workspace.volume_label, link_volume.volume_label
        )))
    }

    pub fn resolve_workspace_path_with_mount(
        &self,
        link_path: &Path,
        mount_path: Option<&Path>,
    ) -> Result<PathBuf, VeilError> {
        if let Some(mount_path) = mount_path {
            return Ok(mount_path.join(&self.workspace.path));
        }

        self.resolve_workspace_path(link_path)
    }

    pub fn container_name(&self) -> String {
        self.workspace.container_name.clone()
    }
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn default_version() -> String {
    "1.0".to_string()
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
            "veil-1234",
            "photos",
            PathBuf::from(".veil/workspaces/default/photos"),
            "fs:abcd",
            "MyUSB",
        );

        link.save(&link_path).unwrap();
        let loaded = VeilLink::load(&link_path).unwrap();

        assert_eq!(loaded.workspace.veil_id, "veil-1234");
        assert_eq!(loaded.container_name(), "photos");
    }
}
