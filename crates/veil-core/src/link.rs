//! `.veil-link` 便携定位文件模型。
//!
//! 链接文件保存 Veil 所在卷、相对卷根的路径以及展示信息，不保存任何文件内容或密码。
//! 它可被复制到其他位置，并可通过配置中的原始字节副本恢复；容器身份始终以
//! `veil_id` 为准，`veil_dir` 变化不会改变身份。

use crate::error::VeilError;
use crate::volume;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// `.veil-link` 文件扩展名。
pub const LINK_EXTENSION: &str = "veil-link";

/// 便携定位文件，可复制到其他位置或由 `config.toml` 原样恢复。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VeilLink {
    /// 链接文件格式版本。
    pub version: String,
    /// RFC 3339 格式的链接创建时间。
    pub created_at: String,
    /// Veil 目录位置与容器身份信息。
    pub veil: LinkVeil,
    /// 容器使用的加密方案描述。
    pub encryption: LinkEncryption,
    /// 可选展示元数据。
    pub metadata: LinkMetadata,
}

/// Veil 目录的位置和基础身份信息。
///
/// `path` 相对卷根目录保存，不写入容器自己的 `.veil-meta`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkVeil {
    /// 不随路径或展示名称变化的容器稳定 ID。
    pub veil_id: String,
    /// 面向用户的容器名称。
    pub veil_name: String,

    /// `veil_dir` 相对于卷根目录的路径。
    pub path: PathBuf,

    /// 稳定卷 ID。
    pub volume_id: String,

    /// 链接创建时记录的卷展示名称。
    pub volume_label: String,

    /// RFC 3339 格式的 Veil 目录记录创建时间。
    pub created_at: String,
}

/// 链接中记录的算法标识。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkEncryption {
    /// 内容加密算法名称。
    pub algorithm: String,
    /// 密钥派生函数名称。
    pub key_derivation: String,
}

impl Default for LinkEncryption {
    /// 使用链接模型当前默认的算法标识构造加密说明。
    fn default() -> Self {
        Self {
            algorithm: default_algorithm(),
            key_derivation: default_key_derivation(),
        }
    }
}

