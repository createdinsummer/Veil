use anyhow::Result;
use colored::Colorize;
use veil_core::container::Container;
use veil_core::index;

/// 树状显示容器内容。
///
/// 以树状结构显示容器内的目录和文件，并输出统计信息（文件数量、总大小）。
///
/// # 参数
/// - `container_path`: 容器文件路径
/// - `password`: 容器密码（`None` 则交互式输入）
///
/// # 返回
/// - `Ok(())`: 成功显示内容
/// - `Err(anyhow::Error)`: 失败（密码错误或读取失败）
///
/// # 示例
/// ```bash
/// # 位置参数方式
/// veil free photos.veil mypass
///
/// # 选项方式
/// veil free photos.veil -p mypass
/// ```
///
/// # 输出示例
/// ```text
/// 容器内容:
/// ├── 2024/
/// │   └── vacation.jpg
/// └── document.pdf
///
/// 统计信息:
///   文件数量: 2
///   总大小: 1234 字节 (0.00 MB)
/// ```
pub fn run(container_path: &str, password: Option<String>) -> Result<()> {
    let password = super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    let container = Container::open(container_path, password)?;

    println!("\n{}", crate::i18n::t("free.title").cyan().bold());
    print!("{}", container.tree_view());

    // 统计信息
    let files = index::list_files(container.root());
    let file_count = files.len();
    let total_size: u64 = files.iter().map(|(_, meta)| meta.size).sum();

    println!("\n{}", crate::i18n::t("free.stats_title").cyan());
    println!("{}", crate::i18n::t1("free.file_count", "count", &file_count.to_string()));
    println!("{}", crate::i18n::t2("free.total_size", "bytes", &total_size.to_string(), "mb", &format!("{:.2}", total_size as f64 / 1_048_576.0)));

    Ok(())
}
