use anyhow::Result;
use colored::Colorize;
use veil_core::config::GlobalConfig;
use veil_core::workspace_ops::WorkspaceManager;

/// 树状显示容器内容
pub fn run_workspace(
    container_name: &str,
    _force: bool,
) -> Result<()> {
    // 加载配置
    let config = GlobalConfig::load()?;

    // 获取容器工作区路径
    let workspace_path = config.get_container_workspace_path(container_name)?;

    // 提示输入密码
    let password_str = super::prompt_password("请输入容器密码: ", None)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 读取元数据
    let manager = WorkspaceManager::new(workspace_path);
    let metadata = manager.read_meta(password)?;

    println!("\n{}", "容器内容:".cyan().bold());

    if metadata.files.is_empty() {
        println!("{}", "  (空)".bright_black());
    } else {
        // 简单列表显示，带缩进表示层级
        let mut sorted_files = metadata.files.clone();
        sorted_files.sort_by(|a, b| a.original_name.cmp(&b.original_name));

        for (i, file) in sorted_files.iter().enumerate() {
            let is_last = i == sorted_files.len() - 1;
            let connector = if is_last { "└── " } else { "├── " };

            // 简单处理：如果有路径分隔符，显示为目录结构
            let name = &file.original_name;
            if name.contains('/') {
                let parts: Vec<&str> = name.split('/').collect();
                let indent = "    ".repeat(parts.len().saturating_sub(1));
                println!("{}{}{} ({})", indent, connector, parts.last().unwrap(), format_size(file.size));
            } else {
                println!("{}{} ({})", connector, name, format_size(file.size));
            }
        }
    }

    // 统计信息
    let file_count = metadata.files.len();
    let total_size: u64 = metadata.files.iter().map(|f| f.size).sum();

    println!("\n{}", "统计信息:".cyan());
    println!("  文件数量: {}", file_count);
    println!("  总大小: {} 字节 ({:.2} MB)", total_size, total_size as f64 / 1_048_576.0);

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
