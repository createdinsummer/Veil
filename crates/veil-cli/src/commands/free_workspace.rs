//! `veil free` 子命令：以简化树状视图展示容器内容。

use crate::error::Result;
use colored::Colorize;
use veil_core::workspace_ops::WorkspaceManager;

/// 按原始相对路径排序并缩进展示文件，同时输出文件和总大小统计。
///
/// # 错误
/// 容器解析、密码读取或元数据解密失败时返回错误。
pub fn run_workspace(container_name: &str, password: Option<String>) -> Result<()> {
    // 解析链接后只保留工作区路径，展示名称仍以元数据为准确认。
    let resolved = super::resolve_container(container_name)?;
    let workspace_path = resolved.workspace_path;

    let password_str =
        super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 解密清单后即可完整展示树状内容，无需读取文件密文本体。
    let manager = WorkspaceManager::new(workspace_path);
    let metadata = manager.read_meta(password)?;

    println!("\n{}", crate::i18n::t("free.title").cyan().bold());

    if metadata.files.is_empty() {
        println!(
            "{}",
            format!("  {}", crate::i18n::t("common.empty")).bright_black()
        );
    } else {
        // 先按完整虚拟路径排序，让同目录文件在输出中保持相邻。
        let mut sorted_files = metadata.files.clone();
        sorted_files.sort_by(|a, b| a.original_name.cmp(&b.original_name));

        for (i, file) in sorted_files.iter().enumerate() {
            let is_last = i == sorted_files.len() - 1;
            let connector = if is_last { "└── " } else { "├── " };

            let name = &file.original_name;
            if name.contains('/') {
                let parts: Vec<&str> = name.split('/').collect();
                let indent = "    ".repeat(parts.len().saturating_sub(1));
                println!(
                    "{}{}{} ({})",
                    indent,
                    connector,
                    parts.last().unwrap(),
                    format_size(file.size)
                );
            } else {
                println!("{}{} ({})", connector, name, format_size(file.size));
            }
        }
    }

    // 统计信息直接基于元数据中的明文大小，不会触发额外解密。
    let file_count = metadata.files.len();
    let total_size: u64 = metadata.files.iter().map(|f| f.size).sum();

    println!("\n{}", crate::i18n::t("free.stats_title").cyan());
    println!(
        "{}",
        crate::i18n::t1("free.file_count", "count", &file_count.to_string())
    );
    println!(
        "{}",
        crate::i18n::t2(
            "free.total_size",
            "bytes",
            &total_size.to_string(),
            "mb",
            &format!("{:.2}", total_size as f64 / 1_048_576.0)
        )
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
