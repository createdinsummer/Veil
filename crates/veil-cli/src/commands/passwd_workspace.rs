//! `veil passwd` 子命令：修改工作区容器密码。

use crate::error::Result;
use colored::Colorize;
use veil_core::workspace_ops::WorkspaceManager;

/// 读取旧密码和新密码，并重新加密容器元数据和所有文件内容。
///
/// # 错误
/// 容器解析、旧密码验证、新密码确认或重新加密失败时返回错误。
pub fn run_workspace(
    container_name: &str,
    old_password: Option<String>,
    new_password: Option<String>,
) -> Result<()> {
    // 密码修改必须作用于容器工作区，而不是链接文件本身。
    let resolved = super::resolve_container(container_name)?;
    let workspace_path = resolved.workspace_path;

    println!("{}", crate::i18n::t("passwd.changing").cyan());

    // 旧密码用于验证身份并解密现有元数据。
    let old_pwd = super::prompt_password(crate::i18n::t("prompt.current_password"), old_password)?;

    // 新密码会用于重新派生主密钥，并重新加密元数据和所有文件。
    let new_pwd = super::prompt_new_password(new_password)?;

    use age::secrecy::ExposeSecret;
    let old_password_str = old_pwd.expose_secret();
    let new_password_str = new_pwd.expose_secret();

    // core 在修改过程中保持原始 veil_id 和工作区结构不变。
    let manager = WorkspaceManager::new(workspace_path);
    manager.change_password(old_password_str, new_password_str)?;

    println!("{}", crate::i18n::t("passwd.changed").green());

    Ok(())
}
