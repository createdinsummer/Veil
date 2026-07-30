use anyhow::Result;
use colored::Colorize;
use std::path::Path;
use veil_core::container::Container;
use indicatif::{ProgressBar, ProgressStyle};

/// 导出文件或目录到本地文件系统。
///
/// 从容器中解密并导出文件、目录或全部内容到本地文件系统。
/// 支持通配符模式（`*` 和 `**`）批量导出。
///
/// # 参数
/// - `container_path`: 容器文件路径
/// - `virtual_path`: 容器内的虚拟路径（`None` 则导出全部内容，支持通配符）
/// - `output`: 导出到的本地路径
/// - `password`: 容器密码（`None` 则交互式输入）
///
/// # 返回
/// - `Ok(())`: 文件/目录已解密并导出到本地
/// - `Err(anyhow::Error)`: 失败（密码错误、路径不存在或写入失败）
///
/// # 示例
/// ```bash
/// # 导出单个文件（位置参数）
/// veil ex photos.veil 2024/vacation.jpg ./vacation.jpg mypass
///
/// # 导出目录
/// veil ex photos.veil backup/ ./my-photos/ mypass
///
/// # 导出全部（使用通配符）
/// veil ex photos.veil "**/*" ./all-files/ mypass
///
/// # 导出匹配的文件
/// veil ex photos.veil "*.jpg" ./jpg-files/ mypass
/// veil ex photos.veil "2024/**/*.jpg" ./2024-jpg/ mypass
///
/// # 选项方式
/// veil ex photos.veil -i 2024/vacation.jpg -o ./vacation.jpg -p mypass
/// ```
pub fn run(
    container_path: &str,
    virtual_path: Option<&str>,
    output: &str,
    password: Option<String>,
) -> Result<()> {
    let password = super::prompt_password("请输入容器密码: ", password)?;

    println!("{}", "正在打开容器...".cyan());
    let container = Container::open(container_path, password)?;

    if let Some(path) = virtual_path {
        // 检查是否包含通配符
        if path.contains('*') {
            // 通配符模式匹配
            println!("{} 正在查找匹配的文件: {}", "→".blue(), path);
            let matched = container.find_files(path)?;

            if matched.is_empty() {
                println!("{} 未找到匹配的文件", "⚠".yellow());
                return Ok(());
            }

            println!("{} 找到 {} 个匹配的文件", "✓".green(), matched.len());

            // 创建进度条
            let pb = ProgressBar::new(matched.len() as u64);
            pb.set_style(ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} 文件 ({eta})")
                .unwrap()
                .progress_chars("#>-"));

            container.extract_matched(path, output)?;

            pb.set_position(matched.len() as u64);
            pb.finish_with_message(format!("{}", "完成".green()));

            println!("{} 导出完成", "✓".green());
        } else {
            // 精确路径匹配（原有逻辑）
            let output_path = Path::new(output);

            // 判断是文件还是目录
            if let Some(meta) = container.get_file(path) {
                // 是文件
                println!("{} 正在导出文件: {} -> {}", "→".blue(), path, output);

                // 创建进度条
                let pb = ProgressBar::new(meta.size);
                pb.set_style(ProgressStyle::default_bar()
                    .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                    .unwrap()
                    .progress_chars("#>-"));

                container.extract_file(path, output_path)?;

                pb.set_position(meta.size);
                pb.finish_with_message(format!("{}", "完成".green()));

                println!("{} 文件已导出: {}", "✓".green(), output);
            } else {
                // 尝试作为目录导出
                println!("{} 正在导出目录: {} -> {}", "→".blue(), path, output);

                // 统计目录下的文件数
                let files = veil_core::index::list_files(container.root());
                let dir_file_count = files.iter()
                    .filter(|(p, _)| p.starts_with(path))
                    .count();

                // 创建进度条
                let pb = ProgressBar::new(dir_file_count as u64);
                pb.set_style(ProgressStyle::default_bar()
                    .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} 文件 ({eta})")
                    .unwrap()
                    .progress_chars("#>-"));

                container.extract_dir(path, output_path)?;

                pb.set_position(dir_file_count as u64);
                pb.finish_with_message(format!("{}", "完成".green()));

                println!("{} 目录已导出完成", "✓".green());
            }
        }
    } else {
        anyhow::bail!("请指定要导出的路径（位置参数或 -i），使用 \"**/*\" 导出全部");
    }

    Ok(())
}
