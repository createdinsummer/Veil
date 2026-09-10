use anyhow::Result;
use colored::Colorize;
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
    let resolved = super::resolve_container(container_name)?;
    let workspace_path = resolved.workspace_path;

    let password_str =
        super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 读取元数据
    let manager = WorkspaceManager::new(workspace_path.clone());
    let metadata = manager.read_meta(password)?;

    println!("\n{}", crate::i18n::t("info.title").cyan().bold());
    println!(
        "{}",
        crate::i18n::t1("info.container_name", "name", &metadata.container_name)
    );
    println!(
        "{}",
        crate::i18n::t1(
            "info.workspace_path",
            "path",
            &workspace_path.display().to_string()
        )
    );
    println!(
        "{}",
        crate::i18n::t1(
            "info.workspace_type",
            "workspace_type",
            &metadata.workspace_type
        )
    );
    println!(
        "{}",
        crate::i18n::t1("info.created_at", "time", &metadata.created_at)
    );

    // 统计文件
    let file_count = metadata.files.len();
    let total_size: u64 = metadata.files.iter().map(|f| f.size).sum();

    println!("\n{}", crate::i18n::t("info.content_title").cyan());
    println!(
        "{}",
        crate::i18n::t1("info.content_count", "count", &file_count.to_string())
    );
    println!(
        "{}",
        crate::i18n::t2(
            "info.content_size",
            "bytes",
            &total_size.to_string(),
            "mb",
            &format!("{:.2}", total_size as f64 / 1_048_576.0)
        )
    );

    // 列出文件
    if !metadata.files.is_empty() {
        println!("\n{}", crate::i18n::t("common.file_list_title"));
        for (idx, file) in metadata.files.iter().enumerate() {
            println!(
                "{}",
                crate::i18n::t3(
                    "common.file_list_item",
                    "index",
                    &(idx + 1).to_string(),
                    "name",
                    &file.original_name,
                    "bytes",
                    &file.size.to_string()
                )
            );
        }
    }

    println!();
    Ok(())
}
