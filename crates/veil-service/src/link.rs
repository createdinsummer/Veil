//! `.veil-link` 便携定位文件模型。
//!
//! 链接只保存容器身份和创建时间；位置由已登记工作区中的实际容器决定。
//! 它可被复制到其他位置，并可通过配置中的结构化正文重新生成；容器身份始终以
//! `veil_id` 为准，`veil_dir` 变化不会改变身份。
//!
//! 所属业务模块：veil-service 的链接（link）模块。链接是容器在文件系统上的「便携
//! 入口」，正文只包含容器 ID、展示名称、容器创建时间与链接创建时间，不包含任何
//! 文件内容或密码；真正打开容器时还要按 veil_id 在本机登记中定位容器目录。
//!
//! 文件内核心清单：
//! - `LinkPlan`：已解析容器与输出路径的组合，供两阶段「准备 → 提交」流程使用；
//! - `VeilLink` / `LinkVeil`：链接文件的 JSONC 序列化模型（版本 + 创建时间 + 容器身份）；
//! - `prepare_link` / `commit_link`：链接创建的两阶段入口（准备计划、校验并落盘）；
//! - `create_registered_link`：为已解析容器创建链接文件并把结构化正文缓存进本机数据库。
//!
//! 依赖能力：`AppConfig`（本机登记/链接缓存）、`jsonc`（保留注释的 JSONC 读写）、
//! `fsutil`（持久化目录与文件同步）。
//!
//! 整体业务目标：把「容器入口」做成可复制、可恢复的小文件——本机缓存足够时即使
//! 链接文件丢失也能凭缓存重新生成；新建入口拒绝覆盖，更新入口允许替换，缓存与文件仍需上层协调失败处理。

use crate::error::VeilError;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// `.veil-link` 文件扩展名。
pub const LINK_EXTENSION: &str = "veil-link";

/// 已解析容器与输出路径，调用方不能修改计划内容。
///
/// 业务定位：链接创建的「准备阶段」产物。resolved 是解析出的容器身份与目录
/// （含该容器是否已有可复用的链接文件），output 是即将写入的链接文件位置；
/// 准备与提交两个阶段之间用户可能修改选择，但计划内容由服务层持有、外部只读。
pub struct LinkPlan {
    /// 解析出的容器：ID、展示名、实际目录，以及已有链接路径/是否需要恢复的标志。
    resolved: crate::config::ResolvedVeil,
    /// 链接文件将要写入的完整路径（.veil-link 后缀）。
    output: std::path::PathBuf,
}
impl LinkPlan {
    /// 功能：为用户提示计划解析过程中是否走过链接缓存恢复路径。
    /// 入参：self: &LinkPlan，为已经解析好容器和输出位置的计划。
    /// 返回值：resolved.recovered_link 为 true 时借用其 link_path，否则 None；标志为 true 但没有路径也返回 None。
    /// 异常：不读文件或数据库，不返回错误。
    /// 注意事项：只是计划中保存的历史信息，不重新检查链接是否存在，也不能证明恢复文件一定由本次调用创建。
    pub fn recovered_link(&self) -> Option<&Path> {
        if self.resolved.recovered_link {
            self.resolved.link_path.as_deref()
        } else {
            None
        }
    }
}

/// 功能：在准备和提交阶段拒绝缺少身份、扩展名不合规或会覆盖已有位置的链接计划。
/// 入参：plan: &LinkPlan，包含已解析容器快照及新输出路径。
/// 返回值：容器 ID 非空、输出最后扩展名精确为 veil-link 且 exists 为 false 时 Ok(())。
/// 异常：分别返回 LinkMissingId、LinkInvalidExtension 或 LinkExists。
/// 注意事项：不验证目标容器仍可用、不读取密码或最新名称；存在性检查不是原子占位，最终 save_new 仍须禁止覆盖。
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

/// 功能：将用户选择的容器和可选输出位置转成供后续提交使用的链接计划。
/// 入参：target: &str 为 AppConfig 支持的 ID、名称、目录或链接选择器；output: `Option<&Path>` 为新链接位置。
/// 返回值：Ok(LinkPlan)，含解析结果和通过基本校验的输出；缺省输出取当前目录下“容器展示名.veil-link”。
/// 异常：本机配置/容器解析失败经 WorkflowError 转换；ID、扩展名或已有目标检查失败返回对应链接错误。
/// 注意事项：解析可能恢复旧链接和刷新缓存，准备并非绝对无副作用；不能取得当前目录时退回相对的 .。
/// 默认文件名直接用解析出的展示名拼接，本函数不调用 safe_veil_name 清理；计划也不冻结容器和输出的后续变化。
pub(crate) fn prepare_link(
    target: &str,
    output: Option<&Path>,
) -> Result<LinkPlan, crate::workflow::WorkflowError> {
    // 加载本机配置，把选择器解析成容器（可能顺带恢复链接缓存）。
    let mut config = crate::config::AppConfig::load()?;
    let resolved = config.resolve_link_target(target)?;
    // 未指定输出时，链接落在当前目录并以容器展示名命名。
    let output = output.map(Path::to_path_buf).unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(format!("{}.veil-link", resolved.veil_name))
    });
    let plan = LinkPlan { resolved, output };
    check_link_plan(&plan)?;
    Ok(plan)
}

