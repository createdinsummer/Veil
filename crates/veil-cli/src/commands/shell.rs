use anyhow::Result;
use colored::Colorize;
use std::io::{self, Write};
use veil_core::container::Container;

/// 交互式 shell 模式。
///
/// 打开容器后进入交互式命令行，私钥保持在内存中，
/// 可以连续执行多个命令而无需重复解密私钥。
///
/// # 参数
/// - `container_path`: 容器文件路径
/// - `password`: 容器密码（`None` 则交互式输入）
///
/// # 返回
/// - `Ok(())`: shell 正常退出
/// - `Err(anyhow::Error)`: 失败（密码错误或打开容器失败）
///
/// # 示例
/// ```bash
/// $ veil shell vault.veil
/// 请输入密码: ****
/// 容器已打开 ✓
///
/// veil> add file1.txt
/// veil> add file2.txt dest.txt
/// veil> free
/// veil> exit
/// ```
///
/// # 支持的命令
/// - `add <source> [dest]` - 添加文件
/// - `rm <path>` - 删除文件
/// - `mv <from> <to>` - 移动文件
/// - `free` - 显示内容
/// - `info` - 显示信息
/// - `ex <input> <output>` - 导出文件
/// - `help` - 显示帮助
/// - `exit` / `quit` - 退出 shell
pub fn run(container_path: &str, password: Option<String>) -> Result<()> {
    let password = super::prompt_password("请输入容器密码: ", password)?;

    println!("{}", "正在打开容器...".cyan());
    let mut container = Container::open(container_path, password)?;

    println!("{} 容器已打开，进入交互模式", "✓".green());
    println!("输入 'help' 查看可用命令，'exit' 退出");
    println!();

    loop {
        // 显示提示符
        print!("{}", "veil> ".bright_blue().bold());
        io::stdout().flush()?;

        // 读取用户输入
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            break;
        }

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        // 解析命令
        let parts: Vec<&str> = input.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        let cmd = parts[0];
        let args = &parts[1..];

        // 执行命令
        match cmd {
            "exit" | "quit" => {
                println!("正在退出...");
                break;
            }
            "help" => {
                print_help();
            }
            "add" => {
                if let Err(e) = cmd_add(&mut container, args) {
                    eprintln!("{} {}", "✗".red(), e);
                }
            }
            "rm" => {
                if let Err(e) = cmd_rm(&mut container, args) {
                    eprintln!("{} {}", "✗".red(), e);
                }
            }
            "mv" => {
                if let Err(e) = cmd_mv(&mut container, args) {
                    eprintln!("{} {}", "✗".red(), e);
                }
            }
            "free" | "ls" => {
                if let Err(e) = cmd_free(&container) {
                    eprintln!("{} {}", "✗".red(), e);
                }
            }
            "info" => {
                if let Err(e) = cmd_info(&container, container_path) {
                    eprintln!("{} {}", "✗".red(), e);
                }
            }
            "ex" | "export" => {
                if let Err(e) = cmd_export(&container, args) {
                    eprintln!("{} {}", "✗".red(), e);
                }
            }
            _ => {
                eprintln!("{} 未知命令: {}. 输入 'help' 查看可用命令", "✗".red(), cmd);
            }
        }
    }

    Ok(())
}

fn print_help() {
    println!("\n{}", "可用命令:".cyan().bold());
    println!();
    println!("  文件操作:");
    println!("    {} <source> [dest]      - 添加文件/目录到容器", "add".green());
    println!("                                source: 本地文件路径");
    println!("                                dest: 容器内路径（可选）");
    println!();
    println!("    {} <path>               - 删除容器内的文件", "rm".green());
    println!("                                path: 容器内路径");
    println!();
    println!("    {} <from> <to>          - 移动/重命名文件", "mv".green());
    println!("                                from: 源路径（容器内）");
    println!("                                to: 目标路径（容器内）");
    println!();
    println!("    {} <input> <output>      - 导出文件到本地", "ex".green());
    println!("    {}                        （别名: export）", "".clear());
    println!("                                input: 容器内路径");
    println!("                                output: 本地路径");
    println!();
    println!("  查看信息:");
    println!("    {}                       - 树状显示容器内容", "free".green());
    println!("    {}                         （别名: ls）", "".clear());
    println!();
    println!("    {}                       - 显示容器详细信息", "info".green());
    println!("                                （大小、文件数、类型分布）");
    println!();
    println!("  其他:");
    println!("    {}                       - 显示此帮助信息", "help".green());
    println!();
    println!("    {}                       - 退出 shell 模式", "exit".green());
    println!("    {}                       - 退出 shell 模式（别名）", "quit".green());
    println!();
    println!("  {}:", "示例".yellow().bold());
    println!("    veil> add photo.jpg photos/vacation.jpg");
    println!("    veil> free");
    println!("    veil> mv old.txt archive/old.txt");
    println!("    veil> ex archive/old.txt ./backup.txt");
    println!("    veil> rm archive/old.txt");
    println!("    veil> exit");
    println!();
}

