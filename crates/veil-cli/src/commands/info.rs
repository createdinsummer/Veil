//! `veil info` 子命令：展示容器身份与内容统计。

use crate::error::Result;
use colored::Colorize;
use std::collections::BTreeMap;
use std::path::Path;
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
/// - `Err(CommandError)`: 失败
///
/// # 示例
/// ```bash
/// veil info photos
/// ```
pub fn run(container_name: &str, password: Option<String>) -> Result<()> {
    // info 只需要工作区路径，展示名称以加密元数据中的值为准。
    let resolved = super::resolve_container(container_name)?;
    let workspace_path = resolved.workspace_path;

    let password_str =
        super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 读取元数据同时完成密码验证；失败时不会打印任何容器内容。
    let manager = WorkspaceManager::new(workspace_path.clone());
    let metadata = manager.read_meta(password)?;

    println!("\n{}", crate::i18n::t("info.title").cyan().bold());
    println!(
        "{}",
        crate::i18n::t1("info.container_name", "name", &metadata.container_name)
    );
    println!(
        "{}",
        crate::i18n::t1("info.veil_id", "id", &metadata.veil_id)
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

    // 文件数量来自已解密清单，总大小在内存中累加，不扫描磁盘密文。
    let file_count = metadata.files.len();
    let total_size: u64 = metadata.files.iter().map(|f| f.size).sum();
    let container_size = directory_size(&workspace_path)?;

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
    println!(
        "{}",
        crate::i18n::t2(
            "info.container_size",
            "bytes",
            &container_size.to_string(),
            "mb",
            &format!("{:.2}", container_size as f64 / 1_048_576.0)
        )
    );

    if metadata.files.is_empty() {
        println!("\n{}", crate::i18n::t("common.empty").bright_black());
    } else {
        println!("\n{}", crate::i18n::t("info.mime_dist_title").cyan());
        let mut distribution = BTreeMap::new();
        for file in &metadata.files {
            let mime = veil_core::mime::guess_mime(&file.original_name)
                .unwrap_or_else(|| "application/octet-stream".to_string());
            *distribution.entry(mime).or_insert(0usize) += 1;
        }
        for (mime, count) in distribution {
            println!(
                "{}",
                crate::i18n::t2("info.mime_item", "type", &mime, "count", &count.to_string())
            );
        }

        // 文件列表按虚拟路径排序，保证输出稳定且便于比较。
        let mut files = metadata.files.clone();
        files.sort_by(|left, right| left.original_name.cmp(&right.original_name));
        println!("\n{}", crate::i18n::t("common.file_list_title"));
        for (idx, file) in files.iter().enumerate() {
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

/// 递归统计工作区目录中实际占用的文件字节数。
fn directory_size(path: &Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            total = total.saturating_add(directory_size(&entry.path())?);
        } else if file_type.is_file() {
            total = total.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(total)
}
