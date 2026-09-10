use anyhow::Result;
use colored::Colorize;
use std::path::Path;
use veil_core::container_format::ContainerPacker;
use veil_core::workspace_ops::WorkspaceManager;

/// 打包工作区到 .veil 容器文件
pub fn run_workspace(
    container_name: &str,
    output_path: Option<&str>,
    password: Option<String>,
) -> Result<()> {
    let resolved = super::resolve_container(container_name)?;
    let workspace_path = resolved.workspace_path;

    // 确定输出路径
    let output = if let Some(path) = output_path {
        path.to_string()
    } else {
        format!("{}.vault.veil", resolved.name)
    };

    // 检查输出文件是否已存在
    if Path::new(&output).exists() {
        anyhow::bail!("{}", crate::i18n::t1("pack.output_exists", "path", &output));
    }

    println!("{}", crate::i18n::t("pack.in_progress").cyan());
    let password_str =
        super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 读取元数据
    let manager = WorkspaceManager::new(workspace_path.clone());
    let metadata = manager.read_meta(password)?;

    // 读取加密的元数据（完整的 TLV 格式，包含头部）
    let meta_path = workspace_path.join(".veil-meta");
    let meta_bytes = std::fs::read(&meta_path)?;

    // 打包
    let packer = ContainerPacker::new(&output);
    packer.pack(&workspace_path, &metadata, &meta_bytes)?;

    // 计算总大小
    let output_size = std::fs::metadata(&output)?.len();

    println!(
        "{}",
        crate::i18n::t1("pack.created", "path", &output).green()
    );
    println!(
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

    // 显示打包解释提示
    crate::hints::show_pack_explain_hint(
        &resolved.name,
        resolved.link_path.as_deref(),
        &output,
        output_size,
    );

    Ok(())
}

/// 格式化文件大小
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
