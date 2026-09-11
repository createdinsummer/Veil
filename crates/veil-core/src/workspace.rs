//! 工作区管理模块
//!
//! 工作区是存储加密文件的物理目录，支持三种类型：
//! - 默认工作区：~/.veil/workspaces/default/
//! - 自定义工作区：用户指定的命名工作区
//! - 专属工作区：单个容器独占的工作区

use crate::error::VeilError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 工作区类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkspaceType {
    /// 默认工作区
    Default,
    /// 自定义工作区（命名）
    Custom(String),
    /// 专属工作区（某个容器独占）
    Dedicated,
}

/// 工作区配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    /// 工作区路径
    pub path: PathBuf,
    /// 工作区类型
    pub workspace_type: WorkspaceType,
    /// 描述信息
    pub description: Option<String>,
    /// 创建时间
    pub created_at: String,
}

impl WorkspaceConfig {
    /// 创建默认工作区配置
    pub fn default_workspace() -> Result<Self, VeilError> {
        let path = Self::default_workspace_path()?;
        Ok(Self {
            path,
            workspace_type: WorkspaceType::Default,
            description: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        })
    }

    /// 创建自定义工作区配置
    pub fn custom_workspace(name: String, path: PathBuf, description: Option<String>) -> Self {
        Self {
            path,
            workspace_type: WorkspaceType::Custom(name),
            description,
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// 创建专属工作区配置
    pub fn dedicated_workspace(path: PathBuf) -> Self {
        Self {
            path,
            workspace_type: WorkspaceType::Dedicated,
            description: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// 获取默认工作区路径
    pub fn default_workspace_path() -> Result<PathBuf, VeilError> {
        let home = dirs::home_dir()
            .ok_or_else(|| VeilError::WorkspaceError("无法获取用户主目录".to_string()))?;
        Ok(home.join(".veil/workspaces/default"))
    }
}

fn safe_container_name(container_name: &str) -> String {
    let mut safe_name: String = container_name
        .chars()
        .take(80)
        .map(|character| {
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
    safe_name = safe_name.trim_matches(['-', '.', ' ']).to_string();
    if safe_name.is_empty() {
        safe_name = "container".to_string();
    }

    safe_name
}

/// 为重复容器生成带创建时间的名称段。
pub fn container_name_with_suffix(container_name: &str, creation_time: &str) -> String {
    format!("{}-{}", safe_container_name(container_name), creation_time)
}

/// 在工作区中分配容器目录，目录名始终以容器 ID 为基础。
pub fn allocate_container_directory(
    workspace_root: &Path,
    veil_id: &str,
    duplicate_suffix: &str,
) -> PathBuf {
    let base_path = workspace_root.join(veil_id);
    if !base_path.exists() {
        return base_path;
    }

    let duplicate_path = workspace_root.join(format!("{}-{}", veil_id, duplicate_suffix));
    if !duplicate_path.exists() {
        return duplicate_path;
    }

    for index in 2u32.. {
        let candidate = workspace_root.join(format!("{}-{}-{}", veil_id, duplicate_suffix, index));
        if !candidate.exists() {
            return candidate;
        }
    }

    unreachable!("容器目录序号不可能耗尽")
}
