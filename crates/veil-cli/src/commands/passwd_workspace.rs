//! `veil passwd` 子命令：修改工作区容器密码。

use crate::error::Result;
use colored::Colorize;

/// 读取旧密码和新密码，并重新保护容器密钥。
///
/// 默认只修改密码保护层，不重新加密文件；`full` 为真时轮换数据主密钥并逐个
/// 重新加密全部文件。
///
/// # 错误
/// 容器解析、旧密码验证、新密码确认或重新加密失败时返回错误。
pub fn run_workspace(
    container_name: &str,
    old_password: Option<String>,
    new_password: Option<String>,
    full: bool,
) -> Result<()> {
    // 密码修改必须作用于容器工作区，而不是链接文件本身。
    let resolved = super::resolve_container(container_name)?;
    let manager = super::workspace_manager(&resolved);

    crate::outln!("{}", crate::i18n::t("passwd.changing").cyan());

    // 旧密码用于验证身份并解密现有元数据。
    let old_pwd = super::prompt_password(crate::i18n::t("prompt.current_password"), old_password)?;

    // 新密码用于重新派生密码保护密钥；完整模式还会轮换数据主密钥。
    let new_pwd = super::prompt_new_password_with_env(new_password, "VEIL_NEW_PASSWORD")?;

    use age::secrecy::ExposeSecret;
    let old_password_str = old_pwd.expose_secret();
    let new_password_str = new_pwd.expose_secret();

    // core 在修改过程中保持原始 veil_id 和工作区结构不变。
    if full {
        manager.change_password_full(old_password_str, new_password_str)?;
    } else {
        manager.change_password(old_password_str, new_password_str)?;
    }

    crate::outln!("{}", crate::i18n::t("passwd.changed").green());

    Ok(())
}
