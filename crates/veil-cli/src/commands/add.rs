use anyhow::Result;
use colored::Colorize;
use std::path::Path;
use veil_core::container::Container;
use indicatif::{ProgressBar, ProgressStyle};
use std::fs;

/// 添加文件或目录到容器。
///
/// 将本地文件系统的文件或目录加密后添加到容器中。自动识别源是文件还是目录：
/// - 如果是文件，调用 [`Container::add_file`]
/// - 如果是目录，调用 [`Container::add_dir`]
///
/// # 参数
/// - `container_path`: 容器文件路径
/// - `source`: 源文件或目录路径（本地文件系统）
/// - `dest`: 容器内的目标路径（`None` 则使用源文件名）
/// - `password`: 容器密码（`None` 则交互式输入）
///
/// # 返回
/// - `Ok(())`: 文件/目录已加密并添加到容器
/// - `Err(anyhow::Error)`: 失败（源不存在、密码错误或加密失败）
///
/// # 示例
/// ```bash
/// # 添加文件（位置参数）
/// veil add photos.veil vacation.jpg 2024/vacation.jpg mypass
///
/// # 添加目录（自动识别）
/// veil add photos.veil ~/Pictures backup/ mypass
///
/// # 使用默认路径
/// veil add photos.veil document.pdf mypass
/// # → 容器内路径: document.pdf
///
/// # 选项方式
/// veil add photos.veil -i photo.jpg -o 2024/photo.jpg -p mypass
/// ```
pub fn run(container_path: &str, source: &str, dest: Option<&str>, password: Option<String>) -> Result<()> {
    let password = super::prompt_password("请输入容器密码: ", password)?;

    println!("{}", "正在打开容器...".cyan());
    let mut container = Container::open(container_path, password)?;

    let source_path = Path::new(source);

    if !source_path.exists() {
        anyhow::bail!("源文件或目录不存在: {}", source);
    }

    if source_path.is_file() {
        // 添加单个文件
        let virtual_path = dest.unwrap_or_else(|| {
            source_path.file_name().unwrap().to_str().unwrap()
        });

        let file_size = source_path.metadata()?.len();

        println!("{} 正在添加文件: {} -> {}", "→".blue(), source, virtual_path);

        // 创建进度条
        let pb = ProgressBar::new(file_size);
        pb.set_style(ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
            .unwrap()
            .progress_chars("#>-"));

        let content = std::fs::read(source_path)?;
        pb.set_position(file_size);

        container.add_file(virtual_path, &content)?;

        pb.finish_with_message(format!("{}", "完成".green()));

        println!("{} 文件已添加: {}", "✓".green(), virtual_path);
    } else if source_path.is_dir() {
        // 添加目录
        let dest_prefix = dest.unwrap_or("");

        println!("{} 正在扫描目录: {}", "→".blue(), source);

        // 收集所有文件
        let mut files = Vec::new();
        for entry in walkdir::WalkDir::new(source_path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            files.push(entry.path().to_path_buf());
        }

        let file_count = files.len();
        println!("{} 找到 {} 个文件，开始添加...", "→".blue(), file_count);

        // 创建进度条
        let pb = ProgressBar::new(file_count as u64);
        pb.set_style(ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} 文件 ({eta})")
            .unwrap()
            .progress_chars("#>-"));

        // 逐个添加文件并更新进度条
        for file_path in files {
            let relative_path = file_path.strip_prefix(source_path).unwrap();
            let virtual_path = if dest_prefix.is_empty() {
                relative_path.to_str().unwrap().to_string()
            } else {
                format!("{}/{}", dest_prefix.trim_end_matches('/'), relative_path.to_str().unwrap())
            };

            let content = fs::read(&file_path)?;
            container.add_file(&virtual_path, &content)?;

            pb.inc(1);
        }

        pb.finish_with_message(format!("{}", "完成".green()));

        println!("{} 目录已添加完成", "✓".green());
    } else {
        anyhow::bail!("不支持的文件类型");
    }

    Ok(())
}
