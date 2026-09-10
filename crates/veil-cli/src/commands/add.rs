use anyhow::Result;
use colored::Colorize;
use std::path::Path;
use veil_core::config::GlobalConfig;
use veil_core::workspace_ops::WorkspaceManager;

/// 添加文件到容器（工作区架构）。
///
/// 将本地文件加密后添加到工作区容器中。
///
/// # 参数
/// - `container_name`: 容器名称
/// - `source`: 源文件路径
/// - `dest`: 目标路径（暂未使用，保留兼容性）
/// - `password`: 容器密码（`None` 则交互式输入）
///
/// # 返回
/// - `Ok(())`: 文件已加密并添加到容器
/// - `Err(anyhow::Error)`: 失败
///
/// # 示例
/// ```bash
/// veil add photos vacation.jpg
/// veil add photos ~/Pictures/photo.jpg
/// ```
pub fn run(container_name: &str, source: &str, _dest: Option<&str>, password: Option<String>) -> Result<()> {
    // 加载配置
    let config = GlobalConfig::load()?;

    // 获取容器工作区路径
    let workspace_path = config.get_container_workspace_path(container_name)?;

    // 检查文件是否存在
    let file = Path::new(source);
    if !file.exists() {
        anyhow::bail!("{}", crate::i18n::t1("add.source_not_found", "path", source));
    }

    if !file.is_file() {
        anyhow::bail!("不是文件: {}（目录支持即将推出）", source);
    }

    println!("{}", crate::i18n::t("opening_container").cyan());
    let password_str = super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 添加文件
    let manager = WorkspaceManager::new(workspace_path);
    let encrypted_name = manager.add_file(file, password)?;

    let file_name = file.file_name().unwrap().to_string_lossy();
    println!("{}", crate::i18n::t1("add.file_added", "path", &file_name).green());
    println!("{}", format!("  加密名: {}", encrypted_name).bright_black());

    Ok(())
}
