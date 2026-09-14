//! 工作区管理模块
//!
//! Work 是组织 Veil 的逻辑工作区，`work_root` 是它对应的根目录。默认和自定义
//! Work 在 `work_root` 下为每个 Veil 分配独立 `veil_dir`；专属 Work 直接把
//! `work_root` 作为单个 Veil 的 `veil_dir`。Veil 身份始终由稳定的 `veil_id`
//! 决定，展示名称或目录位置变化不会改变身份。

use crate::error::VeilError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 工作区的组织类型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WorkType {
    /// 所有未显式选择 Work 的 Veil 共享的默认 Work。
    Default,
    /// 以配置中的名称注册、可被多个 Veil 共享的自定义 Work。
    Custom(String),
    /// `work_root` 本身即 `veil_dir`、由单个 Veil 独占的 Work。
    Dedicated,
}

/// Work 根目录及其注册元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkConfig {
    /// 工作区根目录 `work_root`。
    pub path: PathBuf,
    /// 工作区组织类型。
    pub work_type: WorkType,
    /// 可选的用户说明。
    pub description: Option<String>,
    /// RFC 3339 格式的创建时间。
    pub created_at: String,
}

impl WorkConfig {
    /// 创建指向默认 `work_root` 的 Work 配置。
    ///
    /// # 错误
    /// 无法确定用户主目录时返回 [`VeilError::WorkError`]。
    pub fn default_work() -> Result<Self, VeilError> {
        // 默认工作区路径由用户主目录推导，配置对象只保存最终路径。
        let path = Self::default_work_root()?;
        Ok(Self {
            path,
            work_type: WorkType::Default,
            description: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        })
    }

    /// 创建命名自定义工作区配置。
    ///
    /// `name` 是配置查找键，`path` 是实际 `work_root`。
    pub fn custom_work(name: String, path: PathBuf, description: Option<String>) -> Self {
        // name 是配置查找键，path 才是实际目录。
        Self {
            path,
            work_type: WorkType::Custom(name),
            description,
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// 创建由单个 Veil 独占的 Work 配置。
    pub fn dedicated_work(path: PathBuf) -> Self {
        // 专属 Work 没有额外的 Veil 子目录，path 本身即 veil_dir。
        Self {
            path,
            work_type: WorkType::Dedicated,
            description: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// 返回默认工作区根目录 `~/.veil/works/default`。
    ///
    /// # 错误
    /// 无法确定用户主目录时返回 [`VeilError::WorkError`]。
    pub fn default_work_root() -> Result<PathBuf, VeilError> {
        let home = dirs::home_dir()
            .ok_or_else(|| VeilError::WorkError("无法获取用户主目录".to_string()))?;
        Ok(home.join(".veil/works/default"))
    }
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

/// 在指定工作区根目录下分配唯一的 Veil 目录。
///
/// 目录优先使用 `veil_id`；若已存在，则依次追加 `duplicate_suffix` 和递增序号。
/// 调用方仍需在创建前负责并发安全，函数只根据当前文件系统状态选择名称。
pub fn allocate_veil_directory(work_root: &Path, veil_id: &str, duplicate_suffix: &str) -> PathBuf {
    // 首选稳定 ID 本身，正常情况下完全不需要追加后缀。
    let base_path = work_root.join(veil_id);
    if !base_path.exists() {
        return base_path;
    }

    // 首选带创建时间后缀的目录，再通过递增序号处理极端重复。
    let duplicate_path = work_root.join(format!("{}-{}", veil_id, duplicate_suffix));
    if !duplicate_path.exists() {
        return duplicate_path;
    }

    for index in 2u32.. {
        let candidate = work_root.join(format!("{}-{}-{}", veil_id, duplicate_suffix, index));
        if !candidate.exists() {
            return candidate;
        }
    }

    unreachable!("Veil 目录序号不可能耗尽")
}
