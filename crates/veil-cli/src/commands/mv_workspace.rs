use anyhow::Result;
use colored::Colorize;
use veil_core::config::GlobalConfig;
use veil_core::workspace_ops::WorkspaceManager;

/// 移动或重命名容器内的文件
pub fn run_workspace(
    container_name: &str,
    from: &str,
    to: &str,
    password: Option<String>,
) -> Result<()> {
    // 加载配置
    let config = GlobalConfig::load()?;

    // 获取容器工作区路径
    let workspace_path = config.get_container_workspace_path(container_name)?;

    // 获取密码
    let password_str = super::prompt_password("请输入容器密码: ", password)?;

    use age::secrecy::ExposeSecret;
    let pwd = password_str.expose_secret();

    println!("{}", format!("正在打开容器 '{}'...", container_name).cyan());

    // 使用 WorkspaceManager 重命名文件
    let manager = WorkspaceManager::new(workspace_path);
    manager.rename_file(from, to, pwd)?;

    println!("{}", format!("✓ 已重命名: {} -> {}", from, to).green());

    Ok(())
}
