//! `veil config` 子命令：查看或更新用户设置。
//! 所属模块：CLI命令适配。核心run/set_hint_level/show_config协调偏好保存与摘要；依赖服务配置、HintLevel和本地化，目标是解释当前生效设置。
//!
//! 无级别参数时展示本机数据库位置、空间（工作区）、登记数量与生效提示级别；
//! `--hints full|brief|off` 持久化类型化用户偏好，`VEIL_HINTS` 只覆盖当前进程。

use crate::error::Result;
use crate::i18n;
use colored::Colorize;
use veil_service::config::HintLevel;

use veil_service::{ServiceRequest, ServiceResponse};

/// 根据是否给出提示级别决定保存设置或显示配置摘要。
/// 入参：hint_level（`Option<String>`）为full/brief/off；_show（bool）为旧接口保留值，当前未读取。
/// 返回值：成功保存或显示后Ok(())。
/// 异常：级别/配置/服务/保存错误返回CommandError。
/// 注意事项：None时展示也可能因配置加载初始化应用库/默认空间，不能承诺绝对只读；VEIL_HINTS不因持久设置而被移除。
pub fn run(hint_level: Option<String>, _show: bool) -> Result<()> {
    if let Some(level) = hint_level {
        // 有级别参数时保存用户偏好；展示分支也可能因加载配置而初始化本机数据。
        set_hint_level(&level)?;
    } else {
        show_config()?;
    }

    Ok(())
}

/// 展示本机配置位置、有效提示级别、空间和登记计数。
/// 入参：无，读取全局配置与当前环境。
/// 返回值：摘要显示后Ok(())。
/// 异常：读取配置摘要失败或非预期响应返回CommandError；单独ConfigPath获取失败静默跳过。
/// 注意事项：“自定义”仅指ID不等默认ID的空间；容器/链接计数来自本机缓存而非扫描所有卷。多次服务请求非同一事务快照。
fn show_config() -> Result<()> {
    // effective_level 同时考虑配置文件与 VEIL_HINTS，展示用户真正会看到的级别。
    let config = match super::request(ServiceRequest::ReadConfigSummary)? {
        ServiceResponse::ConfigSummary(summary) => summary,
        // 响应类型与请求不匹配属于服务协议错误。
        _ => return Err(veil_core::error::VeilError::WorkError("服务响应类型不符".into()).into()),
    };
    let effective_level = crate::hints::current_level();

    crate::outln!("\n{}", i18n::t("config.title").cyan().bold());
    crate::outln!();

    // 本机配置文件（app.db）路径；展示失败不致命，静默跳过。
    if let Ok(ServiceResponse::ConfigPath(path)) = super::request(ServiceRequest::ConfigPath) {
        crate::outln!(
            "  {}",
            i18n::t1("config.path", "path", &path.display().to_string()).bright_black()
        );
    }

    // 把生效级别映射为对应文案。
    let level = match effective_level {
        HintLevel::Full => i18n::t("config.hints_full"),
        HintLevel::Brief => i18n::t("config.hints_brief"),
        HintLevel::Off => i18n::t("config.hints_off"),
    };
    crate::outln!(
        "  {}",
        i18n::t1("config.hint_level", "level", level).bright_white()
    );

    // 环境变量存在且合法时额外提示，避免用户误以为配置文件未生效。
    if std::env::var("VEIL_HINTS")
        .ok()
        .and_then(|value| HintLevel::parse(&value))
        .is_some()
    {
        crate::outln!(
            "  {}",
            i18n::t1("config.env_override", "level", effective_level.as_str()).yellow()
        );
    }

    // 默认空间：按系统设置里的 default_space_id 在空间列表中匹配。
    if let Some(space) = config
        .spaces
        .iter()
        .find(|s| s.id == config.default_space_id)
    {
        crate::outln!("  {}", i18n::t1("config.default_work", "path", &space.path));
    }
    // 自定义空间 = 除默认 ID 之外的其余已登记空间。
    let custom = config
        .spaces
        .iter()
        .filter(|s| s.id != config.default_space_id)
        .collect::<Vec<_>>();
    crate::outln!(
        "  {}",
        i18n::t1(
            "config.custom_work_count",
            "count",
            &custom.len().to_string()
        )
    );
    // 逐个列出自定义空间的名称与路径。
    for space in custom {
        crate::outln!(
            "{}",
            i18n::t2(
                "config.custom_work",
                "name",
                &space.name,
                "path",
                &space.path
            )
        );
    }
    // 本机已登记容器数量与链接数量。
    crate::outln!(
        "  {}",
        i18n::t1(
            "config.veil_count",
            "count",
            &config.container_count.to_string()
        )
    );
    crate::outln!(
        "  {}",
        i18n::t1("config.link_count", "count", &config.link_count.to_string())
    );
    crate::outln!();

    Ok(())
}

/// 持久化提示偏好，并提示合法环境覆盖仍然有效。
/// 入参：level（&str）为服务支持的full/brief/off文本。
/// 返回值：保存并显示说明后Ok(())。
/// 异常：解析、服务或应用库写入失败返回CommandError。
/// 注意事项：VEIL_HINTS存在且合法时仍优先；本方法不修改环境，只展示覆盖提示。
fn set_hint_level(level: &str) -> Result<()> {
    // 先验证并规范化输入，再写入全局配置。
    let valid_level = crate::hints::set_hint_level(level)?;

    // 每个级别配一句说明，帮助用户理解三种档位的差异。
    let description = match valid_level {
        HintLevel::Full => i18n::t("config.description_full"),
        HintLevel::Brief => i18n::t("config.description_brief"),
        HintLevel::Off => i18n::t("config.description_off"),
    };

    crate::outln!(
        "{}",
        i18n::t2(
            "config.set_success",
            "level",
            valid_level.as_str(),
            "description",
            description,
        )
        .green()
    );

    // 写配置成功后仍提示当前进程是否被环境变量覆盖。
    if let Ok(value) = std::env::var("VEIL_HINTS")
        && let Some(override_level) = HintLevel::parse(&value)
    {
        crate::outln!(
            "{}",
            i18n::t1("config.env_override", "level", override_level.as_str()).yellow()
        );
    }

    Ok(())
}
