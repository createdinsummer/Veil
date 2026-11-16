//! CLI Work 参数的空间目录适配视图；稳定空间身份保存在应用数据库。
//!
//! 所属模块：服务层工作区配置，兼容命令行的目录参数，并为容器目录计算提供统一路径规则。
//! 核心数据：WorkType 描述默认、命名或专属工作区；WorkConfig 保存路径、类型、说明和创建时间。
//! 核心函数：resolve_root 将配置中的相对路径解析到配置目录；validate_veil_id 确保稳定 ID 可用作单个目录名；veil_name_with_suffix 生成安全的链接文件名片段。
//! 依赖能力：Serde 配置序列化、文件路径分段校验和 UTC 时间戳。整体目标是让工作区位置可移植，并阻止容器 ID 被解释成路径穿越。

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
    /// 工作区目录；可以是绝对路径，也可以相对于应用配置目录。
    pub path: PathBuf,
    /// 工作区的组织策略，决定是否允许多个容器共存。
    pub work_type: WorkType,
    /// 用户填写的可选说明，只用于展示和辨认工作区。
    pub description: Option<String>,
    /// 创建时记录的 RFC 3339 UTC 时间。
    pub created_at: String,
}

impl WorkConfig {
    /// 功能：为可存多个容器的命名工作区建立经过路径语法检查的配置对象。
    /// 入参：name: String 为展示名，当前不额外校验；path: PathBuf 为绝对路径或相对应用配置目录的路径；description: `Option<String>` 为可选说明。
    /// 返回值：Ok(WorkConfig)，类型为 Custom(name)，保存路径、说明和当前 UTC 创建时间。
    /// 异常：空路径或不支持的相对根/前缀形式返回 WorkError。
    /// 注意事项：不创建目录、不登记 app.db，也不验证空间重名、权限或是否已存在。
    pub fn custom_work(
        name: String,
        path: PathBuf,
        description: Option<String>,
    ) -> Result<Self, VeilError> {
        Self::from_path(path, WorkType::Custom(name), description)
    }

    /// 功能：为专属工作区建立路径配置，表达后续只能容纳一个容器的业务意图。
    /// 入参：path: PathBuf，为绝对位置或相对配置目录的位置。
    /// 返回值：Ok(WorkConfig)，work_type 为 Dedicated，description 为 None，创建时间取当前 UTC。
    /// 异常：不合法路径语法返回 WorkError。
    /// 注意事项：这里只组装对象，不检查当前容器数或空目录，不把专属约束写入数据库。
    pub fn dedicated_work(path: PathBuf) -> Result<Self, VeilError> {
        Self::from_path(path, WorkType::Dedicated, None)
    }

    /// 功能：统一保存工作区位置与组织策略，并在交给后续解析前拒绝不支持的路径形式。
    /// 入参：path: PathBuf 为工作区位置；work_type: WorkType 为默认/命名/专属组织策略；description: `Option<String>` 为展示说明。
    /// 返回值：Ok(WorkConfig)，所有输入保存并附当前 UTC RFC 3339 创建时间。
    /// 异常：validate_path 失败返回 WorkError。
    /// 注意事项：只验证路径语法，不核验名称和文件系统；失败前构造的临时对象不会产生目录或登记副作用。
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

    /// 功能：确认工作区配置能按“绝对路径或配置目录相对路径”的规则解释。
    /// 入参：self: &WorkConfig，读取 path 字段。
    /// 返回值：路径非空，且为绝对路径或不含根/驱动器前缀的相对形式时 Ok(())。
    /// 异常：空路径或带不支持根/前缀的相对路径返回 WorkError。
    /// 注意事项：相对路径中的 .. 没有在此禁止；不读取磁盘，不保证目标属于配置目录内部或一定可访问。
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

    /// 功能：将工作区配置解释为后续定位容器的目录路径。
    /// 入参：self: &WorkConfig 提供 path；config_dir: &Path 必须为绝对配置目录，即使 self.path 已为绝对也要求此条件。
    /// 返回值：Ok(PathBuf)，相对路径拼到 config_dir，绝对 self.path 则按 Path::join 语义直接作为结果。
    /// 异常：工作区路径语法或配置基准不是绝对路径时返回 WorkError。
    /// 注意事项：不创建目录、不读当前目录、不解析符号链接或清理 ..，结果只是规则拼接的绝对路径文本。
    pub fn resolve_root(&self, config_dir: &Path) -> Result<PathBuf, VeilError> {
        self.validate_path()?;
        if !config_dir.is_absolute() {
            return Err(VeilError::WorkError("配置目录必须是绝对路径".to_string()));
        }
        Ok(config_dir.join(&self.path))
    }
}

/// 功能：限定容器身份可被用作一个目录名，避免把身份参数解释成多段路径。
/// 入参：veil_id: &str，为非空单个普通路径段，不得含 / 或反斜杠。
/// 返回值：按当前平台 Path 分段规则仅含一个 Normal 分段且 trim 后非空时 Ok(())。
/// 异常：空白、绝对/相对特殊段、多段或含分隔符时返回 InvalidFormat。
/// 注意事项：不要求 veil-前缀或随机数格式，不校验所有平台的保留设备名等创建规则，也不为目录创建预留位置。
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

/// 功能：将容器展示名清理成适合默认文件名主体的文本片段。
/// 入参：veil_name: &str，可含用户输入的任意名称。
/// 返回值：String，先取前 80 个字符，把控制字符和常见路径保留符替换为 -，再去掉首尾连字符、点和 ASCII 空格；空结果为 veil。
/// 异常：只处理字符串，不返回错误。
/// 注意事项：不是所有操作系统文件名规则的完整校验，也不保证唯一性；长度按字符而不是 UTF-8 字节计算。
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

/// 功能：把清理后的名称与调用方时间后缀组合，帮助区分展示名相同的对象。
/// 入参：veil_name: &str 为原名称；creation_time: &str 为原样拼接的后缀文本，通常由调用方生成。
/// 返回值：String，格式为“安全名称-creation_time”。
/// 异常：不解析时间、不返回错误。
/// 注意事项：只清理名称主体，creation_time 没有在此清理或验证；完整文件名和最终路径的安全性、长度与冲突仍由调用方负责。
pub fn veil_name_with_suffix(veil_name: &str, creation_time: &str) -> String {
    format!("{}-{}", safe_veil_name(veil_name), creation_time)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// 验证工作目录和链接路径按配置文件所在目录解析，而不依赖启动时当前目录。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn paths_are_relative_to_config_directory() {
        let dir = tempfile::TempDir::new().unwrap();
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
