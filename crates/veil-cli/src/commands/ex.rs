use anyhow::Result;
use colored::Colorize;
use std::path::{Path, PathBuf};
use veil_core::container::Container;
use indicatif::{ProgressBar, ProgressStyle};

/// 批量导出文件并实时推进字节进度条。
fn export_files_with_progress(
    container: &Container,
    files: &[(String, u64)],
    pb: &ProgressBar,
    mut dest_for: impl FnMut(&str) -> PathBuf,
) -> Result<()> {
    let mut exported = 0u64;
    for (virtual_path, size) in files {
        let dest = dest_for(virtual_path);
        container.extract_file_with_progress(virtual_path, dest, |done, _| {
            pb.set_position(exported + done);
        })?;
        exported += *size;
    }
    Ok(())
}

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

            let files: Vec<(String, u64)> = matched
                .iter()
                .filter_map(|p| container.get_file(p).map(|meta| (p.clone(), meta.size)))
                .collect();
            let total_size: u64 = files.iter().map(|(_, size)| *size).sum();

            // 创建进度条
            let pb = ProgressBar::new(total_size);
            pb.set_style(ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                .unwrap()
                .progress_chars("#>-"));

            let result = export_files_with_progress(
                &container,
                &files,
                &pb,
                |virtual_path| PathBuf::from(output).join(virtual_path),
            );

            match result {
                Ok(()) => {
                    pb.finish_with_message(format!("{}", "完成".green()));
                    println!("{} 导出完成", "✓".green());
                }
                Err(e) => {
                    pb.abandon_with_message(format!("{}", "失败".red()));
                    return Err(e);
                }
            }

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

                let files = vec![(path.to_string(), meta.size)];
                let result = export_files_with_progress(
                    &container,
                    &files,
                    &pb,
                    |_| PathBuf::from(output),
                );

                match result {
                    Ok(()) => {
                        pb.finish_with_message(format!("{}", "完成".green()));
                        println!("{} 文件已导出: {}", "✓".green(), output);
                    }
                    Err(e) => {
                        pb.abandon_with_message(format!("{}", "失败".red()));
                        return Err(e);
                    }
                }

            } else {
                // 尝试作为目录导出
                println!("{} 正在导出目录: {} -> {}", "→".blue(), path, output);

                let prefix = format!("{}/", path.trim_end_matches('/'));
                let files: Vec<(String, u64)> = veil_core::index::list_files(container.root())
                    .into_iter()
                    .filter(|(p, _)| p.starts_with(&prefix))
                    .map(|(p, meta)| (p, meta.size))
                    .collect();

                if files.is_empty() {
                    println!("{} 目录为空，无文件导出", "→".yellow());
                    return Ok(());
                }

                let total_size: u64 = files.iter().map(|(_, size)| *size).sum();

                // 创建进度条
                let pb = ProgressBar::new(total_size);
                pb.set_style(ProgressStyle::default_bar()
                    .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                    .unwrap()
                    .progress_chars("#>-"));

                let result = export_files_with_progress(
                    &container,
                    &files,
                    &pb,
                    |virtual_path| {
                        let rel = virtual_path.strip_prefix(&prefix).unwrap_or(virtual_path);
                        PathBuf::from(output).join(rel)
                    },
                );

                match result {
                    Ok(()) => {
                        pb.finish_with_message(format!("{}", "完成".green()));
                        println!("{} 目录已导出完成", "✓".green());
                    }
                    Err(e) => {
                        pb.abandon_with_message(format!("{}", "失败".red()));
                        return Err(e);
                    }
                }

            }
        }
    } else {
        anyhow::bail!("请指定要导出的路径（位置参数或 -i），使用 \"**/*\" 导出全部");
    }

    Ok(())
}
