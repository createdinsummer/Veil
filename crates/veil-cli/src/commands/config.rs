//! `veil config` 子命令：查看或更新用户偏好。

use crate::error::Result;
use crate::i18n;
use colored::Colorize;
use veil_core::config::{GlobalConfig, HintsLevel};

/// 根据参数设置提示级别，或展示当前有效配置。
///
/// `_show` 仅用于保留旧命令语法；只要未提供 `hints_level` 就会展示配置。
///
/// # 错误
/// 配置加载、级别解析或保存失败时返回错误。
pub fn run(hints_level: Option<String>, _show: bool) -> Result<()> {
    if let Some(level) = hints_level {
        // 有级别参数时执行写入；否则只读展示，保持命令语义简单。
        set_hints_level(&level)?;
    } else {
        show_config()?;
    }

    Ok(())
}

/// 展示提示级别、默认工作区以及容器、卷、链接数量。
///
/// # 错误
/// 全局配置加载失败时返回错误。
fn show_config() -> Result<()> {
    // effective_level 同时考虑配置文件与 VEIL_HINTS，展示用户真正会看到的级别。
    let config = GlobalConfig::load()?;
    let effective_level = crate::hints::current_level();

    crate::outln!("\n{}", i18n::t("config.title").cyan().bold());
    crate::outln!();

    if let Ok(path) = GlobalConfig::config_path() {
        crate::outln!(
            "  {}",
            i18n::t1("config.path", "path", &path.display().to_string()).bright_black()
        );
    }

    let level = match effective_level {
        HintsLevel::Full => i18n::t("config.hints_full"),
        HintsLevel::Brief => i18n::t("config.hints_brief"),
        HintsLevel::Off => i18n::t("config.hints_off"),
    };
    crate::outln!(
        "  {}",
        i18n::t1("config.hints_level", "level", level).bright_white()
    );

    // 环境变量存在且合法时额外提示，避免用户误以为配置文件未生效。
    if std::env::var("VEIL_HINTS")
        .ok()
        .and_then(|value| HintsLevel::parse(&value))
        .is_some()
    {
        crate::outln!(
            "  {}",
            i18n::t1("config.env_override", "level", effective_level.as_str()).yellow()
        );
    }

    if let Some(ref default_ws) = config.workspace.default {
        crate::outln!(
            "  {}",
            i18n::t1(
                "config.default_workspace",
                "path",
                &default_ws.path.display().to_string()
            )
            .bright_black()
        );
    }

    crate::outln!(
        "  {}",
        i18n::t1(
            "config.custom_workspace_count",
            "count",
            &config.workspace.custom.len().to_string()
        )
    );
    let mut custom_workspaces: Vec<_> = config.workspace.custom.iter().collect();
    custom_workspaces.sort_by(|left, right| left.0.cmp(right.0));
    for (name, workspace) in custom_workspaces {
        crate::outln!(
            "{}",
            i18n::t2(
                "config.custom_workspace",
                "name",
                name,
                "path",
                &workspace.path.display().to_string()
            )
            .bright_black()
        );
    }

    crate::outln!(
        "  {}",
        i18n::t1(
            "config.container_count",
            "count",
            &config.containers.len().to_string()
        )
    );
    crate::outln!(
        "  {}",
        i18n::t1(
            "config.volume_count",
            "count",
            &config.volumes.len().to_string()
        )
    );
    crate::outln!(
        "  {}",
        i18n::t1(
            "config.link_count",
            "count",
            &config.links.len().to_string()
        )
    );
    crate::outln!();

    Ok(())
}

/// 校验并持久化提示级别，同时报告环境变量是否仍会覆盖该值。
///
/// # 错误
/// 级别无效、配置加载或保存失败时返回错误。
fn set_hints_level(level: &str) -> Result<()> {
    // 先验证并规范化输入，再写入全局配置。
    let valid_level =
        HintsLevel::parse(level).ok_or_else(|| crate::cli_error!(ConfigInvalidLevel))?;

    crate::hints::set_hint_level(valid_level.as_str())?;

    let description = match valid_level {
        HintsLevel::Full => i18n::t("config.description_full"),
        HintsLevel::Brief => i18n::t("config.description_brief"),
        HintsLevel::Off => i18n::t("config.description_off"),
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
        && let Some(override_level) = HintsLevel::parse(&value)
    {
        crate::outln!(
            "{}",
            i18n::t1("config.env_override", "level", override_level.as_str()).yellow()
        );
    }

    Ok(())
}
