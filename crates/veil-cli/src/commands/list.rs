//! `veil list` 子命令：列出工作区容器的文件清单。
//! 所属模块：CLI命令适配。核心run展示FileEntry，format_size/format_time负责可读格式；依赖服务元数据和本地时区，不读正文。
//!
//! 输出每个文件的名称、版本数、明文大小、加密时间与总大小；只解密元数据，
//! 不打开任何文件密文，因此速度快且不触碰内容。

use crate::error::Result;
use colored::Colorize;

/// 读取元数据并显示按路径排序的文件清单、版本数及总大小。
/// 入参：veil_name（&str）为容器选择器；password（`Option<String>`）为显式密码，None回退环境/TTY。
/// 返回值：完成显示后Ok(())，无文件输出空状态。
/// 异常：定位/密码/数据库读取错误返回CommandError；极端总大小u64求和溢出可能panic或按构建溢出行为处理。
/// 注意事项：标题名称来自定位缓存，条目来自解密库；version为历史链长度，不是UUID。统计不读取密文正文。
pub fn run(veil_name: &str, password: Option<String>) -> Result<()> {
    // list 只读取容器元数据，不需要打开每个文件 blob。
    let resolved = super::resolve_veil(veil_name)?;
    let manager = super::container_client(&resolved);
    let display_name = resolved.name();

    let password_str = super::prompt_password(crate::i18n::t("prompt.veil_password"), password)?;

    use secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 读取清单成功即表示密码正确，随后即可安全展示文件信息。
    let mut files = manager.list_files(password)?;
    // 按明文路径排序，保证同一容器多次执行的输出稳定。
    files.sort_by(|left, right| left.original_name.cmp(&right.original_name));

    // 标题：容器名 + 文件数量。
    crate::outln!(
        "\n{}",
        crate::i18n::t2(
            "list.veil_title",
            "name",
            display_name,
            "count",
            &files.len().to_string()
        )
        .cyan()
        .bold()
    );

    // 空容器保留标题和空状态，不打印总大小表格。
    if files.is_empty() {
        crate::outln!("\n{}", crate::i18n::t("common.empty").yellow());
        return Ok(());
    }

    crate::outln!();

    // 文件条目保存明文大小，可直接求和给用户展示。
    let total_size: u64 = files.iter().map(|f| f.size).sum();

    // 逐条输出：序号 + 明文名称；下一行输出版本数、明文大小与加密时间。
    for (idx, file) in files.iter().enumerate() {
        crate::outln!(
            "  {} {}",
            format!("{}.", idx + 1).bright_black(),
            file.original_name.bright_white()
        );
        // 版本数是版本历史链的长度，代表该文件内容继承的代数。
        crate::outln!(
            "     版本: {}  大小: {}  加密时间: {}",
            file.version_history.len(),
            format_size(file.size),
            format_time(&file.encrypted_at)
        );
    }

    crate::outln!();
    crate::outln!(
        "{}",
        crate::i18n::t1("list.total_size", "size", &format_size(total_size)).bright_black()
    );
    crate::outln!();

    Ok(())
}

/// 将字节数换算成便于终端阅读的容量文本。
/// 入参：bytes（u64）为文件或包逻辑字节数。
/// 返回值：String：小于1024用整数B，其余按1024次幂显示KB/MB/GB并保留两位小数。
/// 异常：无Result返回。
/// 注意事项：标签使用KB但计算为二进制1024；超过GB仍用GB，f64转换在极大整数时可能舍入。
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    // 先匹配达到的最大单位阈值，再以1024次幂换算；不改变原始统计值。
    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// 把元数据时间转换为用户本地时区显示。
/// 入参：rfc3339（&str）为带时区的时间文本。
/// 返回值：解析成功返回本地YYYY-MM-DD HH:MM:SS，失败原文返回。
/// 异常：解析错误不传播，不返回Result。
/// 注意事项：本地时区取执行环境；丢弃亚秒和时区显示，不改变数据库时间。
fn format_time(rfc3339: &str) -> String {
    use chrono::{DateTime, Local};

    if let Ok(dt) = DateTime::parse_from_rfc3339(rfc3339) {
        // 解析成功后转本地时区展示，用户看到的是自己的墙钟时间。
        let local: DateTime<Local> = dt.into();
        local.format("%Y-%m-%d %H:%M:%S").to_string()
    } else {
        // 解析失败（异常时间格式）时原样展示，不掩盖数据。
        rfc3339.to_string()
    }
}