/// 可选的链接展示信息。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LinkMetadata {
    /// 用户提供的说明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 用户用于分类或检索的标签。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl VeilLink {
    /// 使用 Veil 身份、`veil_dir` 相对路径和卷信息构造新链接。
    pub fn new(
        veil_id: impl Into<String>,
        veil_name: impl Into<String>,
        relative_path: PathBuf,
        volume_id: impl Into<String>,
        volume_label: impl Into<String>,
    ) -> Self {
        // 链接创建时间与 Veil 目录记录时间使用同一次采样，保证首次序列化一致。
        let now = now();
        Self {
            version: default_version(),
            created_at: now.clone(),
            veil: LinkVeil {
                veil_id: veil_id.into(),
                veil_name: veil_name.into(),
                path: relative_path,
                volume_id: volume_id.into(),
                volume_label: volume_label.into(),
                created_at: now,
            },
            encryption: LinkEncryption::default(),
            metadata: LinkMetadata::default(),
        }
    }

    /// 从磁盘读取并解析 TOML 格式的链接文件。
    ///
    /// # 错误
    /// 文件读取失败时返回 [`VeilError::ConfigError`]；TOML 无效或字段不匹配时返回
    /// [`VeilError::InvalidFormat`]。
    pub fn load(path: &Path) -> Result<Self, VeilError> {
        // 先把 I/O 错误和格式错误分层，便于调用方决定是修复路径还是修复内容。
        let content = fs::read_to_string(path).map_err(|error| {
            VeilError::ConfigError(format!("读取链接文件失败 {}: {}", path.display(), error))
        })?;

        let link: Self = toml::from_str(&content).map_err(|error| {
            VeilError::InvalidFormat(format!("链接文件格式错误 {}: {}", path.display(), error))
        })?;
        link.validate()?;
        Ok(link)
    }

    /// 将链接序列化为 TOML 并写入指定路径，缺少父目录时会自动创建。
    ///
    /// # 错误
    /// 父目录创建、序列化或文件写入失败时返回相应错误。
    pub fn save(&self, path: &Path) -> Result<(), VeilError> {
        self.validate()?;

        let content = toml::to_string_pretty(self).map_err(|error| {
            VeilError::SerializationError(format!("序列化链接文件失败: {}", error))
        })?;

        // 链接允许写入尚不存在的子目录；原子替换保证中断时不会留下半截链接。
        crate::fsutil::atomic_write(path, content.as_bytes())?;
        Ok(())
    }

    /// 根据链接所在位置解析其 `veil_dir`。
    ///
    /// 只有链接当前所在卷的 ID 与链接记录一致时才允许拼接相对路径。
    ///
    /// # 错误
    /// 卷识别失败或链接与 Veil 目录不在同一卷时返回错误。
    pub fn resolve_veil_dir(&self, link_path: &Path) -> Result<PathBuf, VeilError> {
        // 先确认链接当前落在目标卷，避免把相对路径错误拼到另一块磁盘。
        let link_volume = volume::volume_for_path(link_path)?;
        if link_volume.volume_id == self.veil.volume_id {
            return Ok(link_volume.mount_path.join(&self.veil.path));
        }

        Err(VeilError::VolumeUnavailable(format!(
            "卷 {} ({}) 未挂载；链接位于卷 {}",
            self.veil.volume_id, self.veil.volume_label, link_volume.volume_label
        )))
    }

    /// 优先使用调用方提供的挂载路径解析 `veil_dir`。
    ///
    /// `mount_path` 存在时直接与其拼接；否则回退到按链接所在卷解析。
    ///
    /// # 错误
    /// 未提供挂载路径且卷校验失败时返回错误。
    pub fn resolve_veil_dir_with_mount(
        &self,
        link_path: &Path,
        mount_path: Option<&Path>,
    ) -> Result<PathBuf, VeilError> {
        // 配置缓存提供挂载点时可跳过卷探测，适用于卷 ID 已注册的场景。
        if let Some(mount_path) = mount_path {
            let mounted_volume = volume::volume_for_path(mount_path)?;
            if mounted_volume.volume_id != self.veil.volume_id {
                return Err(VeilError::VolumeUnavailable(format!(
                    "卷 {} ({}) 当前不可用；缓存挂载点属于卷 {}",
                    self.veil.volume_id, self.veil.volume_label, mounted_volume.volume_label
                )));
            }
            return Ok(mounted_volume.mount_path.join(&self.veil.path));
        }

        // 没有缓存挂载点时必须回到链接自身的卷身份校验。
        self.resolve_veil_dir(link_path)
    }

    /// 返回链接记录的 Veil 展示名称。
    pub fn veil_name(&self) -> String {
        self.veil.veil_name.clone()
    }

    /// 校验链接身份和 `veil_dir` 相对路径。
    fn validate(&self) -> Result<(), VeilError> {
        if self.veil.veil_id.trim().is_empty() {
            return Err(VeilError::InvalidFormat("链接缺少 veil_id".to_string()));
        }
        if self.veil.veil_name.trim().is_empty() {
            return Err(VeilError::InvalidFormat("链接缺少容器名称".to_string()));
        }
        if self.veil.volume_id.trim().is_empty() {
            return Err(VeilError::InvalidFormat("链接缺少 volume_id".to_string()));
        }

        let mut has_normal_component = false;
        for component in self.veil.path.components() {
            match component {
                Component::Normal(_) => has_normal_component = true,
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(VeilError::InvalidFormat(format!(
                        "链接中的 Veil 目录必须是安全的相对路径: {}",
                        self.veil.path.display()
                    )));
                }
            }
        }
        if !has_normal_component {
            return Err(VeilError::InvalidFormat(
                "链接中的 Veil 目录不能为空".to_string(),
            ));
        }
        Ok(())
    }
}

