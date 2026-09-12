//! `veil shell` 子命令：提供针对单个容器的交互式命令循环。

use crate::error::Result;
use colored::Colorize;
use std::io::{self, Write};
use veil_core::workspace_ops::WorkspaceManager;

/// 验证密码后进入交互式文件管理会话。
///
/// 支持 `ls`、`info`、`add`、`rm`、`ex`、`help` 和 `exit`；每次操作都复用已解析的
/// 工作区管理器，并在读取信息时重新解密最新元数据。
///
/// # 错误
/// 容器解析、密码读取、标准输入输出操作或会话建立失败时返回错误。
pub fn run_workspace(container_name: &str, password: Option<String>) -> Result<()> {
    // shell 生命周期内复用同一 WorkspaceManager，避免每次命令重新解析链接。
    let resolved = super::resolve_container(container_name)?;
    let workspace_path = resolved.workspace_path;

    println!(
        "{}",
        crate::i18n::t1("shell.opening_named", "name", container_name).cyan()
    );
    let password_str =
        super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    use age::secrecy::ExposeSecret;
    let pwd = password_str.expose_secret();

    // 入口处先验证密码；失败时直接退出，不进入交互循环。
    let manager = WorkspaceManager::new(workspace_path.clone());
    manager.read_meta(pwd)?;

    println!("{}", crate::i18n::t("shell.opened_named").green());
    println!();
    println!(
        "{}",
        crate::i18n::t1("shell.title_named", "name", container_name).bright_cyan()
    );
    println!("{}", crate::i18n::t("shell.command_hint").bright_black());
    println!();

    // 每轮读取一行命令，空行直接忽略。
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

        // 命令分发保持轻量，具体文件操作仍委托给 WorkspaceManager。
        match cmd {
            "exit" | "quit" | "q" => {
                println!("{}", crate::i18n::t("shell.goodbye").bright_black());
                break;
            }
            "help" | "h" | "?" => {
                print_help();
            }
            "ls" | "list" => {
                list_files(&manager, pwd)?;
            }
            "info" => {
                match manager.read_meta(pwd) {
                    Ok(current_metadata) => {
                        if let Err(e) = show_info(container_name, &current_metadata) {
                            println!(
                                "{}",
                                crate::i18n::t1("shell.info_failed", "error", &e.to_string()).red()
                            );
                        }
                    }
                    Err(e) => {
                        println!(
                            "{}",
                            crate::i18n::t1("shell.metadata_failed", "error", &e.to_string()).red()
                        );
                    }
                }
            }
            "add" => {
                if parts.len() < 2 {
                    println!("{}", crate::i18n::t("shell.usage_add").yellow());
                    continue;
                }
                let file_path = parts[1];
                match manager.add_file(std::path::Path::new(file_path), pwd) {
                    Ok(encrypted_name) => {
                        println!(
                            "{}",
                            crate::i18n::t2(
                                "shell.added",
                                "file",
                                file_path,
                                "encrypted",
                                &encrypted_name
                            )
                            .green()
                        );
                    }
                    Err(e) => {
                        println!(
                            "{}",
                            crate::i18n::t1("shell.add_failed", "error", &e.to_string()).red()
                        );
                    }
                }
            }
            "rm" | "delete" => {
                if parts.len() < 2 {
                    println!("{}", crate::i18n::t("shell.usage_rm").yellow());
                    continue;
                }
                let file_name = parts[1];
                match manager.remove_file(file_name, pwd) {
                    Ok(()) => {
                        println!(
                            "{}",
                            crate::i18n::t1("shell.deleted", "path", file_name).green()
                        );
                    }
                    Err(e) => {
                        println!(
                            "{}",
                            crate::i18n::t1("shell.delete_failed", "error", &e.to_string()).red()
                        );
                    }
                }
            }
            "ex" | "extract" => {
                if parts.len() < 3 {
                    println!("{}", crate::i18n::t("shell.usage_ex").yellow());
                    continue;
                }
                let file_name = parts[1];
                let output_path = parts[2];
                match manager.extract_file(file_name, std::path::Path::new(output_path), pwd) {
                    Ok(()) => {
                        println!(
                            "{}",
                            crate::i18n::t1("shell.exported", "path", output_path).green()
                        );
                    }
                    Err(e) => {
                        println!(
                            "{}",
                            crate::i18n::t1("shell.export_failed", "error", &e.to_string()).red()
                        );
                    }
                }
            }
            _ => {
                println!("{}", crate::i18n::t1("shell.unknown", "cmd", cmd).yellow());
            }
        }
    }

    Ok(())
}