fn cmd_add(container: &mut Container, args: &[&str]) -> Result<()> {
    if args.is_empty() {
        anyhow::bail!("用法: add <source> [dest]");
    }

    let source = args[0];
    let dest = args.get(1).map(|s| *s);

    let source_path = std::path::Path::new(source);
    if !source_path.exists() {
        anyhow::bail!("源文件不存在: {}", source);
    }

    if source_path.is_file() {
        let virtual_path = dest.unwrap_or_else(|| {
            source_path.file_name().unwrap().to_str().unwrap()
        });

        println!("{} 正在添加: {} -> {}", "→".blue(), source, virtual_path);
        let content = std::fs::read(source_path)?;
        container.add_file(virtual_path, &content)?;
        println!("{} 文件已添加: {}", "✓".green(), virtual_path);
    } else if source_path.is_dir() {
        let dest_prefix = dest.unwrap_or("");
        println!("{} 正在添加目录: {} -> {}", "→".blue(), source, dest_prefix);
        container.add_dir(source_path, dest_prefix)?;
        println!("{} 目录已添加", "✓".green());
    }

    Ok(())
}

fn cmd_rm(container: &mut Container, args: &[&str]) -> Result<()> {
    if args.is_empty() {
        anyhow::bail!("用法: rm <path>");
    }

    let path = args[0];
    println!("{} 正在删除: {}", "→".blue(), path);
    container.remove_file(path)?;
    println!("{} 已删除: {}", "✓".green(), path);

    Ok(())
}

fn cmd_mv(container: &mut Container, args: &[&str]) -> Result<()> {
    if args.len() < 2 {
        anyhow::bail!("用法: mv <from> <to>");
    }

    let from = args[0];
    let to = args[1];

    println!("{} 正在移动: {} -> {}", "→".blue(), from, to);
    container.rename_file(from, to)?;
    println!("{} 已移动", "✓".green());

    Ok(())
}

fn cmd_free(container: &Container) -> Result<()> {
    println!("\n{}", "容器内容:".cyan().bold());
    print!("{}", container.tree_view());

    let files = veil_core::index::list_files(container.root());
    let file_count = files.len();
    let total_size: u64 = files.iter().map(|(_, meta)| meta.size).sum();

    println!("\n{}", "统计信息:".cyan());
    println!("  文件数量: {}", file_count);
    println!("  总大小: {} 字节 ({:.2} MB)", total_size, total_size as f64 / 1_048_576.0);
    println!();

    Ok(())
}

fn cmd_info(container: &Container, container_path: &str) -> Result<()> {
    println!("\n{}", "容器信息:".cyan().bold());
    println!("  路径: {}", container_path);

    let metadata = std::fs::metadata(container_path)?;
    println!("  容器大小: {} 字节 ({:.2} MB)",
             metadata.len(),
             metadata.len() as f64 / 1_048_576.0);

    let files = veil_core::index::list_files(container.root());
    let file_count = files.len();
    let total_size: u64 = files.iter().map(|(_, meta)| meta.size).sum();

    println!("\n{}", "内容统计:".cyan());
    println!("  文件数量: {}", file_count);
    println!("  内容总大小: {} 字节 ({:.2} MB)", total_size, total_size as f64 / 1_048_576.0);

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

    println!();
    Ok(())
}

fn cmd_export(container: &Container, args: &[&str]) -> Result<()> {
    if args.len() < 2 {
        anyhow::bail!("用法: ex <input> <output>");
    }

    let input = args[0];
    let output = args[1];
    let output_path = std::path::Path::new(output);

    if let Some(_meta) = container.get_file(input) {
        println!("{} 正在导出: {} -> {}", "→".blue(), input, output);
        container.extract_file(input, output_path)?;
        println!("{} 文件已导出", "✓".green());
    } else {
        println!("{} 正在导出目录: {} -> {}", "→".blue(), input, output);
        container.extract_dir(input, output_path)?;
        println!("{} 目录已导出", "✓".green());
    }

    Ok(())
}
