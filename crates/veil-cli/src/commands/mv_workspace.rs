use anyhow::Result;
use colored::Colorize;
use veil_core::workspace_ops::WorkspaceManager;

/// 移动或重命名容器内的文件
pub fn run_workspace(
    container_name: &str,
    from: &str,
    to: &str,
    password: Option<String>,
) -> Result<()> {
    let resolved = super::resolve_container(container_name)?;
    let workspace_path = resolved.workspace_path;

    // 获取密码
    let password_str =
        super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    use age::secrecy::ExposeSecret;
    let pwd = password_str.expose_secret();

    println!(
        "{}",
        crate::i18n::t1("mv.opening_named", "name", container_name).cyan()
    );

    // 使用 WorkspaceManager 重命名文件
    let manager = WorkspaceManager::new(workspace_path);
    manager.rename_file(from, to, pwd)?;

    println!(
        "{}",
        crate::i18n::t2("mv.renamed", "from", from, "to", to).green()
    );

    Ok(())
}
