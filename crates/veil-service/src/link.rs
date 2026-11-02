//! `.veil-link` 便携定位文件模型。
//!
//! 链接只保存容器身份和创建时间；位置由已登记工作区中的实际容器决定。
//! 它可被复制到其他位置，并可通过配置中的结构化正文重新生成；容器身份始终以
//! `veil_id` 为准，`veil_dir` 变化不会改变身份。

use crate::error::VeilError;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// `.veil-link` 文件扩展名。
pub const LINK_EXTENSION: &str = "veil-link";

/// 已解析容器与输出路径，调用方不能修改计划内容。
pub struct LinkPlan {
    resolved: crate::config::ResolvedVeil,
    output: std::path::PathBuf,
}
impl LinkPlan {
    pub fn recovered_link(&self) -> Option<&Path> {
        if self.resolved.recovered_link {
            self.resolved.link_path.as_deref()
        } else {
            None
        }
    }
}

fn check_link_plan(plan: &LinkPlan) -> Result<(), crate::workflow::WorkflowError> {
    use crate::workflow::WorkflowError;
    if plan.resolved.veil_id.is_empty() {
        return Err(WorkflowError::LinkMissingId);
    }
    if plan.output.extension().and_then(|ext| ext.to_str()) != Some(LINK_EXTENSION) {
        return Err(WorkflowError::LinkInvalidExtension(plan.output.clone()));
    }
    if plan.output.exists() {
        return Err(WorkflowError::LinkExists(plan.output.clone()));
    }
    Ok(())
}

pub(crate) fn prepare_link(
    target: &str,
    output: Option<&Path>,
) -> Result<LinkPlan, crate::workflow::WorkflowError> {
    let mut config = crate::config::AppConfig::load()?;
    let resolved = config.resolve_link_target(target)?;
    let output = output.map(Path::to_path_buf).unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(format!("{}.veil-link", resolved.veil_name))
    });
    let plan = LinkPlan { resolved, output };
    check_link_plan(&plan)?;
    Ok(plan)
}

pub(crate) fn commit_link(
    plan: LinkPlan,
) -> Result<std::path::PathBuf, crate::workflow::WorkflowError> {
    check_link_plan(&plan)?;
    create_registered_link(&plan.resolved, &plan.output)?;
    Ok(plan.output)
}

/// 为已解析的容器创建并登记链接。
pub(crate) fn create_registered_link(
    resolved: &crate::config::ResolvedVeil,
    output: &Path,
) -> Result<(), VeilError> {
    if output.exists() {
        return Err(VeilError::WorkError("链接已存在".into()));
    }
    let mut config = crate::config::AppConfig::load()?;
    let result = (|| {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Some(existing) = resolved.link_path.as_deref() {
            VeilLink::load(existing)?.save_new(output)?;
            config.cache_link(output)?;
        } else {
            config.register_link_at(
                &resolved.veil_id,
                &resolved.veil_name,
                &resolved.veil_dir,
                output,
            )?;
        }
        Ok(())
    })();
    result
}

/// JSONC 格式的便携定位文件，可复制到其他位置或由本机数据库中的正文缓存重新生成。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VeilLink {
    /// 链接文件格式版本。
    pub version: String,
    /// RFC 3339 格式的链接创建时间。
    pub created_at: String,
    /// 容器自身身份信息。
    pub veil: LinkVeil,
}

/// 容器自身身份信息，不包含工作区归属或路径。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkVeil {
    /// 不随路径或展示名称变化的容器稳定 ID。
    pub veil_id: String,
    /// 面向用户的容器名称。
    pub veil_name: String,

    /// RFC 3339 格式的容器原始创建时间，来自解锁后的容器数据库。
    pub created_at: String,
}

impl VeilLink {
    /// 使用容器自身信息创建链接。
    pub fn new(
        veil_id: impl Into<String>,
        veil_name: impl Into<String>,
        veil_created_at: impl Into<String>,
    ) -> Self {
        Self {
            version: default_version(),
            created_at: now(),
            veil: LinkVeil {
                veil_id: veil_id.into(),
                veil_name: veil_name.into(),
                created_at: veil_created_at.into(),
            },
        }
    }

