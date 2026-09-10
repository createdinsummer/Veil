use crate::i18n;
use anyhow::Result;
use colored::Colorize;
use veil_core::config::{GlobalConfig, HintsLevel};

/// 配置命令 - 设置提示级别
pub fn run(hints_level: Option<String>, _show: bool) -> Result<()> {
    if let Some(level) = hints_level {
        set_hints_level(&level)?;
    } else {
        // `--show` 是兼容旧写法的显式开关；无参数时也默认展示配置。
        show_config()?;
    }

    Ok(())
}

fn show_config() -> Result<()> {
    let config = GlobalConfig::load()?;
    let effective_level = crate::hints::current_level();

    println!("\n{}", i18n::t("config.title").cyan().bold());
    println!();

    let level = match effective_level {
        HintsLevel::Full => i18n::t("config.hints_full"),
        HintsLevel::Brief => i18n::t("config.hints_brief"),
        HintsLevel::Off => i18n::t("config.hints_off"),
    };
    println!(
        "  {}",
        i18n::t1("config.hints_level", "level", level).bright_white()
    );

    if std::env::var("VEIL_HINTS")
        .ok()
        .and_then(|value| HintsLevel::parse(&value))
        .is_some()
    {
        println!(
            "  {}",
            i18n::t1("config.env_override", "level", effective_level.as_str()).yellow()
        );
    }

    if let Some(ref default_ws) = config.workspace.default {
        println!(
            "  {}",
            i18n::t1(
                "config.default_workspace",
                "path",
                &default_ws.path.display().to_string()
            )
            .bright_black()
        );
    }

    println!(
        "  {}",
        i18n::t1(
            "config.container_count",
            "count",
            &config.containers.len().to_string()
        )
    );
    println!();

    Ok(())
}

fn set_hints_level(level: &str) -> Result<()> {
    let valid_level = HintsLevel::parse(level)
        .ok_or_else(|| anyhow::anyhow!("{}", i18n::t("config.invalid_level")))?;

    crate::hints::set_hint_level(valid_level.as_str())?;

    let description = match valid_level {
        HintsLevel::Full => i18n::t("config.description_full"),
        HintsLevel::Brief => i18n::t("config.description_brief"),
        HintsLevel::Off => i18n::t("config.description_off"),
    };

    println!(
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

    if let Ok(value) = std::env::var("VEIL_HINTS") {
        if let Some(override_level) = HintsLevel::parse(&value) {
            println!(
                "{}",
                i18n::t1("config.env_override", "level", override_level.as_str()).yellow()
            );
        }
    }

    Ok(())
}
