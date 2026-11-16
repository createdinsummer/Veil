//! `veil info` 子命令：展示容器身份与内容统计。
//! 所属模块：CLI命令适配。核心run读取ContainerInfo并展示；依赖适配器、密码与本地化，目标是呈现身份、逻辑大小与文件分布。
//!
//! 输出容器名、稳定 veil_id、veil_dir 目录、创建时间、文件数量与内容总大小、
//! 目录普通文件逻辑长度之和、MIME 类型分布和按虚拟路径排序的文件清单。
//! 只读取并解密元数据，不打开文件密文。

use crate::error::Result;
use colored::Colorize;

/// 读取元数据并显示身份、大小、MIME分布与文件清单。
/// 入参：veil_name（&str）为容器选择器；password（`Option<String>`）为显式密码，None回退环境/TTY。
/// 返回值：完成显示后Ok(())，无文件输出空状态。
/// 异常：定位/密码/数据库读取错误返回CommandError；极端总大小u64求和溢出可能panic或按构建溢出行为处理。
/// 注意事项：目录字节为普通文件逻辑长度之和，不是实际分配块；MIME按扩展名猜测，内容本体不做认证。
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

    // 名称、稳定ID和创建时间来自解密元数据；目录另取已解析的容器句柄。
    crate::outln!("\n{}", crate::i18n::t("info.title").cyan().bold());
    crate::outln!(
        "{}",
        crate::i18n::t1("info.veil_name", "name", &metadata.veil_name)
    );
    crate::outln!(
        "{}",
        crate::i18n::t1("info.veil_id", "id", &metadata.veil_id)
    );
    // veil_dir 是真正存放加密数据的目录，路径取自已解析的容器句柄。
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
    // 磁盘占用由服务层统计（含 veil.db 与全部密文），与明文总大小通常不同。
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
        // 空容器只显示空状态，不输出 MIME 分布与文件清单。
        crate::outln!("\n{}", crate::i18n::t("common.empty").bright_black());
    } else {
        crate::outln!("\n{}", crate::i18n::t("info.mime_dist_title").cyan());
        // MIME 分布来自服务层预聚合的映射：类型 → 文件数量。
        for (mime, count) in &info.mime_distribution {
            crate::outln!(
                "{}",
                crate::i18n::t2("info.mime_item", "type", &mime, "count", &count.to_string())
            );
        }

        // 文件列表按虚拟路径排序，保证输出稳定且便于比较。
        // 先克隆再排序，避免改动服务层返回的原清单顺序。
        let mut files = metadata.files.clone();
        files.sort_by(|left, right| left.original_name.cmp(&right.original_name));
        crate::outln!("\n{}", crate::i18n::t("common.file_list_title"));
        // 逐条输出序号、明文路径与字节大小。
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
