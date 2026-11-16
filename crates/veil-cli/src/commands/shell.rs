//! `veil shell` 子命令：提供针对单个 Veil 的交互式命令循环。
//! 所属模块：CLI命令适配。核心run为命令循环，print_help/list_files/show_info为展示辅助；依赖CLI适配器、密码输入和stdin，目标是交互操作便利。
//!
//! 会话内支持 `ls`/`list`、`info`、`add`、`rm`/`delete`、`ex`/`extract`、`help`
//! 和 `exit`/`quit`/`q`。本层持续持有密码SecretString，业务操作仍委托服务读取最新数据；
//! 底层可缓存连接或受保护密码副本，本层不保证所有密钥仅存在于一次命令调用内。

use super::CliContainer;
use crate::error::Result;
use colored::Colorize;
use std::io::{self, Write};

/// 验证一次密码后进入终端命令循环，复用已取得密码进行文件操作。
/// 入参：veil_name为容器选择器；password为可选显式密码，None回退环境/TTY。
/// 返回值：输入EOF或exit/quit/q后Ok(())。
/// 异常：初次定位/密码验证、stdin/stdout I/O及ls子操作失败返回CommandError；其他子命令多只显示错误并继续。
/// 注意事项：密码SecretString在整个循环存活；CliContainer复用最初句柄，但add/ex转调子命令会重新解析原selector。只split_whitespace，不支持引号转义或空格路径，不能视为系统shell。
pub fn run(veil_name: &str, password: Option<String>) -> Result<()> {
    // shell 生命周期内复用同一 VeilManager，避免每次命令重新解析链接。
    let resolved = super::resolve_veil(veil_name)?;
    let display_name = resolved.name().to_owned();
    let manager = super::container_client(&resolved);

    crate::outln!(
        "{}",
        crate::i18n::t1("shell.opening_named", "name", veil_name).cyan()
    );
    let password_str = super::prompt_password(crate::i18n::t("prompt.veil_password"), password)?;

    use secrecy::ExposeSecret;
    let pwd = password_str.expose_secret();

    // 入口处先验证密码；失败时直接退出，不进入交互循环。
    manager.read_meta(pwd)?;

    // 会话建立成功：输出欢迎标题与命令速览。
    crate::outln!("{}", crate::i18n::t("shell.opened_named").green());
    crate::outln!();
    crate::outln!(
        "{}",
        crate::i18n::t1("shell.title_named", "name", &display_name).bright_cyan()
    );
    crate::outln!("{}", crate::i18n::t("shell.command_hint").bright_black());
    crate::outln!();

    // 每轮读取一行命令，空行直接忽略。
    loop {
        // 输出提示符（不换行），提示用户当前处于 veil 会话。
        crate::out!("{}", "veil> ".bright_green());
        io::stdout().flush()?;

        let mut input = String::new();
        // 输入流关闭（EOF，如管道输入结束）时退出会话。
        if io::stdin().read_line(&mut input)? == 0 {
            break;
        }

        let input = input.trim();
        // 空行不产生任何动作，继续下一轮。
        if input.is_empty() {
            continue;
        }

        // 按空白切分为命令与参数；首个分量是命令名。
        let parts: Vec<&str> = input.split_whitespace().collect();
        let cmd = parts[0];

        // 命令分发保持轻量，具体文件操作仍委托给 VeilManager。
        match cmd {
            "exit" | "quit" | "q" => {
                // 三种退出写法等价：打印告别语后结束循环。
                crate::outln!("{}", crate::i18n::t("shell.goodbye").bright_black());
                break;
            }
            "help" | "h" | "?" => {
                // 打印会话支持的命令清单。
                print_help();
            }
            "ls" | "list" => {
                // 复用会话内的管理器列出文件；失败向上返回并结束会话。
                list_files(&manager, pwd)?;
            }
            "info" => match manager.read_meta(pwd) {
                // 读取成功则用最新元数据展示身份与统计。
                Ok(current_metadata) => {
                    if let Err(e) = show_info(&display_name, &current_metadata) {
                        // info 失败不影响会话，只显示错误继续循环。
                        crate::outln!(
                            "{}",
                            crate::i18n::t1("shell.info_failed", "error", &e.report()).red()
                        );
                    }
                }
                Err(e) => {
                    // 元数据读取失败（如锁冲突）同样只提示，不退出会话。
                    let error = crate::error::CommandError::from(e);
                    crate::outln!(
                        "{}",
                        crate::i18n::t1("shell.metadata_failed", "error", &error.report()).red()
                    );
                }
            },
            "add" => {
                // 参数不足只提示用法，不退出会话。
                if parts.len() < 2 {
                    crate::outln!("{}", crate::i18n::t("shell.usage_add").yellow());
                    continue;
                }
                let file_path = parts[1];
                // 复用 add 子命令；密码直接用已验证的会话密码，冲突策略固定为逐个询问。
                if let Err(error) = super::add::run(
                    veil_name,
                    file_path,
                    None,
                    Some(pwd.to_owned()),
                    super::add::ConflictPolicy::Ask,
                ) {
                    crate::outln!(
                        "{}",
                        crate::i18n::t1("shell.add_failed", "error", &error.report()).red()
                    );
                }
            }
            "rm" | "delete" => {
                if parts.len() < 2 {
                    crate::outln!("{}", crate::i18n::t("shell.usage_rm").yellow());
                    continue;
                }
                let file_name = parts[1];
                // 直接调用会话管理器的删除；失败只提示，会话继续。
                match manager.remove_path(file_name, pwd) {
                    Ok(_) => {
                        crate::outln!(
                            "{}",
                            crate::i18n::t1("shell.deleted", "path", file_name).green()
                        );
                    }
                    Err(e) => {
                        let error = crate::error::CommandError::from(e);
                        crate::outln!(
                            "{}",
                            crate::i18n::t1("shell.delete_failed", "error", &error.report()).red()
                        );
                    }
                }
            }
            "ex" | "extract" => {
                // 导出需要容器内路径与本地输出路径两个参数。
                if parts.len() < 3 {
                    crate::outln!("{}", crate::i18n::t("shell.usage_ex").yellow());
                    continue;
                }
                let file_name = parts[1];
                let output_path = parts[2];
                // 复用 ex 子命令，密码与冲突策略与 add 相同。
                match super::ex::run(
                    veil_name,
                    Some(file_name),
                    output_path,
                    Some(pwd.to_owned()),
                    super::add::ConflictPolicy::Ask,
                ) {
                    Ok(()) => {}
                    Err(e) => {
                        crate::outln!(
                            "{}",
                            crate::i18n::t1("shell.export_failed", "error", &e.report()).red()
                        );
                    }
                }
            }
            _ => {
                // 未知命令黄色提示，不终止会话。
                crate::outln!("{}", crate::i18n::t1("shell.unknown", "cmd", cmd).yellow());
            }
        }
    }

    Ok(())
}

