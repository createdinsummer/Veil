//! `veil info` 子命令：展示容器身份与内容统计。

use crate::error::Result;
use colored::Colorize;

/// 显示 Veil 详细信息。
///
/// 输出 Veil 的元数据和内容统计。
///
/// # 参数
/// - `veil_name`: Veil 名称、`veil_id` 或 `.veil-link` 路径。
/// - `password`: Veil 密码（`None` 则交互式输入）
///
/// # 返回
/// - `Ok(())`: 成功显示信息
/// - `Err(CommandError)`: 失败
///
/// # 示例
/// ```bash
/// veil info photos
/// ```
pub fn run(veil_name: &str, password: Option<String>) -> Result<()> {
    // info 绑定已确认的 ID，展示名称以加密元数据中的值为准。
    let resolved = super::resolve_veil(veil_name)?;
    let manager = super::container_client(&resolved);
    let veil_dir = resolved.directory();

    let password_str = super::prompt_password(crate::i18n::t("prompt.veil_password"), password)?;

    use secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 读取元数据同时完成密码验证；失败时不会打印任何容器内容。
    let info = manager.info(password)?;
    let metadata = &info.metadata;

    crate::outln!("\n{}", crate::i18n::t("info.title").cyan().bold());
    crate::outln!(
        "{}",
        crate::i18n::t1("info.veil_name", "name", &metadata.veil_name)
    );
    crate::outln!(
        "{}",
        crate::i18n::t1("info.veil_id", "id", &metadata.veil_id)
    );
    crate::outln!(
        "{}",
        crate::i18n::t1("info.veil_dir", "path", &veil_dir.display().to_string())
    );
    crate::outln!(
        "{}",
        crate::i18n::t1("info.created_at", "time", &metadata.created_at)
    );

    // 文件数量来自已解密清单，总大小在内存中累加，不扫描磁盘密文。
    let file_count = metadata.files.len();
    let total_size: u64 = metadata.files.iter().map(|f| f.size).sum();
    let veil_size = info.disk_usage;

    crate::outln!("\n{}", crate::i18n::t("info.content_title").cyan());
    crate::outln!(
        "{}",
        crate::i18n::t1("info.content_count", "count", &file_count.to_string())
    );
    crate::outln!(
        "{}",
        crate::i18n::t2(
            "info.content_size",
            "bytes",
            &total_size.to_string(),
            "mb",
            &format!("{:.2}", total_size as f64 / 1_048_576.0)
        )
    );
    crate::outln!(
        "{}",
        crate::i18n::t2(
            "info.veil_size",
            "bytes",
            &veil_size.to_string(),
            "mb",
            &format!("{:.2}", veil_size as f64 / 1_048_576.0)
        )
    );

    if metadata.files.is_empty() {
        crate::outln!("\n{}", crate::i18n::t("common.empty").bright_black());
    } else {
        crate::outln!("\n{}", crate::i18n::t("info.mime_dist_title").cyan());
        for (mime, count) in &info.mime_distribution {
            crate::outln!(
                "{}",
                crate::i18n::t2("info.mime_item", "type", &mime, "count", &count.to_string())
            );
        }

        // 文件列表按虚拟路径排序，保证输出稳定且便于比较。
        let mut files = metadata.files.clone();
        files.sort_by(|left, right| left.original_name.cmp(&right.original_name));
        crate::outln!("\n{}", crate::i18n::t("common.file_list_title"));
        for (idx, file) in files.iter().enumerate() {
            crate::outln!(
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

    crate::outln!();
    Ok(())
}
