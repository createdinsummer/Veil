use anyhow::Result;
use colored::Colorize;
use std::path::Path;
use veil_core::container::Container;
use indicatif::{ProgressBar, ProgressStyle};
use std::fs;
use std::io::Read;

/// 格式化文件大小
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

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

/// 包装 Reader 以实时更新进度条
struct ProgressReader<R> {
    inner: R,
    progress: ProgressBar,
}

impl<R> ProgressReader<R> {
    fn new(inner: R, progress: ProgressBar) -> Self {
        Self { inner, progress }
    }
}

impl<R: Read> Read for ProgressReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.progress.inc(n as u64);
        Ok(n)
    }
}

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
    let password = super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    println!("{}", crate::i18n::t("opening_container").cyan());
    let mut container = Container::open(container_path, password)?;

    let source_path = Path::new(source);

    if !source_path.exists() {
        anyhow::bail!("{}", crate::i18n::t1("add.source_not_found", "path", source));
    }

    if source_path.is_file() {
        // 添加单个文件
        let virtual_path = dest.unwrap_or_else(|| {
            source_path.file_name().unwrap().to_str().unwrap()
        });

        let file_size = source_path.metadata()?.len();

        println!("{}", crate::i18n::t2("add.adding_file", "source", source, "dest", virtual_path).blue());

        // 创建进度条
        let pb = ProgressBar::new(file_size);
        pb.set_style(ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
            .unwrap()
            .progress_chars("#>-"));

        // 使用流式加密（包装进度条更新）
        let file = fs::File::open(source_path)?;
        let reader = ProgressReader::new(file, pb.clone());

        let result = container.add_file_streaming(virtual_path, reader);

        // 确保进度条被清理
        match result {
            Ok(_) => {
                pb.finish_with_message(crate::i18n::t("done_label").green().to_string());
                println!("{}", crate::i18n::t1("add.file_added", "path", virtual_path).green());
                Ok(())
            }
            Err(e) => {
                pb.abandon_with_message(crate::i18n::t("failed_label").red().to_string());
                Err(e)
            }
        }?;
    } else if source_path.is_dir() {
        // 添加目录
        let dest_prefix = dest.unwrap_or("");

        println!("{}", crate::i18n::t1("add.scanning_dir", "path", source).blue());

        // 不在 CLI 层扫描，让 core 层扫描并通过回调返回总数
        let pb = ProgressBar::new(0); // 初始为 0，回调里更新
        pb.set_style(ProgressStyle::default_bar()
            .template(crate::i18n::t("pb.dir_template"))
            .unwrap()
            .progress_chars("#>-"));

        // add_dir 带进度回调
        let result = container.add_dir_with_progress(source_path, dest_prefix, |processed, total, file_path, file_size| {
            // 第一次回调时设置总数
            if pb.length() == Some(0) {
                pb.set_length(total as u64);
                println!("{}", crate::i18n::t1("add.found_files", "count", &total.to_string()).blue());
            }
            pb.set_position((processed + 1) as u64);
            let file_name = file_path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?");
            pb.set_message(format!("{} ({})", file_name, format_size(file_size)));
        });

        // 确保进度条被清理
        match result {
            Ok(_) => {
                if let Some(len) = pb.length() {
                    if len == 0 {
                        // 空目录情况
                        pb.finish_and_clear();
                        println!("{}", crate::i18n::t("add.empty_dir").yellow());
                    } else {
                        pb.set_position(len);
                        pb.finish_with_message(crate::i18n::t("done_label").green().to_string());
                        println!("{}", crate::i18n::t("add.dir_added").green());
                    }
                } else {
                    pb.finish_and_clear();
                    println!("{}", crate::i18n::t("add.empty_dir").yellow());
                }
                Ok(())
            }
            Err(e) => {
                pb.abandon_with_message(crate::i18n::t("failed_label").red().to_string());
                Err(e)
            }
        }?;
    } else {
        anyhow::bail!("{}", crate::i18n::t("add.unsupported_type"));
    }

    Ok(())
}