/// 输出本交互循环支持的命令速览。
/// 入参：无。
/// 返回值：无，打印本地化帮助。
/// 异常：无Result返回。
/// 注意事项：只是本程序命令表，不执行系统shell，也不检查用户输入。
fn print_help() {
    // 帮助内容与 match 分支保持一一对应，便于用户只记短命令。
    crate::outln!("{}", crate::i18n::t("shell.help_title").bright_cyan());
    crate::outln!(
        "  {}  - {}",
        "ls".bright_white(),
        crate::i18n::t("shell.help_ls")
    );
    crate::outln!(
        "  {}  - {}",
        "info".bright_white(),
        crate::i18n::t("shell.help_info")
    );
    crate::outln!(
        "  {}  - {}",
        "add <file>".bright_white(),
        crate::i18n::t("shell.help_add")
    );
    crate::outln!(
        "  {}  - {}",
        "rm <file>".bright_white(),
        crate::i18n::t("shell.help_rm")
    );
    crate::outln!(
        "  {}  - {}",
        "ex <file> <output>".bright_white(),
        crate::i18n::t("shell.help_ex")
    );
    crate::outln!(
        "  {}  - {}",
        "help".bright_white(),
        crate::i18n::t("shell.help_help")
    );
    crate::outln!(
        "  {}  - {}",
        "exit".bright_white(),
        crate::i18n::t("shell.help_exit")
    );
}

/// 为交互循环读取并打印当前文件条目。
/// 入参：manager（&CliContainer）为已定位适配器；password（&str）为本轮借用的会话密码。
/// 返回值：读取显示完成后Ok(())，无文件打印空状态。
/// 异常：密码/元数据读取错误传播，run的ls分支会因此退出循环。
/// 注意事项：按底层返回顺序遍历，不在此排序；每项显示序号、内部路径和明文大小。
fn list_files(manager: &CliContainer, password: &str) -> Result<()> {
    let files = manager.list_files(password)?;

    // 空容器只显示空状态，不打印表头。
    if files.is_empty() {
        crate::outln!("{}", crate::i18n::t("common.empty").bright_black());
        return Ok(());
    }

    // 标题复用文件数量消息，再逐项输出原始名称和明文大小。
    crate::outln!(
        "{}",
        format!(
            "{}:",
            crate::i18n::t1("free.file_count", "count", &files.len().to_string()).trim()
        )
        .bright_cyan()
    );
    // 逐条输出文件清单：序号、明文名称与字节大小（复用 i18n 模板）。
    for (i, file) in files.iter().enumerate() {
        crate::outln!(
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

/// 用已解密元数据打印容器名、文件数和明文总量。
/// 入参：_veil_name（&str）为未使用兼容参数；metadata（&MetaData）为外层取得的敏感快照。
/// 返回值：固定返回Ok(())，不再开库。
/// 异常：无显式业务错误分支；u64总大小溢出受构建规则影响。
/// 注意事项：名称取metadata，不取定位缓存；不计算磁盘分配量，也不认证密文正文。
fn show_info(_veil_name: &str, metadata: &veil_core::metadata::MetaData) -> Result<()> {
    // info 子命令直接复用已解密元数据，不进行第二次读取。
    crate::outln!("{}", crate::i18n::t("info.title").bright_cyan());
    crate::outln!(
        "{}",
        crate::i18n::t1("info.veil_name", "name", &metadata.veil_name)
    );
    crate::outln!(
        "{}",
        crate::i18n::t1(
            "info.content_count",
            "count",
            &metadata.files.len().to_string()
        )
    );

    // 总大小为各文件条目明文大小的求和。
    let total_size: u64 = metadata.files.iter().map(|f| f.size).sum();
    crate::outln!(
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
