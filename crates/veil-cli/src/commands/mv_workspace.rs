//! `veil mv` 子命令：移动或重命名容器内的文件。

use crate::error::Result;
use colored::Colorize;

/// 移动或重命名文件、目录在容器元数据中的原始路径。
///
/// 密文文件本身不移动；目标路径已存在时由 [`WorkspaceManager::move_path`] 拒绝。
///
/// # 错误
/// 容器解析、密码读取、源不存在、目标冲突、目录循环或元数据写回失败时返回错误。
pub fn run_workspace(
    container_name: &str,
    from: &str,
    to: &str,
    password: Option<String>,
) -> Result<()> {
    // mv 只改变元数据中的原始路径，不复制或重新加密 blob。
    let resolved = super::resolve_container(container_name)?;
    let manager = super::workspace_manager(&resolved);

    let password_str =
        super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    use age::secrecy::ExposeSecret;
    let pwd = password_str.expose_secret();

    // 尽早显示目标容器，方便用户确认链接解析结果。
    crate::outln!(
        "{}",
        crate::i18n::t1("mv.opening_named", "name", container_name).cyan()
    );

    // 移动失败时原元数据保持不变，成功后再打印最终目标路径。
    let (_, final_target) = manager.move_path(from, to, pwd)?;

    crate::outln!(
        "{}",
        crate::i18n::t2("mv.moved", "from", from, "to", &final_target).green()
    );

    Ok(())
}