    /// 从磁盘读取并解析 JSONC 格式的链接文件。
    ///
    /// # 错误
    /// 文件读取失败时返回 [`VeilError::ConfigError`]；JSONC 无效或字段不匹配时返回
    /// [`VeilError::InvalidFormat`]。
    pub fn load(path: &Path) -> Result<Self, VeilError> {
        // 先把 I/O 错误和格式错误分层，便于调用方决定是修复路径还是修复内容。
        let content = fs::read_to_string(path).map_err(|error| {
            VeilError::ConfigError(format!("读取链接文件失败 {}: {}", path.display(), error))
        })?;

        let link: Self = crate::jsonc::from_str(&content).map_err(|error| {
            VeilError::InvalidFormat(format!("链接文件格式错误 {}: {}", path.display(), error))
        })?;
        link.validate()?;
        Ok(link)
    }

    /// 将链接保存为 JSONC，更新时保留注释，缺少父目录时会自动创建。
    ///
    /// # 错误
    /// 父目录创建、序列化或文件写入失败时返回相应错误。
    pub fn save(&self, path: &Path) -> Result<(), VeilError> {
        self.validate()?;

        crate::jsonc::save(path, self)
    }

    pub fn save_new(&self, path: &Path) -> Result<(), VeilError> {
        use std::io::Write;
        self.validate()?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        crate::fsutil::create_dir_all_durable(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(
            &serde_json::to_vec_pretty(self)
                .map_err(|e| VeilError::SerializationError(e.to_string()))?,
        )?;
        temp.as_file().sync_all()?;
        temp.persist_noclobber(path)
            .map_err(|e| VeilError::Io(e.error))?;
        crate::fsutil::sync_directory(parent)?;
        Ok(())
    }

    /// 返回链接记录的 Veil 展示名称。
    pub fn veil_name(&self) -> String {
        self.veil.veil_name.clone()
    }

    /// 校验链接格式和容器身份。
    pub(crate) fn validate(&self) -> Result<(), VeilError> {
        crate::work::validate_veil_id(&self.veil.veil_id)?;
        if self.version != default_version() || self.created_at.trim().is_empty() {
            return Err(VeilError::InvalidFormat(
                "链接版本无效或缺少创建时间".to_string(),
            ));
        }
        if self.veil.veil_name.trim().is_empty() || self.veil.created_at.trim().is_empty() {
            return Err(VeilError::InvalidFormat(
                "链接缺少容器名称或创建时间".to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_contains_only_identity_and_preserves_comments() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("photos.veil-link");
        let mut link = VeilLink::new("veil-1234", "photos", "2020-01-02T03:04:05Z");
        link.save(&path).unwrap();
        let content = fs::read_to_string(&path).unwrap().replace(
            "\"veil_name\": \"photos\",",
            "/* 展示名称 */ \"veil_name\": \"photos\", // 名称说明",
        );
        fs::write(&path, content).unwrap();
        link.veil.veil_name = "new-photos".into();
        link.save(&path).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("/* 展示名称 */"));
        assert!(content.contains("// 名称说明"));
        let loaded = VeilLink::load(&path).unwrap();
        assert_eq!(loaded.veil.veil_name, "new-photos");
        assert_eq!(loaded.veil.created_at, "2020-01-02T03:04:05Z");
        let document = serde_json::to_value(loaded).unwrap();
        assert_eq!(document.as_object().unwrap().len(), 3);
        assert_eq!(document["veil"].as_object().unwrap().len(), 3);
    }

    #[test]
    fn invalid_identity_is_never_saved() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("invalid.veil-link");
        for id in ["", "../outside", "a/b", "a\\b", "."] {
            assert!(
                VeilLink::new(id, "photos", "2020-01-02T03:04:05Z")
                    .save(&path)
                    .is_err()
            );
            assert!(!path.exists());
        }
    }
}
