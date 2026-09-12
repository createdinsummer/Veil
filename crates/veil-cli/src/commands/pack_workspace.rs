//! `veil pack` 子命令：把工作区打包为单文件 `.veil`。

use crate::error::Result;
use colored::Colorize;
use std::path::Path;
use veil_core::container_format::ContainerPacker;
use veil_core::workspace_ops::WorkspaceManager;

/// 将容器元数据和所有加密文件封装为可分享的 `.veil` 文件。
///
/// 输出已存在时拒绝覆盖；未指定路径时使用 `<容器名>.vault.veil`。
///
/// # 错误
/// 容器解析、输出路径检查、密码读取、元数据读取或打包写入失败时返回错误。
pub fn run_workspace(
    container_name: &str,
    output_path: Option<&str>,
    password: Option<String>,
) -> Result<()> {
    // 打包目标是解析后的工作区，而不是输入链接文件。
    let resolved = super::resolve_container(container_name)?;
    let workspace_path = resolved.workspace_path;

    // 未指定输出时沿用命令约定的 <名称>.vault.veil。
    let output = if let Some(path) = output_path {
        path.to_string()
    } else {
        format!("{}.vault.veil", resolved.name)
    };

    // 打包文件整体覆写，因此必须显式拒绝已存在的输出路径。
    if Path::new(&output).exists() {
        crate::cli_bail!(PackOutputExists, "path" => &output);
    }

    crate::outln!("{}", crate::i18n::t("pack.in_progress").cyan());
    let password_str =
        super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 先完成密码验证和清单读取，再读取 .veil-meta 的完整字节。
    let manager = WorkspaceManager::new(workspace_path.clone());
    let metadata = manager.read_meta(password)?;

    let meta_path = workspace_path.join(".veil-meta");
    let meta_bytes = std::fs::read(&meta_path)?;

    // packer 只负责布局，不重新加密元数据或文件内容。
    let packer = ContainerPacker::new(&output);
    packer.pack(&workspace_path, &metadata, &meta_bytes)?;

    let output_size = std::fs::metadata(&output)?.len();

    crate::outln!(
        "{}",
        crate::i18n::t1("pack.created", "path", &output).green()
    );
    crate::outln!(
        "{}",
        crate::i18n::t2(
            "pack.stats",
            "count",
            &metadata.files.len().to_string(),
            "size",
            &format_size(output_size)
        )
        .bright_black()
    );

    crate::hints::show_pack_explain_hint(
        &resolved.name,
        resolved.link_path.as_deref(),
        &output,
        output_size,
    );

    Ok(())
}

/// 将字节数格式化为 B、KB、MB 或 GB，并保留两位小数。
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}
