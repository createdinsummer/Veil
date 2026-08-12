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
    let password = super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    println!("{}", crate::i18n::t("opening_container").cyan());
    let mut container = Container::open(container_path, password)?;

    println!("{}", crate::i18n::t("shell.opened").green());
    println!("{}", crate::i18n::t("shell.hint"));
    println!();

    loop {
        // 显示提示符
        print!("{}", crate::i18n::t("shell.prompt").bright_blue().bold());
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
                println!("{}", crate::i18n::t("shell.exiting"));
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
                eprintln!("{}", crate::i18n::t1("shell.unknown_cmd", "cmd", cmd).red());
            }
        }
    }

    Ok(())
}

fn print_help() {
    println!("{}", crate::i18n::t("shell.help"));
}

fn cmd_add(container: &mut Container, args: &[&str]) -> Result<()> {
    if args.is_empty() {
        anyhow::bail!("{}", crate::i18n::t("shell.add_usage"));
    }

    let source = args[0];
    let dest = args.get(1).map(|s| *s);

    let source_path = std::path::Path::new(source);
    if !source_path.exists() {
        anyhow::bail!("{}", crate::i18n::t1("add.source_not_found", "path", source));
    }

    if source_path.is_file() {
        let virtual_path = dest.unwrap_or_else(|| {
            source_path.file_name().unwrap().to_str().unwrap()
        });

        println!("{}", crate::i18n::t2("add.adding_file", "source", source, "dest", virtual_path).blue());
        let file = std::fs::File::open(source_path)?;
        container.add_file_streaming(virtual_path, file)?;
        println!("{}", crate::i18n::t1("add.file_added", "path", virtual_path).green());
    } else if source_path.is_dir() {
        let dest_prefix = dest.unwrap_or("");
        println!("{}", crate::i18n::t2("add.adding_file", "source", source, "dest", dest_prefix).blue());
        container.add_dir(source_path, dest_prefix)?;
        println!("{}", crate::i18n::t("add.dir_added").green());
    }

    Ok(())
}

fn cmd_rm(container: &mut Container, args: &[&str]) -> Result<()> {
    if args.is_empty() {
        anyhow::bail!("{}", crate::i18n::t("shell.rm_usage"));
    }

    let path = args[0];
    println!("{}", crate::i18n::t1("rm.deleting", "path", path).blue());
    container.remove_file(path)?;
    println!("{}", crate::i18n::t1("rm.deleted", "path", path).green());

    Ok(())
}

fn cmd_mv(container: &mut Container, args: &[&str]) -> Result<()> {
    if args.len() < 2 {
        anyhow::bail!("{}", crate::i18n::t("shell.mv_usage"));
    }

    let from = args[0];
    let to = args[1];

    println!("{}", crate::i18n::t2("mv.moving", "from", from, "to", to).blue());
    container.rename_file(from, to)?;
    println!("{}", crate::i18n::t2("mv.moved", "from", from, "to", to).green());

    Ok(())
}

fn cmd_free(container: &Container) -> Result<()> {
    println!("\n{}", crate::i18n::t("free.title").cyan().bold());
    print!("{}", container.tree_view());

    let files = veil_core::index::list_files(container.root());
    let file_count = files.len();
    let total_size: u64 = files.iter().map(|(_, meta)| meta.size).sum();

    println!("\n{}", crate::i18n::t("free.stats_title").cyan());
    println!("{}", crate::i18n::t1("free.file_count", "count", &file_count.to_string()));
    println!("{}", crate::i18n::t2("free.total_size", "bytes", &total_size.to_string(), "mb", &format!("{:.2}", total_size as f64 / 1_048_576.0)));
    println!();

    Ok(())
}

fn cmd_info(container: &Container, container_path: &str) -> Result<()> {
    println!("\n{}", crate::i18n::t("info.title").cyan().bold());
    println!("{}", crate::i18n::t1("info.path", "path", container_path));

    let metadata = std::fs::metadata(container_path)?;
    println!("{}",
        crate::i18n::t2("info.container_size",
            "bytes", &metadata.len().to_string(),
            "mb", &format!("{:.2}", metadata.len() as f64 / 1_048_576.0)));

    let files = veil_core::index::list_files(container.root());
    let file_count = files.len();
    let total_size: u64 = files.iter().map(|(_, meta)| meta.size).sum();

    println!("\n{}", crate::i18n::t("info.content_title").cyan());
    println!("{}", crate::i18n::t1("info.content_count", "count", &file_count.to_string()));
    println!("{}", crate::i18n::t2("info.content_size",
        "bytes", &total_size.to_string(),
        "mb", &format!("{:.2}", total_size as f64 / 1_048_576.0)));

    let mut mime_stats: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (_, meta) in &files {
        let mime = meta.mime.as_deref().unwrap_or(crate::i18n::t("unknown"));
        *mime_stats.entry(mime.to_string()).or_insert(0) += 1;
    }

    if !mime_stats.is_empty() {
        println!("\n{}", crate::i18n::t("info.mime_dist_title").cyan());
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
        anyhow::bail!("{}", crate::i18n::t("shell.ex_usage"));
    }

    let input = args[0];
    let output = args[1];
    let output_path = std::path::Path::new(output);

    if let Some(_meta) = container.get_file(input) {
        println!("{}", crate::i18n::t2("ex.exporting_file", "source", input, "dest", output).blue());
        container.extract_file(input, output_path)?;
        println!("{}", crate::i18n::t("ex.file_exported").green());
    } else {
        println!("{}", crate::i18n::t2("ex.exporting_dir", "source", input, "dest", output).blue());
        container.extract_dir(input, output_path)?;
        println!("{}", crate::i18n::t("ex.dir_exported").green());
    }

    Ok(())
}
