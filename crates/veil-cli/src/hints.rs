use crate::i18n;
use colored::Colorize;
use std::path::Path;
use veil_core::config::{GlobalConfig, HintsLevel};

#[derive(Debug, Clone, Copy)]
enum HintType {
    FirstInit,
    FirstPack,
    FirstUnpack,
    LinkRecovery,
    FileTypeAmbiguity,
    ExtensionHidden,
}

/// 当前生效的提示级别：`VEIL_HINTS` 环境变量优先，其次读取全局配置。
pub fn current_level() -> HintsLevel {
    if let Ok(value) = std::env::var("VEIL_HINTS") {
        if let Some(level) = HintsLevel::parse(&value) {
            return level;
        }
    }

    GlobalConfig::load()
        .map(|config| config.preferences.hints_level)
        .unwrap_or(HintsLevel::Full)
}

pub fn set_hint_level(level: &str) -> anyhow::Result<()> {
    let parsed = HintsLevel::parse(level)
        .ok_or_else(|| anyhow::anyhow!("{}", i18n::t("config.invalid_level")))?;
    let mut config = GlobalConfig::load()?;
    config.preferences.hints_level = parsed;
    config.save()?;
    Ok(())
}

fn allowed(hint_type: HintType, level: HintsLevel) -> bool {
    match level {
        HintsLevel::Off => false,
        HintsLevel::Brief => matches!(
            hint_type,
            HintType::FirstUnpack | HintType::LinkRecovery | HintType::FileTypeAmbiguity
        ),
        HintsLevel::Full => true,
    }
}

fn should_show_once(hint_type: HintType) -> bool {
    let mut config = match GlobalConfig::load() {
        Ok(config) => config,
        Err(_) => return allowed(hint_type, current_level()),
    };

    if !allowed(hint_type, current_level()) {
        return false;
    }

    let already_shown = match hint_type {
        HintType::FirstInit => config.system.init_hint_shown,
        HintType::FirstPack => config.system.pack_hint_shown,
        HintType::FirstUnpack => config.system.unpack_hint_shown,
        HintType::ExtensionHidden => config.system.extension_hint_shown,
        HintType::LinkRecovery | HintType::FileTypeAmbiguity => return true,
    };

    if already_shown {
        return false;
    }

    match hint_type {
        HintType::FirstInit => config.system.init_hint_shown = true,
        HintType::FirstPack => config.system.pack_hint_shown = true,
        HintType::FirstUnpack => config.system.unpack_hint_shown = true,
        HintType::ExtensionHidden => config.system.extension_hint_shown = true,
        HintType::LinkRecovery | HintType::FileTypeAmbiguity => {}
    }

    let _ = config.save();
    true
}

pub fn show_first_init_hint(container_name: &str, link_path: &Path) {
    if !should_show_once(HintType::FirstInit) {
        return;
    }

    print_hint_box(
        &i18n::t1("hints.init.title", "name", container_name),
        &[
            i18n::t("hints.init.link_line").to_string(),
            i18n::t("hints.init.size_line").to_string(),
            i18n::t("hints.init.recovery_line").to_string(),
            i18n::t1(
                "hints.init.path_line",
                "path",
                &link_path.display().to_string(),
            ),
        ],
    );

    show_extension_hint();
}

pub fn show_pack_explain_hint(
    container_name: &str,
    link_path: Option<&Path>,
    output_path: &str,
    size: u64,
) {
    if !should_show_once(HintType::FirstPack) {
        return;
    }

    let link_display = link_path
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| format!("{}.veil-link", container_name));
    let size_mb = format!("{:.2}", size as f64 / 1_048_576.0);

    print_hint_box(
        i18n::t("hints.pack.title"),
        &[
            i18n::t1("hints.pack.link_line", "path", &link_display),
            i18n::t("hints.pack.link_desc").to_string(),
            String::new(),
            i18n::t2(
                "hints.pack.output_line",
                "path",
                output_path,
                "size_mb",
                &size_mb,
            ),
            i18n::t("hints.pack.output_desc").to_string(),
        ],
    );
}

pub fn show_unpack_explain_hint(container_name: &str, link_path: &Path, file_count: usize) {
    if !should_show_once(HintType::FirstUnpack) {
        return;
    }

    let link_name = link_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("{}.veil-link", container_name));

    print_hint_box(
        &i18n::t2(
            "hints.unpack.title",
            "package",
            &format!("{}.veil", container_name),
            "link",
            &link_name,
        ),
        &[i18n::t3(
            "hints.unpack.body",
            "link",
            &link_name,
            "count",
            &file_count.to_string(),
            "name",
            container_name,
        )],
    );
}

pub fn show_link_recovery_hint(link_path: &Path) {
    if !allowed(HintType::LinkRecovery, current_level()) {
        return;
    }

    print_hint_box(
        i18n::t("hints.link_recovery.title"),
        &[i18n::t1(
            "hints.link_recovery.body",
            "path",
            &link_path.display().to_string(),
        )],
    );
}

pub fn show_file_type_ambiguity_hint(link_path: &Path, container_path: &Path) {
    if !allowed(HintType::FileTypeAmbiguity, current_level()) {
        return;
    }

    print_hint_box(
        i18n::t("hints.file_ambiguity.title"),
        &[i18n::t2(
            "hints.file_ambiguity.body",
            "link",
            &link_path.display().to_string(),
            "package",
            &container_path.display().to_string(),
        )],
    );
}

pub fn show_extension_hint() {
    if !should_show_once(HintType::ExtensionHidden) {
        return;
    }

    print_hint_box(
        i18n::t("hints.extension.title"),
        &[i18n::t("hints.extension.body").to_string()],
    );
}

pub fn show_files_help() {
    println!("\n{}", i18n::t("help.files.content"));
}

pub fn print_hint_box(title: &str, lines: &[String]) {
    println!();
    println!("{}", "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_black());
    println!("{}", title.cyan().bold());
    println!();
    for line in lines {
        println!("{}", line);
    }
    println!("{}", "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_black());
    println!();
    println!("{}", i18n::t("hints.dismiss").bright_black());
    println!();
}
