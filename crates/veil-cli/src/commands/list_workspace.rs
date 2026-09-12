//! `veil list` 子命令：列出工作区容器的文件清单。

use crate::error::Result;
use colored::Colorize;
use veil_core::workspace_ops::WorkspaceManager;

/// 解密元数据并输出文件名称、大小、加密时间和总大小。
///
/// # 错误
/// 容器解析、密码读取或元数据解密失败时返回错误。
pub fn run_workspace(container_name: &str, password: Option<String>) -> Result<()> {
    // list 只读取容器元数据，不需要打开每个文件 blob。
    let resolved = super::resolve_container(container_name)?;
    let workspace_path = resolved.workspace_path;

    let password_str = super::prompt_password("请输入容器密码: ", password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 读取清单成功即表示密码正确，随后即可安全展示文件信息。
    let manager = WorkspaceManager::new(workspace_path);
    let files = manager.list_files(password)?;

    // 空容器提前返回，避免打印只有标题和总大小 0 的表格。
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

    // 文件条目保存明文大小，可直接求和给用户展示。
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

/// 将 RFC 3339 时间转换为本地 `YYYY-MM-DD HH:MM:SS`；解析失败时原样返回。
fn format_time(rfc3339: &str) -> String {
    use chrono::{DateTime, Local};

    if let Ok(dt) = DateTime::parse_from_rfc3339(rfc3339) {
        let local: DateTime<Local> = dt.into();
        local.format("%Y-%m-%d %H:%M:%S").to_string()
    } else {
        rfc3339.to_string()
    }
}
