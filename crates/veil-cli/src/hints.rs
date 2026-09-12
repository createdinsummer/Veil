//! 一次性操作提示的判定与渲染。
//!
//! 提示级别可由 `VEIL_HINTS` 临时覆盖，否则读取全局配置。首次创建、打包和解包提示
//! 会把“已展示”状态写回配置；链接恢复和文件类型歧义提示属于条件提示，不记录次数。

use crate::i18n;
use crate::error::Result;
use colored::Colorize;
use std::path::Path;
use veil_core::config::{GlobalConfig, HintsLevel};

/// 当前支持的提示场景。
#[derive(Debug, Clone, Copy)]
enum HintType {
    /// 首次创建容器后的使用说明。
    FirstInit,
    /// 首次打包后的文件用途说明。
    FirstPack,
    /// 首次解包后的文件映射说明。
    FirstUnpack,
    /// 链接缺失并完成恢复后的说明。
    LinkRecovery,
    /// 同名链接和打包文件同时存在时的说明。
    FileTypeAmbiguity,
}

/// 当前生效的提示级别：`VEIL_HINTS` 环境变量优先，其次读取全局配置。
pub fn current_level() -> HintsLevel {
    // 环境变量是临时覆盖层，只有合法值才允许覆盖持久化配置。
    if let Ok(value) = std::env::var("VEIL_HINTS") {
        if let Some(level) = HintsLevel::parse(&value) {
            return level;
        }
    }

    // 配置读取失败时回退到完整提示，保证新环境仍能获得帮助信息。
    GlobalConfig::load()
        .map(|config| config.preferences.hints_level)
        .unwrap_or(HintsLevel::Full)
}

/// 解析并持久化提示级别。
///
/// # 错误
/// 级别无效、全局配置加载失败或保存失败时返回错误。
pub fn set_hint_level(level: &str) -> Result<()> {
    let parsed =
        HintsLevel::parse(level).ok_or_else(|| crate::cli_error!(ConfigInvalidLevel))?;
    let mut config = GlobalConfig::load()?;
    config.preferences.hints_level = parsed;
    config.save()?;
    Ok(())
}

/// 判断指定提示在当前级别下是否允许展示。
fn allowed(hint_type: HintType, level: HintsLevel) -> bool {
    // Brief 只保留解包和异常恢复类关键提示。
    match level {
        HintsLevel::Off => false,
        HintsLevel::Brief => matches!(
            hint_type,
            HintType::FirstUnpack | HintType::LinkRecovery | HintType::FileTypeAmbiguity
        ),
        HintsLevel::Full => true,
    }
}

/// 对需要记录状态的提示执行级别检查和一次性判定。
///
/// 返回 `true` 时立即把对应展示标记写回配置；配置读写失败时回退到仅按级别判断。
fn should_show_once(hint_type: HintType) -> bool {
    let mut config = match GlobalConfig::load() {
        Ok(config) => config,
        Err(_) => return allowed(hint_type, current_level()),
    };

    if !allowed(hint_type, current_level()) {
        return false;
    }

    // 只有首次使用类提示需要写回“已展示”状态，条件提示每次都允许出现。
    let already_shown = match hint_type {
        HintType::FirstInit => config.system.init_hint_shown,
        HintType::FirstPack => config.system.pack_hint_shown,
        HintType::FirstUnpack => config.system.unpack_hint_shown,
        HintType::LinkRecovery | HintType::FileTypeAmbiguity => return true,
    };

    if already_shown {
        return false;
    }

    // 标记成功后才尝试保存；保存失败不会阻止本次提示展示。
    match hint_type {
        HintType::FirstInit => config.system.init_hint_shown = true,
        HintType::FirstPack => config.system.pack_hint_shown = true,
        HintType::FirstUnpack => config.system.unpack_hint_shown = true,
        HintType::LinkRecovery | HintType::FileTypeAmbiguity => {}
    }

    let _ = config.save();
    true
}

/// 展示首次创建容器后的链接、工作区和打包/解包说明。
pub fn show_first_init_hint(container_name: &str, link_path: &Path, workspace_path: &Path) {
    if !allowed(HintType::FirstInit, current_level()) {
        return;
    }

    // 优先使用实际链接文件名，异常路径才回退到容器名。
    let link_name = link_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("{}.veil-link", container_name));

    crate::outln!();
    crate::outln!("{}", i18n::t1("hints.init.title", "name", &link_name));
    crate::outln!();
    crate::outln!("  {}", i18n::t("hints.init.storage_line"));
    crate::outln!("    {}", workspace_path.display());
    crate::outln!();
    crate::outln!("  {}", i18n::t("hints.init.pack_title"));
    crate::outln!(
        "    {}",
        i18n::t1("hints.init.pack_command", "container", &link_name)
    );
    crate::outln!();
    crate::outln!("  {}", i18n::t("hints.init.unpack_title"));
    crate::outln!("    {}", i18n::t("hints.init.unpack_command"));
    crate::outln!();
    crate::outln!("  {}", i18n::t("hints.init.close_title"));
    crate::outln!("    {}", i18n::t("hints.init.close_command"));
    crate::outln!();
}

/// 展示首次打包结果中链接与 `.veil` 文件的关系。
pub fn show_pack_explain_hint(
    container_name: &str,
    link_path: Option<&Path>,
    output_path: &str,
    size: u64,
) {
    if !should_show_once(HintType::FirstPack) {
        return;
    }

    // 打包可能从链接路径或容器名触发，两种来源都要正确展示。
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

/// 展示首次解包后链接、文件数量和容器名称的对应关系。
pub fn show_unpack_explain_hint(container_name: &str, link_path: &Path, file_count: usize) {
    if !should_show_once(HintType::FirstUnpack) {
        return;
    }

    // 解包提示重点说明新链接与包文件的对应关系。
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

/// 提示指定链接已通过配置缓存恢复。
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

/// 提示同名链接与打包文件同时存在，并说明当前采用链接。
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

/// 输出文件类型帮助主题的正文。
pub fn show_files_help() {
    crate::outln!("\n{}", i18n::t("help.files.content"));
}

/// 使用统一边框输出标题和多行提示，并在末尾显示关闭提示的方法。
pub fn print_hint_box(title: &str, lines: &[String]) {
    crate::outln!();
    crate::outln!("{}", "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_black());
    crate::outln!("{}", title.cyan().bold());
    crate::outln!();
    for line in lines {
        crate::outln!("{}", line);
    }
    crate::outln!("{}", "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━".bright_black());
    crate::outln!();
    crate::outln!("{}", i18n::t("hints.dismiss").bright_black());
    crate::outln!();
}
