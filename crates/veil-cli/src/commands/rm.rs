//! `veil rm` 子命令：删除工作区容器中的文件。

use crate::error::Result;
use colored::Colorize;
use veil_core::workspace_ops::{RemovedPathKind, WorkspaceManager};

/// 从容器删除文件或目录（工作区架构）。
///
/// 删除容器内的文件和加密数据。
///
/// # 参数
/// - `container_name`: 容器名称
/// - `file_name`: 要删除的容器内文件或目录路径
/// - `password`: 容器密码（`None` 则交互式输入）
///
/// # 返回
/// - `Ok(())`: 文件或目录已删除
/// - `Err(CommandError)`: 失败
///
/// # 示例
/// ```bash
/// veil rm photos vacation.jpg
/// ```
pub fn run(container_name: &str, file_name: &str, password: Option<String>) -> Result<()> {
    // 先解析链接，确保后续删除针对的是最终工作区而不是输入本身。
    let resolved = super::resolve_container(container_name)?;
    let workspace_path = resolved.workspace_path;

    // 删除前先输出目标；真正删除仍需要密码和元数据查找成功。
    println!(
        "{}",
        crate::i18n::t1("rm.deleting", "path", file_name).yellow()
    );
    let password_str =
        super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // core 会先提交更新后的元数据，再清理失去引用的密文文件。
    let manager = WorkspaceManager::new(workspace_path);
    let removed = manager.remove_path(file_name, password)?;

    let message_key = match removed {
        RemovedPathKind::File => "rm.deleted",
        RemovedPathKind::Directory => "rm.deleted_directory",
    };
    println!(
        "{}",
        crate::i18n::t1(message_key, "path", file_name).green()
    );

    Ok(())
}
