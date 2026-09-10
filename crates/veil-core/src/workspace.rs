//! 工作区管理模块
//!
//! 工作区是存储加密文件的物理目录，支持三种类型：
//! - 默认工作区：~/.veil/workspaces/default/
//! - 自定义工作区：用户指定的命名工作区
//! - 专属工作区：单个容器独占的工作区

use crate::error::VeilError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