/// 功能：提交已准备的计划，创建链接入口并保存本机正文缓存。
/// 入参：plan: LinkPlan，按值消费先前准备的容器与输出位置。
/// 返回值：Ok(PathBuf) 为最终输出路径。
/// 异常：计划重新校验或 create_registered_link 的创建/缓存失败返回 WorkflowError。
/// 注意事项：重新校验只检查非空身份、扩展名和当前存在性，不重新解析容器的最新位置或名称；跨文件和数据库写入不保证整体回滚。
pub(crate) fn commit_link(
    plan: LinkPlan,
) -> Result<std::path::PathBuf, crate::workflow::WorkflowError> {
    check_link_plan(&plan)?;
    create_registered_link(&plan.resolved, &plan.output)?;
    Ok(plan.output)
}

/// 功能：为已经解析的容器复制已有链接或生成新链接，并将成功读取的正文写入本机缓存。
/// 入参：resolved: &ResolvedVeil 为调用方持有的容器信息；output: &Path 为尚不存在的新入口路径。
/// 返回值：文件创建和缓存流程均成功时 Ok(())。
/// 异常：目标已存在返回 WorkError；父目录、旧链接读取、新文件发布或缓存失败传播 VeilError。
/// 注意事项：此入口不校验输出扩展名；复用分支读取旧链接正文后直接复制，不重新对照 resolved.veil_id。
/// 复用分支缓存失败不会在这里删除新文件；新建分支的 register_link_at 也仅尽力补偿，发布后同步失败同样可能已有文件。
pub(crate) fn create_registered_link(
    resolved: &crate::config::ResolvedVeil,
    output: &Path,
) -> Result<(), VeilError> {
    // 覆盖已有链接文件会破坏其他入口，直接拒绝。
    if output.exists() {
        return Err(VeilError::WorkError("链接已存在".into()));
    }
    let mut config = crate::config::AppConfig::load()?;
    let result = (|| {
        // 确保输出目录存在（链接可能放在尚未创建的目录里）。
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        // 容器已有链接文件：复制其内容（保持身份一致）到新位置并缓存；
        // 没有时按本机登记信息新建链接。
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
///
/// 业务定位：链接文件的完整序列化模型。version 标识格式版本，created_at 记录链接
/// 创建时间，veil 承载容器自身身份。链接不保存文件内容、密码或容器目录位置——
/// 打开容器时仍需通过本机登记按 veil_id 定位实际目录。
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
///
/// 只有三个字段：稳定 ID、展示名称、容器原始创建时间。veil_id 是跨目录移动、
/// 改名都不变的身份锚点；名称和时间只用于展示与识别，不参与定位。
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
    /// 功能：用已知容器身份组装一份链接正文，供后续验证并保存。
    /// 入参：veil_id/veil_name/veil_created_at: impl `Into<String>`，分别为稳定 ID、展示名及容器原始创建时间文本。
    /// 返回值：VeilLink，version 为 1.0，链接自身创建时间取当前 UTC，容器字段原样保存。
    /// 异常：只构造对象，不返回 Result，也不立即验证输入。
    /// 注意事项：不查询容器数据库，不写文件；创建时间文本在后续 validate 只检查非空，不校验完整 RFC 3339 语法。
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

    /// 功能：从磁盘链接读取容器选择所需的身份与展示信息，并进行当前格式校验。
    /// 入参：path: &Path 为链接文件路径，不在这里强制要求 veil-link 扩展名。
    /// 返回值：Ok(VeilLink)，正文通过 JSONC 反序列化及 validate 的身份/版本/非空字段检查。
    /// 异常：文件读取含非 UTF-8 失败包装为 ConfigError；JSONC 或字段校验失败为 InvalidFormat。
    /// 注意事项：错误说明可能带路径，应由统一日志入口分类脱敏；读取成功不证明容器已登记、存在或密码正确。
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

    /// 功能：保存或更新一份链接正文，在合法旧 JSONC 上尽量保留仍存在字段旁的注释。
    /// 入参：self: &VeilLink 为要持久化的完整模型；path: &Path 为目标文件，允许替换已有链接。
    /// 返回值：validate 与 jsonc::save 成功时 Ok(())。
    /// 异常：身份/版本/非空字段非法返回 InvalidFormat；旧文本、序列化或文件写入失败传播 VeilError。
    /// 注意事项：与 save_new 不同，此方法允许更新现有目标；不登记 link_cache，文件与本机缓存同步由上层负责。
    /// 父目录创建和替换由通用原子写入处理，发布后的目录同步失败在该通用路径中仅告警。
    pub fn save(&self, path: &Path) -> Result<(), VeilError> {
        self.validate()?;

        crate::jsonc::save(path, self)
    }

    /// 功能：以禁止覆盖方式发布一份完整新链接，避免把已存在入口替换成其他容器。
    /// 入参：self: &VeilLink 为已准备正文；path: &Path 为尚不存在的最终输出路径。
    /// 返回值：模型校验、父目录准备、临时写入/文件同步、禁止覆盖发布及目录同步成功时 Ok(())。
    /// 异常：非法模型、序列化或 I/O 失败返回 VeilError，目标已存在时为 AlreadyExists 类 I/O 错误。
    /// 注意事项：目标扩展名由上层计划检查，本方法不检查；发布之后父目录同步失败也返回 Err，此时文件可能已经完整存在。
    /// 不自动写本机缓存，也不保证所有平台与硬件故障下的断电持久性。
    pub fn save_new(&self, path: &Path) -> Result<(), VeilError> {
        use std::io::Write;
        self.validate()?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        // 准备父目录并请求同步新建目录项，然后在同目录创建临时文件；持久性仍取决于平台与设备。
        crate::fsutil::create_dir_all_durable(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        // 新建正文使用带缩进的标准 JSON，属于 JSONC 子集；与更新保存不同，这里没有旧注释需要保留。
        temp.write_all(
            &serde_json::to_vec_pretty(self)
                .map_err(|e| VeilError::SerializationError(e.to_string()))?,
        )?;
        // 先落盘再改名：保证新链接要么完整存在、要么完全不存在，不会出现半截文件。
        temp.as_file().sync_all()?;
        temp.persist_noclobber(path)
            .map_err(|e| VeilError::Io(e.error))?;
        // 发布后请求同步父目录；失败会报错，但不能安全撤销已经创建的最终路径。
        crate::fsutil::sync_directory(parent)?;
        Ok(())
    }

    /// 功能：取出链接记录的容器展示名称，供界面或命令显示。
    /// 入参：self: &VeilLink。
    /// 返回值：String，为 veil.veil_name 的独立副本。
    /// 异常：只复制内存文本，不返回错误。
    /// 注意事项：是链接中保存的名称，可能落后于容器当前权威名称，不触发解锁或刷新。
    pub fn veil_name(&self) -> String {
        self.veil.veil_name.clone()
    }

    /// 功能：拒绝缺少必要身份和展示信息的链接，供读取与两种保存入口共同使用。
    /// 入参：self: &VeilLink，检查 veil_id、version、链接创建时间、名称及容器创建时间。
    /// 返回值：ID 能作为安全单目录段、version 精确为 1.0，其他必需文本 trim 后非空时 Ok(())。
    /// 异常：ID 规则、版本或必需字段不符合时返回 InvalidFormat。
    /// 注意事项：不验证时间戳语法、不要求 ID 具备固定 veil-随机格式，不核对实际容器或本机登记，也不校验文件位置。
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

/// 功能：为新链接记录生成当前创建时间文本。
/// 入参：无，读取 UTC 系统时钟。
/// 返回值：String，Chrono 输出的 RFC 3339 文本。
/// 异常：不返回业务错误。
/// 注意事项：是可调整系统时钟的观察值，不用于测量时长，也不是容器原始创建时间。
fn now() -> String {
    Utc::now().to_rfc3339()
}

/// 功能：集中提供当前链接正文格式标识，供构造与校验一致使用。
/// 入参：无。
/// 返回值：String，固定为 1.0。
/// 异常：不返回错误。
/// 注意事项：这是链接格式版本，不是应用包版本或 .veil 便携包的版本 2。
fn default_version() -> String {
    "1.0".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// 验证链接缓存只保存稳定容器身份信息，并在 JSONC 更新时保留用户注释。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
    /// 验证身份字段无效时不写入或覆盖已有链接文件。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