/// 返回当前 UTC 时间的 RFC 3339 表示。
fn now() -> String {
    Utc::now().to_rfc3339()
}

/// 返回当前链接格式版本。
fn default_version() -> String {
    "1.0".to_string()
}

/// 返回链接中默认记录的加密算法名称。
fn default_algorithm() -> String {
    "ChaCha20-Poly1305".to_string()
}

/// 返回链接中默认记录的密钥派生函数名称。
fn default_key_derivation() -> String {
    "Argon2id".to_string()
}

/// `.veil-link` 序列化与路径解析的单元测试。
#[cfg(test)]
mod tests {
    use super::*;

    /// 验证链接文件保存后可以完整加载。
    #[test]
    fn round_trip_link_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let link_path = dir.path().join("photos.veil-link");
        let link = VeilLink::new(
            "veil-1234",
            "photos",
            PathBuf::from(".veil/works/default/photos"),
            "fs:abcd",
            "MyUSB",
        );

        link.save(&link_path).unwrap();
        let loaded = VeilLink::load(&link_path).unwrap();

        assert_eq!(loaded.veil.veil_id, "veil-1234");
        assert_eq!(loaded.veil_name(), "photos");
        assert_eq!(loaded.encryption.algorithm, "ChaCha20-Poly1305");
    }

    /// 验证绝对路径、父目录穿越和空路径在保存与加载时都会被拒绝。
    #[test]
    fn rejects_unsafe_veil_dirs() {
        let dir = tempfile::TempDir::new().unwrap();

        for invalid_path in [
            PathBuf::from("/absolute/work"),
            PathBuf::from("../outside"),
            PathBuf::from("safe/../../outside"),
            PathBuf::new(),
        ] {
            let link = VeilLink::new("veil-1234", "photos", invalid_path, "fs:abcd", "MyUSB");
            assert!(link.save(&dir.path().join("unsafe.veil-link")).is_err());
        }
    }

    /// 验证加载被手工改写的绝对路径链接时也会拒绝。
    #[test]
    fn load_rejects_absolute_veil_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let link_path = dir.path().join("unsafe.veil-link");
        fs::write(
            &link_path,
            r#"version = "1.0"
created_at = "2026-01-01T00:00:00+00:00"

[veil]
veil_id = "veil-1234"
veil_name = "photos"
path = "/absolute/work"
volume_id = "fs:abcd"
volume_label = "MyUSB"
created_at = "2026-01-01T00:00:00+00:00"

[encryption]
algorithm = "ChaCha20-Poly1305"
key_derivation = "Argon2id"

[metadata]
"#,
        )
        .unwrap();

        assert!(VeilLink::load(&link_path).is_err());
    }

    /// 验证缓存挂载点属于其他卷时不会继续拼接工作区路径。
    #[test]
    fn rejects_cached_mount_from_other_volume() {
        let dir = tempfile::TempDir::new().unwrap();
        let link_path = dir.path().join("photos.veil-link");
        let link = VeilLink::new(
            "veil-1234",
            "photos",
            PathBuf::from(".veil/works/default/photos"),
            "fs:definitely-wrong",
            "MissingDisk",
        );

        assert!(matches!(
            link.resolve_veil_dir_with_mount(&link_path, Some(dir.path())),
            Err(VeilError::VolumeUnavailable(_))
        ));
    }

    /// 验证 Unix 下链接文件权限不会向同组或其他用户开放。
    #[cfg(unix)]
    #[test]
    fn link_file_is_private_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        let link_path = dir.path().join("private.veil-link");
        let link = VeilLink::new(
            "veil-1234",
            "photos",
            PathBuf::from(".veil/works/default/photos"),
            "fs:abcd",
            "MyUSB",
        );
        link.save(&link_path).unwrap();

        let mode = fs::metadata(&link_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
