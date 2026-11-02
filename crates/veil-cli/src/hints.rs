//! 一次性操作提示的判定与渲染。
//!
//! 提示级别可由 `VEIL_HINTS` 临时覆盖，否则读取全局配置。首次创建、打包和解包提示
//! 会把“已展示”状态写回配置；链接恢复提示属于条件提示，不记录次数。

use crate::error::Result;
use crate::i18n;
use colored::Colorize;
use std::path::Path;
use veil_service::config::HintLevel;
use veil_service::hints::HintKind;
use veil_service::{ServiceRequest, ServiceResponse};

/// 当前生效的提示级别。
pub fn current_level() -> HintLevel {
    let value = std::env::var("VEIL_HINTS").ok();
    match crate::commands::request(ServiceRequest::EffectiveHintLevel {
        override_value: value.as_deref(),
    }) {
        Ok(ServiceResponse::EffectiveHintLevel(level)) => level,
        _ => HintLevel::Full,
    }
}

/// 请求服务层保存提示级别。
pub fn set_hint_level(level: &str) -> Result<HintLevel> {
    match crate::commands::request(ServiceRequest::SetHintLevel { level })? {
        ServiceResponse::HintLevelSet(level) => Ok(level),
        _ => Err(veil_core::error::VeilError::WorkError("服务响应类型不符合请求".into()).into()),
    }
}

fn should_show(kind: HintKind) -> bool {
    let value = std::env::var("VEIL_HINTS").ok();
    match crate::commands::request(ServiceRequest::ShouldShowHint {
        kind,
        override_value: value.as_deref(),
    }) {
        Ok(ServiceResponse::ShowHint(show)) => show,
        _ => false,
    }
}

/// 展示首次创建容器后的链接、工作区和打包/解包说明。
pub fn show_first_init_hint(veil_name: &str, link_path: &Path, veil_dir: &Path) {
    if !should_show(HintKind::FirstInit) {
        return;
    }

    // 优先使用实际链接文件名，异常路径才回退到容器名。
    let link_name = link_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("{}.veil-link", veil_name));

    crate::outln!();
    crate::outln!("{}", i18n::t1("hints.init.title", "name", &link_name));
    crate::outln!();
    crate::outln!("  {}", i18n::t("hints.init.storage_line"));
    crate::outln!("    {}", veil_dir.display());
    crate::outln!();
    crate::outln!("  {}", i18n::t("hints.init.pack_title"));
    crate::outln!(
        "    {}",
        i18n::t1("hints.init.pack_command", "veil", &link_name)
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
    veil_name: &str,
    link_path: Option<&Path>,
    output_path: &str,
    size: u64,
) {
    if !should_show(HintKind::FirstPack) {
        return;
    }

    // 打包可能从链接路径或容器名触发，两种来源都要正确展示。
    let link_display = link_path
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| format!("{}.veil-link", veil_name));
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
pub fn show_unpack_explain_hint(veil_name: &str, link_path: &Path, file_count: usize) {
    if !should_show(HintKind::FirstUnpack) {
        return;
    }

    // 解包提示重点说明新链接与包文件的对应关系。
    let link_name = link_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("{}.veil-link", veil_name));

    print_hint_box(
        &i18n::t2(
            "hints.unpack.title",
            "package",
            &format!("{}.veil", veil_name),
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
            veil_name,
        )],
    );
}

/// 提示指定链接已通过配置缓存恢复。
pub fn show_link_recovery_hint(link_path: &Path) {
    if !should_show(HintKind::LinkRecovery) {
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
