use anyhow::Result;
use colored::Colorize;
use veil_core::config::GlobalConfig;
use veil_core::workspace_ops::WorkspaceManager;
use std::path::Path;

/// 添加文件到工作区容器
pub fn run_workspace(
    container_name: &str,
    file_path: &str,
    password: Option<String>,
) -> Result<()> {
    // 加载配置
    let config = GlobalConfig::load()?;

    // 获取容器工作区路径
    let workspace_path = config.get_container_workspace_path(container_name)?;

    // 检查文件是否存在
    let file = Path::new(file_path);
    if !file.exists() {
        anyhow::bail!("文件不存在: {}", file_path);
    }

    if !file.is_file() {
        anyhow::bail!("不是文件: {}", file_path);
    }

    println!("{}", "正在加密文件...".cyan());
    let password_str = super::prompt_password("请输入容器密码: ", password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 添加文件
    let manager = WorkspaceManager::new(workspace_path);
    let encrypted_name = manager.add_file(file, password)?;

    let file_name = file.file_name().unwrap().to_string_lossy();
    println!("{}", format!("✓ 已添加文件: {}", file_name).green());
    println!("{}", format!("  加密名: {}", encrypted_name).bright_black());

    Ok(())
}
