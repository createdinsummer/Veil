use anyhow::Result;
use colored::Colorize;
use veil_core::config::GlobalConfig;
use veil_core::workspace_ops::WorkspaceManager;

/// 修改容器密码
pub fn run_workspace(
    container_name: &str,
    old_password: Option<String>,
    new_password: Option<String>,
) -> Result<()> {
    // 加载配置
    let config = GlobalConfig::load()?;

    // 获取容器工作区路径
    let workspace_path = config.get_container_workspace_path(container_name)?;

    println!("{}", "正在修改容器密码...".cyan());

    // 获取旧密码
    let old_pwd = super::prompt_password("请输入当前密码: ", old_password)?;

    // 获取新密码
    let new_pwd = super::prompt_new_password(new_password)?;

    use age::secrecy::ExposeSecret;
    let old_password_str = old_pwd.expose_secret();
    let new_password_str = new_pwd.expose_secret();

    // 使用 WorkspaceManager 修改密码
    let manager = WorkspaceManager::new(workspace_path);
    manager.change_password(old_password_str, new_password_str)?;

    println!("{}", "✓ 密码修改成功".green());

    Ok(())
}
