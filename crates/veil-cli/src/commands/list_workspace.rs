use anyhow::Result;
use colored::Colorize;
use veil_core::workspace_ops::WorkspaceManager;

/// 列出容器中的所有文件
pub fn run_workspace(container_name: &str, password: Option<String>) -> Result<()> {
    let resolved = super::resolve_container(container_name)?;
    let workspace_path = resolved.workspace_path;

    let password_str = super::prompt_password("请输入容器密码: ", password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 列出文件
    let manager = WorkspaceManager::new(workspace_path);
    let files = manager.list_files(password)?;

    if files.is_empty() {
        println!("{}", crate::i18n::t("common.empty").yellow());
        return Ok(());
    }

    println!(
        "\n{}",
        crate::i18n::t2(
            "list.workspace_title",
            "name",
            container_name,
            "count",
            &files.len().to_string()
        )
        .cyan()
        .bold()
    );
    println!();

    // 计算总大小
    let total_size: u64 = files.iter().map(|f| f.size).sum();

    for (idx, file) in files.iter().enumerate() {
        println!(
            "  {} {}",
            format!("{}.", idx + 1).bright_black(),
            file.original_name.bright_white()
        );
        println!(
            "     大小: {}  加密时间: {}",
            format_size(file.size),
            format_time(&file.encrypted_at)
        );
    }

    println!();
    println!(
        "{}",
        crate::i18n::t1("list.total_size", "size", &format_size(total_size)).bright_black()
    );
    println!();

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

/// 格式化时间
fn format_time(rfc3339: &str) -> String {
    use chrono::{DateTime, Local};

    if let Ok(dt) = DateTime::parse_from_rfc3339(rfc3339) {
        let local: DateTime<Local> = dt.into();
        local.format("%Y-%m-%d %H:%M:%S").to_string()
    } else {
        rfc3339.to_string()
    }
}
