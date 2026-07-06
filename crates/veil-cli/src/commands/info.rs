use anyhow::Result;
use colored::Colorize;
use veil_core::container::Container;
use veil_core::index;

/// 显示容器详细信息。
///
/// 输出容器的元数据（路径、大小）、内容统计（文件数量、内容总大小）和 MIME 类型分布。
///
/// # 参数
/// - `container_path`: 容器文件路径
/// - `password`: 容器密码（`None` 则交互式输入）
///
/// # 返回
/// - `Ok(())`: 成功显示信息
/// - `Err(anyhow::Error)`: 失败（密码错误或读取失败）
///
/// # 示例
/// ```bash
/// # 位置参数方式
/// veil info photos.veil mypass
///
/// # 选项方式
/// veil info photos.veil -p mypass
/// ```
///
/// # 输出示例
/// ```text
/// 容器信息:
///   路径: photos.veil
///   容器大小: 5678 字节 (0.01 MB)
///
/// 内容统计:
///   文件数量: 4
///   内容总大小: 1234 字节 (0.00 MB)
///
/// 文件类型分布:
///   image/jpeg                     3
///   application/pdf                1
/// ```
pub fn run(container_path: &str, password: Option<String>) -> Result<()> {
    let password = super::prompt_password("请输入容器密码: ", password)?;

    let container = Container::open(container_path, password)?;

    println!("\n{}", "容器信息:".cyan().bold());
    println!("  路径: {}", container_path);

    // 文件大小
    let metadata = std::fs::metadata(container_path)?;
    println!("  容器大小: {} 字节 ({:.2} MB)",
             metadata.len(),
             metadata.len() as f64 / 1_048_576.0);

    // 统计文件
    let files = index::list_files(container.root());
    let file_count = files.len();
    let total_size: u64 = files.iter().map(|(_, meta)| meta.size).sum();

    println!("\n{}", "内容统计:".cyan());
    println!("  文件数量: {}", file_count);
    println!("  内容总大小: {} 字节 ({:.2} MB)", total_size, total_size as f64 / 1_048_576.0);

    // MIME 类型统计
    let mut mime_stats: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (_, meta) in &files {
        let mime = meta.mime.as_deref().unwrap_or("未知");
        *mime_stats.entry(mime.to_string()).or_insert(0) += 1;
    }

    if !mime_stats.is_empty() {
        println!("\n{}", "文件类型分布:".cyan());
        let mut types: Vec<_> = mime_stats.iter().collect();
        types.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        for (mime, count) in types {
            println!("  {:<30} {}", mime, count);
        }
    }

    Ok(())
}
