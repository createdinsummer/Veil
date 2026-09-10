use anyhow::Result;
use colored::Colorize;
use veil_core::config::GlobalConfig;
use veil_core::workspace_ops::WorkspaceManager;

/// 从容器删除文件（工作区架构）。
///
/// 删除容器内的文件和加密数据。
///
/// # 参数
/// - `container_name`: 容器名称
/// - `file_name`: 要删除的文件名
/// - `password`: 容器密码（`None` 则交互式输入）
///
/// # 返回
/// - `Ok(())`: 文件已删除
/// - `Err(anyhow::Error)`: 失败
///
/// # 示例
/// ```bash
/// veil rm photos vacation.jpg
/// ```
pub fn run(container_name: &str, file_name: &str, password: Option<String>) -> Result<()> {
    // 加载配置
    let config = GlobalConfig::load()?;

    // 获取容器工作区路径
    let workspace_path = config.get_container_workspace_path(container_name)?;

    println!("{}", crate::i18n::t1("rm.deleting", "path", file_name).yellow());
    let password_str = super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 删除文件
    let manager = WorkspaceManager::new(workspace_path);
    manager.remove_file(file_name, password)?;

    println!("{}", crate::i18n::t1("rm.deleted", "path", file_name).green());

    Ok(())
}
