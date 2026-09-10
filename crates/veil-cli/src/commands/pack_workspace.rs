use anyhow::Result;
use colored::Colorize;
use veil_core::config::GlobalConfig;
use veil_core::container_format::ContainerPacker;
use veil_core::workspace_ops::WorkspaceManager;
use std::path::Path;

/// 打包工作区到 .veil 容器文件
pub fn run_workspace(
    container_name: &str,
    output_path: Option<&str>,
    password: Option<String>,
) -> Result<()> {
    // 加载配置
    let config = GlobalConfig::load()?;

    // 获取容器工作区路径
    let workspace_path = config.get_container_workspace_path(container_name)?;

    // 确定输出路径
    let output = if let Some(path) = output_path {
        path.to_string()
    } else {
        format!("{}.veil", container_name)
    };

    // 检查输出文件是否已存在
    if Path::new(&output).exists() {
        anyhow::bail!("输出文件已存在: {}", output);
    }

    println!("{}", "正在打包容器...".cyan());
    let password_str = super::prompt_password("请输入容器密码: ", password)?;

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

    println!("{}", format!("✓ 已打包到: {}", output).green());
    println!("{}", format!("  文件数: {}  大小: {}", metadata.files.len(), format_size(output_size)).bright_black());

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
