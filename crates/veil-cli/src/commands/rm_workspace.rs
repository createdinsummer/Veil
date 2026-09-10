use anyhow::Result;
use colored::Colorize;
use veil_core::config::GlobalConfig;
use veil_core::workspace_ops::WorkspaceManager;

/// 从容器删除文件
pub fn run_workspace(
    container_name: &str,
    file_name: &str,
    password: Option<String>,
) -> Result<()> {
    // 加载配置
    let config = GlobalConfig::load()?;

    // 获取容器工作区路径
    let workspace_path = config.get_container_workspace_path(container_name)?;

    println!("{}", format!("正在删除文件: {}", file_name).yellow());
    let password_str = super::prompt_password("请输入容器密码: ", password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 删除文件
    let manager = WorkspaceManager::new(workspace_path);
    manager.remove_file(file_name, password)?;

    println!("{}", format!("✓ 已删除文件: {}", file_name).green());

    Ok(())
}