/// 输出交互式 shell 支持的命令。
fn print_help() {
    // 帮助内容与 match 分支保持一一对应，便于用户只记短命令。
    println!("{}", crate::i18n::t("shell.help_title").bright_cyan());
    println!(
        "  {}  - {}",
        "ls".bright_white(),
        crate::i18n::t("shell.help_ls")
    );
    println!(
        "  {}  - {}",
        "info".bright_white(),
        crate::i18n::t("shell.help_info")
    );
    println!(
        "  {}  - {}",
        "add <file>".bright_white(),
        crate::i18n::t("shell.help_add")
    );
    println!(
        "  {}  - {}",
        "rm <file>".bright_white(),
        crate::i18n::t("shell.help_rm")
    );
    println!(
        "  {}  - {}",
        "ex <file> <output>".bright_white(),
        crate::i18n::t("shell.help_ex")
    );
    println!(
        "  {}  - {}",
        "help".bright_white(),
        crate::i18n::t("shell.help_help")
    );
    println!(
        "  {}  - {}",
        "exit".bright_white(),
        crate::i18n::t("shell.help_exit")
    );
}

/// 读取并展示当前容器文件列表。
///
/// # 错误
/// 元数据读取或密码校验失败时返回错误。
fn list_files(manager: &WorkspaceManager, password: &str) -> Result<()> {
    let files = manager.list_files(password)?;

    // 空容器只显示空状态，不打印表头。
    if files.is_empty() {
        println!("{}", crate::i18n::t("common.empty").bright_black());
        return Ok(());
    }

    // 标题复用文件数量消息，再逐项输出原始名称和明文大小。
    println!(
        "{}",
        format!(
            "{}:",
            crate::i18n::t1("free.file_count", "count", &files.len().to_string()).trim()
        )
        .bright_cyan()
    );
    for (i, file) in files.iter().enumerate() {
        println!(
            "{}",
            crate::i18n::t3(
                "common.file_list_item",
                "index",
                &(i + 1).to_string(),
                "name",
                &file.original_name,
                "bytes",
                &file.size.to_string()
            )
        );
    }

    Ok(())
}

/// 使用已解密元数据展示容器身份和内容统计。
///
/// 当前实现不读取 `_container_name`，容器名称直接取自 `metadata`。
fn show_info(_container_name: &str, metadata: &veil_core::metadata::MetaData) -> Result<()> {
    // info 子命令直接复用已解密元数据，不进行第二次读取。
    println!("{}", crate::i18n::t("info.title").bright_cyan());
    println!(
        "{}",
        crate::i18n::t1("info.container_name", "name", &metadata.container_name)
    );
    println!(
        "{}",
        crate::i18n::t1(
            "info.workspace_type",
            "workspace_type",
            &metadata.workspace_type
        )
    );
    println!(
        "{}",
        crate::i18n::t1(
            "info.content_count",
            "count",
            &metadata.files.len().to_string()
        )
    );

    // 总大小为各文件条目明文大小的求和。
    let total_size: u64 = metadata.files.iter().map(|f| f.size).sum();
    println!(
        "{}",
        crate::i18n::t2(
            "free.total_size",
            "bytes",
            &total_size.to_string(),
            "mb",
            &format!("{:.2}", total_size as f64 / 1_048_576.0)
        )
    );

    Ok(())
}
