use anyhow::Result;
use colored::Colorize;
use veil_core::config::GlobalConfig;
use veil_core::workspace_ops::WorkspaceManager;
use std::io::{self, Write};

/// 交互式 shell
pub fn run_workspace(
    container_name: &str,
    password: Option<String>,
) -> Result<()> {
    // 加载配置
    let config = GlobalConfig::load()?;

    // 获取容器工作区路径
    let workspace_path = config.get_container_workspace_path(container_name)?;

    // 验证密码
    println!("{}", format!("打开容器 '{}'...", container_name).cyan());
    let password_str = super::prompt_password("请输入容器密码: ", password)?;

    use age::secrecy::ExposeSecret;
    let pwd = password_str.expose_secret();

    // 验证密码并读取元数据
    let manager = WorkspaceManager::new(workspace_path.clone());
    let metadata = manager.read_meta(pwd)?;

    println!("{}", "✓ 容器已打开".green());
    println!();
    println!("{}", format!("Veil Shell - 容器: {}", container_name).bright_cyan());
    println!("{}", "输入命令 (ls/add/rm/ex/info/help/exit):".bright_black());
    println!();

    loop {
        print!("{}", "veil> ".bright_green());
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        let parts: Vec<&str> = input.split_whitespace().collect();
        let cmd = parts[0];

        match cmd {
            "exit" | "quit" | "q" => {
                println!("{}", "再见！".bright_black());
                break;
            }
            "help" | "h" | "?" => {
                print_help();
            }
            "ls" | "list" => {
                list_files(&manager, pwd)?;
            }
            "info" => {
                // 重新读取最新的元数据
                match manager.read_meta(pwd) {
                    Ok(current_metadata) => {
                        if let Err(e) = show_info(container_name, &current_metadata) {
                            println!("{}", format!("❌ 显示信息失败: {}", e).red());
                        }
                    }
                    Err(e) => {
                        println!("{}", format!("❌ 读取元数据失败: {}", e).red());
                    }
                }
            }
            "add" => {
                if parts.len() < 2 {
                    println!("{}", "用法: add <文件路径>".yellow());
                    continue;
                }
                let file_path = parts[1];
                match manager.add_file(std::path::Path::new(file_path), pwd) {
                    Ok(encrypted_name) => {
                        println!("{}", format!("✓ 已添加: {} -> {}", file_path, encrypted_name).green());
                    }
                    Err(e) => {
                        println!("{}", format!("❌ 添加失败: {}", e).red());
                    }
                }
            }
            "rm" | "delete" => {
                if parts.len() < 2 {
                    println!("{}", "用法: rm <文件名>".yellow());
                    continue;
                }
                let file_name = parts[1];
                match manager.remove_file(file_name, pwd) {
                    Ok(()) => {
                        println!("{}", format!("✓ 已删除: {}", file_name).green());
                    }
                    Err(e) => {
                        println!("{}", format!("❌ 删除失败: {}", e).red());
                    }
                }
            }
            "ex" | "extract" => {
                if parts.len() < 3 {
                    println!("{}", "用法: ex <文件名> <输出路径>".yellow());
                    continue;
                }
                let file_name = parts[1];
                let output_path = parts[2];
                match manager.extract_file(file_name, std::path::Path::new(output_path), pwd) {
                    Ok(()) => {
                        println!("{}", format!("✓ 已导出: {}", output_path).green());
                    }
                    Err(e) => {
                        println!("{}", format!("❌ 导出失败: {}", e).red());
                    }
                }
            }
            _ => {
                println!("{}", format!("未知命令: {}. 输入 'help' 查看帮助", cmd).yellow());
            }
        }
    }

    Ok(())
}

fn print_help() {
    println!("{}", "可用命令:".bright_cyan());
    println!("  {}  - 列出所有文件", "ls".bright_white());
    println!("  {}  - 显示容器信息", "info".bright_white());
    println!("  {}  - 添加文件到容器", "add <文件路径>".bright_white());
    println!("  {}  - 从容器删除文件", "rm <文件名>".bright_white());
    println!("  {}  - 导出文件", "ex <文件名> <输出路径>".bright_white());
    println!("  {}  - 显示帮助", "help".bright_white());
    println!("  {}  - 退出 shell", "exit".bright_white());
}

fn list_files(manager: &WorkspaceManager, password: &str) -> Result<()> {
    let files = manager.list_files(password)?;

    if files.is_empty() {
        println!("{}", "容器为空".bright_black());
        return Ok(());
    }

    println!("{}", format!("共 {} 个文件:", files.len()).bright_cyan());
    for (i, file) in files.iter().enumerate() {
        println!("  {}. {} ({} bytes)", i + 1, file.original_name, file.size);
    }

    Ok(())
}

fn show_info(container_name: &str, metadata: &veil_core::metadata::MetaData) -> Result<()> {
    println!("{}", "容器信息:".bright_cyan());
    println!("  容器名称: {}", metadata.container_name);
    println!("  工作区类型: {}", metadata.workspace_type);
    println!("  文件数量: {}", metadata.files.len());

    let total_size: u64 = metadata.files.iter().map(|f| f.size).sum();
    println!("  总大小: {} bytes", total_size);

    Ok(())
}
