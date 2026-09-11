//! 工作区管理模块
//!
//! 工作区是容器加密文件和 `.veil-meta` 的物理目录，支持默认、自定义和专属三种
//! 组织方式。本模块只负责路径模型和目录名分配；容器身份仍由稳定的 `veil_id`
//! 决定，展示名称或磁盘路径变化不会改变容器身份。

use crate::error::VeilError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 工作区的组织类型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkspaceType {
    /// 所有未显式选择工作区的容器共享的默认目录。
    Default,
    /// 以配置中的名称注册、可被多个容器共享的自定义工作区。
    Custom(String),
    /// 目录本身即容器根、由单个容器独占的工作区。
    Dedicated,
}

/// 工作区路径及其注册元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    /// 工作区根目录。
    pub path: PathBuf,
    /// 工作区组织类型。
    pub workspace_type: WorkspaceType,
    /// 可选的用户说明。
    pub description: Option<String>,
    /// RFC 3339 格式的创建时间。
    pub created_at: String,
}

impl WorkspaceConfig {
    /// 创建指向用户默认工作区路径的配置。
    ///
    /// # 错误
    /// 无法确定用户主目录时返回 [`VeilError::WorkspaceError`]。
    pub fn default_workspace() -> Result<Self, VeilError> {
        // 默认工作区路径由用户主目录推导，配置对象只保存最终路径。
        let path = Self::default_workspace_path()?;
        Ok(Self {
            path,
            workspace_type: WorkspaceType::Default,
            description: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        })
    }

    /// 创建命名自定义工作区配置。
    ///
    /// `name` 是配置查找键，`path` 是工作区实际根目录。
    pub fn custom_workspace(name: String, path: PathBuf, description: Option<String>) -> Self {
        // name 是配置查找键，path 才是实际目录。
        Self {
            path,
            workspace_type: WorkspaceType::Custom(name),
            description,
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// 创建由单个容器独占的工作区配置。
    pub fn dedicated_workspace(path: PathBuf) -> Self {
        // 专属工作区没有容器子目录，path 本身即容器根。
        Self {
            path,
            workspace_type: WorkspaceType::Dedicated,
            description: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// 返回默认工作区路径 `~/.veil/workspaces/default`。
    ///
    /// # 错误
    /// 无法确定用户主目录时返回 [`VeilError::WorkspaceError`]。
    pub fn default_workspace_path() -> Result<PathBuf, VeilError> {
        let home = dirs::home_dir()
            .ok_or_else(|| VeilError::WorkspaceError("无法获取用户主目录".to_string()))?;
        Ok(home.join(".veil/workspaces/default"))
    }
}

/// 将容器名称转换为可安全用于文件名和展示后缀的形式。
///
/// 名称最多保留 80 个字符，控制字符和常见路径保留字符替换为 `-`，并移除首尾的
/// 空白、点或连字符；清理后为空时回退为 `container`。
fn safe_container_name(container_name: &str) -> String {
    // 先限制长度并逐字符替换文件系统不适合保留的符号。
    let mut safe_name: String = container_name
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
        safe_name = "container".to_string();
    }

    safe_name
}

/// 为同名容器生成包含清理后名称和创建时间的名称段。
pub fn container_name_with_suffix(container_name: &str, creation_time: &str) -> String {
    format!("{}-{}", safe_container_name(container_name), creation_time)
}

/// 在指定工作区根目录下分配唯一的容器目录。
///
/// 目录优先使用 `veil_id`；若已存在，则依次追加 `duplicate_suffix` 和递增序号。
/// 调用方仍需在创建前负责并发安全，函数只根据当前文件系统状态选择名称。
pub fn allocate_container_directory(
    workspace_root: &Path,
    veil_id: &str,
    duplicate_suffix: &str,
) -> PathBuf {
    // 首选稳定 ID 本身，正常情况下完全不需要追加后缀。
    let base_path = workspace_root.join(veil_id);
    if !base_path.exists() {
        return base_path;
    }

    // 首选带创建时间后缀的目录，再通过递增序号处理极端重复。
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
