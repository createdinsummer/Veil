use anyhow::Result;
use colored::Colorize;
use veil_core::config::GlobalConfig;
use veil_core::workspace_ops::WorkspaceManager;

/// 显示容器详细信息（工作区架构）。
///
/// 输出容器的元数据和内容统计。
///
/// # 参数
/// - `container_name`: 容器名称
/// - `password`: 容器密码（`None` 则交互式输入）
///
/// # 返回
/// - `Ok(())`: 成功显示信息
/// - `Err(anyhow::Error)`: 失败
///
/// # 示例
/// ```bash
/// veil info photos
/// ```
pub fn run(container_name: &str, password: Option<String>) -> Result<()> {
    // 加载配置
    let config = GlobalConfig::load()?;

    // 获取容器工作区路径
    let workspace_path = config.get_container_workspace_path(container_name)?;

    let password_str = super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 读取元数据
    let manager = WorkspaceManager::new(workspace_path.clone());
    let metadata = manager.read_meta(password)?;

    println!("\n{}", crate::i18n::t("info.title").cyan().bold());
    println!("  容器名称: {}", metadata.container_name);
    println!("  工作区路径: {}", workspace_path.display());
    println!("  工作区类型: {}", metadata.workspace_type);
    println!("  创建时间: {}", metadata.created_at);

    // 统计文件
    let file_count = metadata.files.len();
    let total_size: u64 = metadata.files.iter().map(|f| f.size).sum();

    println!("\n{}", crate::i18n::t("info.content_title").cyan());
    println!("{}", crate::i18n::t1("info.content_count", "count", &file_count.to_string()));
    println!("{}", crate::i18n::t2("info.content_size",
        "bytes", &total_size.to_string(),
        "mb", &format!("{:.2}", total_size as f64 / 1_048_576.0)));

    // 列出文件
    if !metadata.files.is_empty() {
        println!("\n文件列表:");
        for (idx, file) in metadata.files.iter().enumerate() {
            println!("  {}. {} ({} bytes)",
                idx + 1,
                file.original_name,
                file.size
            );
        }
    }

    println!();
    Ok(())
}
