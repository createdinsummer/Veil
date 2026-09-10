use anyhow::Result;
use colored::Colorize;
use veil_core::workspace_ops::WorkspaceManager;

/// 修改容器密码
pub fn run_workspace(
    container_name: &str,
    old_password: Option<String>,
    new_password: Option<String>,
) -> Result<()> {
    let resolved = super::resolve_container(container_name)?;
    let workspace_path = resolved.workspace_path;

    println!("{}", crate::i18n::t("passwd.changing").cyan());

    // 获取旧密码
    let old_pwd = super::prompt_password(crate::i18n::t("prompt.current_password"), old_password)?;

    // 获取新密码
    let new_pwd = super::prompt_new_password(new_password)?;

    use age::secrecy::ExposeSecret;
    let old_password_str = old_pwd.expose_secret();
    let new_password_str = new_pwd.expose_secret();

    // 使用 WorkspaceManager 修改密码
    let manager = WorkspaceManager::new(workspace_path);
    manager.change_password(old_password_str, new_password_str)?;

    println!("{}", crate::i18n::t("passwd.changed").green());

    Ok(())
}
