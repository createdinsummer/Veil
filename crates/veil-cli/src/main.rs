use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

mod commands;
mod i18n;
mod output_encoding;

/// (默认中文，运行时根据 VEIL_LANG 覆写)
#[derive(Parser)]
#[command(name = "veil", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// (默认中文，运行时根据 VEIL_LANG 覆写)
    Init {
        #[arg(value_name = "容器文件")]
        container: Option<String>,
        #[arg(value_name = "密码")]
        password: Option<String>,
        #[arg(short, long, conflicts_with = "password", value_name = "密码")]
        password_opt: Option<String>,
    },

    /// (默认中文，运行时根据 VEIL_LANG 覆写)
    Add {
        #[arg(value_name = "容器文件")]
        container: Option<String>,
        #[arg(value_name = "源路径")]
        input_pos: Option<String>,
        #[arg(value_name = "目标路径")]
        output_pos: Option<String>,
        #[arg(value_name = "密码")]
        password_pos: Option<String>,
        #[arg(short, long, conflicts_with = "input_pos", value_name = "路径")]
        input: Option<String>,
        #[arg(short, long, conflicts_with = "output_pos", value_name = "路径")]
        output: Option<String>,
        #[arg(short, long, conflicts_with = "password_pos", value_name = "密码")]
        password: Option<String>,
    },

    /// (默认中文，运行时根据 VEIL_LANG 覆写)
    Rm {
        #[arg(value_name = "容器文件")]
        container: Option<String>,
        #[arg(value_name = "路径")]
        path: Option<String>,
        #[arg(value_name = "密码")]
        password_pos: Option<String>,
        #[arg(short, long, conflicts_with = "password_pos", value_name = "密码")]
        password: Option<String>,
    },

    /// (默认中文，运行时根据 VEIL_LANG 覆写)
    Mv {
        #[arg(value_name = "容器文件")]
        container: Option<String>,
        #[arg(value_name = "源路径")]
        from_pos: Option<String>,
        #[arg(value_name = "目标路径")]
        to_pos: Option<String>,
        #[arg(value_name = "密码")]
        password_pos: Option<String>,
        #[arg(short, long, conflicts_with = "from_pos", value_name = "路径")]
        input: Option<String>,
        #[arg(short, long, conflicts_with = "to_pos", value_name = "路径")]
        output: Option<String>,
        #[arg(short, long, conflicts_with = "password_pos", value_name = "密码")]
        password: Option<String>,
    },

    /// (默认中文，运行时根据 VEIL_LANG 覆写)
    Free {
        #[arg(value_name = "容器文件")]
        container: Option<String>,
        #[arg(value_name = "密码")]
        password_pos: Option<String>,
        #[arg(short, long, conflicts_with = "password_pos", value_name = "密码")]
        password: Option<String>,
    },

    /// (默认中文，运行时根据 VEIL_LANG 覆写)
    Ex {
        #[arg(value_name = "容器文件")]
        container: Option<String>,
        #[arg(value_name = "输入路径")]
        input_pos: Option<String>,
        #[arg(value_name = "输出路径")]
        output_pos: Option<String>,
        #[arg(value_name = "密码")]
        password_pos: Option<String>,
        #[arg(short, long, conflicts_with = "input_pos", value_name = "路径")]
        input: Option<String>,
        #[arg(short, long, conflicts_with = "output_pos", value_name = "路径")]
        output: Option<String>,
        #[arg(short, long, conflicts_with = "password_pos", value_name = "密码")]
        password: Option<String>,
    },

    /// (默认中文，运行时根据 VEIL_LANG 覆写)
    Info {
        #[arg(value_name = "容器文件")]
        container: Option<String>,
        #[arg(value_name = "密码")]
        password_pos: Option<String>,
        #[arg(short, long, conflicts_with = "password_pos", value_name = "密码")]
        password: Option<String>,
    },

    /// (默认中文，运行时根据 VEIL_LANG 覆写)
    Passwd {
        #[arg(value_name = "容器文件")]
        container: Option<String>,
        #[arg(value_name = "旧密码")]
        old_password_pos: Option<String>,
        #[arg(value_name = "新密码")]
        new_password_pos: Option<String>,
        #[arg(short, long, conflicts_with = "old_password_pos", value_name = "密码")]
        password: Option<String>,
        #[arg(short = 'n', long, conflicts_with = "new_password_pos", value_name = "密码")]
        new_password: Option<String>,
    },

    /// (默认中文，运行时根据 VEIL_LANG 覆写)
    Shell {
        #[arg(value_name = "容器文件")]
        container: Option<String>,
        #[arg(value_name = "密码")]
        password_pos: Option<String>,
        #[arg(short, long, conflicts_with = "password_pos", value_name = "密码")]
        password: Option<String>,
    },
}

/// 用当前语言覆写所有 clap 显示字符串（about、usage、help_template、arg value_name / help）
fn build_localized_command() -> clap::Command {
    let mut cmd = Cli::command()
        .about(i18n::clap_about())
        .long_about(i18n::clap_long_about())
        .help_template(i18n::t("clap.help_template"));

    let sub_template = i18n::t("clap.sub_help_template");
    let v_container = i18n::t("arg.container");
    let v_password = i18n::t("arg.password");
    let v_path = i18n::t("arg.path");
    let v_source = i18n::t("arg.source_path");
    let v_dest = i18n::t("arg.dest_path");
    let v_input = i18n::t("arg.input_path");
    let v_output = i18n::t("arg.output_path");
    let v_old_pw = i18n::t("arg.old_password");
    let v_new_pw = i18n::t("arg.new_password");

    // --- init ---
    cmd = cmd.mut_subcommand("init", |sub| {
        sub.about(i18n::t("cmd.init.about"))
            .override_usage(i18n::t("cmd.init.usage"))
            .help_template(sub_template)
            .mut_arg("container", |a| {
                a.value_name(v_container)
                    .help(i18n::t("help.init.container"))
            })
            .mut_arg("password", |a| {
                a.value_name(v_password)
                    .help(i18n::t("help.init.password"))
            })
            .mut_arg("password_opt", |a| {
                a.value_name(v_password)
                    .help(i18n::t("help.init.password_opt"))
            })
    });

    // --- add ---
    cmd = cmd.mut_subcommand("add", |sub| {
        sub.about(i18n::t("cmd.add.about"))
            .override_usage(i18n::t("cmd.add.usage"))
            .help_template(sub_template)
            .mut_arg("container", |a| a.value_name(v_container).help(i18n::t("help.container")))
            .mut_arg("input_pos", |a| a.value_name(v_input).help(i18n::t("help.add.input_pos")))
            .mut_arg("output_pos", |a| a.value_name(v_output).help(i18n::t("help.add.output_pos")))
            .mut_arg("password_pos", |a| a.value_name(v_password).help(i18n::t("help.password_pos")))
            .mut_arg("input", |a| a.value_name(v_path).help(i18n::t("help.add.input")))
            .mut_arg("output", |a| a.value_name(v_path).help(i18n::t("help.add.output")))
            .mut_arg("password", |a| a.value_name(v_password).help(i18n::t("help.password_opt")))
    });

    // --- rm ---
    cmd = cmd.mut_subcommand("rm", |sub| {
        sub.about(i18n::t("cmd.rm.about"))
            .override_usage(i18n::t("cmd.rm.usage"))
            .help_template(sub_template)
            .mut_arg("container", |a| a.value_name(v_container).help(i18n::t("help.container")))
            .mut_arg("path", |a| a.value_name(v_path).help(i18n::t("help.rm.path")))
            .mut_arg("password_pos", |a| a.value_name(v_password).help(i18n::t("help.password_pos")))
            .mut_arg("password", |a| a.value_name(v_password).help(i18n::t("help.password_opt")))
    });

    // --- mv ---
    cmd = cmd.mut_subcommand("mv", |sub| {
        sub.about(i18n::t("cmd.mv.about"))
            .override_usage(i18n::t("cmd.mv.usage"))
            .help_template(sub_template)
            .mut_arg("container", |a| a.value_name(v_container).help(i18n::t("help.container")))
            .mut_arg("from_pos", |a| a.value_name(v_source).help(i18n::t("help.mv.from_pos")))
            .mut_arg("to_pos", |a| a.value_name(v_dest).help(i18n::t("help.mv.to_pos")))
            .mut_arg("password_pos", |a| a.value_name(v_password).help(i18n::t("help.password_pos")))
            .mut_arg("input", |a| a.value_name(v_path).help(i18n::t("help.mv.input")))
            .mut_arg("output", |a| a.value_name(v_path).help(i18n::t("help.mv.output")))
            .mut_arg("password", |a| a.value_name(v_password).help(i18n::t("help.password_opt")))
    });

    // --- free ---
    cmd = cmd.mut_subcommand("free", |sub| {
        sub.about(i18n::t("cmd.free.about"))
            .override_usage(i18n::t("cmd.free.usage"))
            .help_template(sub_template)
            .mut_arg("container", |a| a.value_name(v_container).help(i18n::t("help.container")))
            .mut_arg("password_pos", |a| a.value_name(v_password).help(i18n::t("help.password_pos")))
            .mut_arg("password", |a| a.value_name(v_password).help(i18n::t("help.password_opt")))
    });

    // --- ex ---
    cmd = cmd.mut_subcommand("ex", |sub| {
        sub.about(i18n::t("cmd.ex.about"))
            .override_usage(i18n::t("cmd.ex.usage"))
            .help_template(sub_template)
            .mut_arg("container", |a| a.value_name(v_container).help(i18n::t("help.container")))
            .mut_arg("input_pos", |a| a.value_name(v_input).help(i18n::t("help.ex.input_pos")))
            .mut_arg("output_pos", |a| a.value_name(v_output).help(i18n::t("help.ex.output_pos")))
            .mut_arg("password_pos", |a| a.value_name(v_password).help(i18n::t("help.password_pos")))
            .mut_arg("input", |a| a.value_name(v_path).help(i18n::t("help.ex.input")))
            .mut_arg("output", |a| a.value_name(v_path).help(i18n::t("help.ex.output")))
            .mut_arg("password", |a| a.value_name(v_password).help(i18n::t("help.password_opt")))
    });

    // --- info ---
    cmd = cmd.mut_subcommand("info", |sub| {
        sub.about(i18n::t("cmd.info.about"))
            .override_usage(i18n::t("cmd.info.usage"))
            .help_template(sub_template)
            .mut_arg("container", |a| a.value_name(v_container).help(i18n::t("help.container")))
            .mut_arg("password_pos", |a| a.value_name(v_password).help(i18n::t("help.password_pos")))
            .mut_arg("password", |a| a.value_name(v_password).help(i18n::t("help.password_opt")))
    });

    // --- passwd ---
    cmd = cmd.mut_subcommand("passwd", |sub| {
        sub.about(i18n::t("cmd.passwd.about"))
            .override_usage(i18n::t("cmd.passwd.usage"))
            .help_template(sub_template)
            .mut_arg("container", |a| a.value_name(v_container).help(i18n::t("help.container")))
            .mut_arg("old_password_pos", |a| {
                a.value_name(v_old_pw)
                    .help(i18n::t("help.passwd.old_password_pos"))
            })
            .mut_arg("new_password_pos", |a| {
                a.value_name(v_new_pw)
                    .help(i18n::t("help.passwd.new_password_pos"))
            })
            .mut_arg("password", |a| {
                a.value_name(v_password)
                    .help(i18n::t("help.passwd.password"))
            })
            .mut_arg("new_password", |a| {
                a.value_name(v_password)
                    .help(i18n::t("help.passwd.new_password"))
            })
    });

    // --- shell ---
    cmd = cmd.mut_subcommand("shell", |sub| {
        sub.about(i18n::t("cmd.shell.about"))
            .override_usage(i18n::t("cmd.shell.usage"))
            .help_template(sub_template)
            .mut_arg("container", |a| a.value_name(v_container).help(i18n::t("help.container")))
            .mut_arg("password_pos", |a| a.value_name(v_password).help(i18n::t("help.password_pos")))
            .mut_arg("password", |a| a.value_name(v_password).help(i18n::t("help.password_opt")))
    });

    cmd
}

/// 查找命令用法
fn cmd_usage(cmd_name: &str) -> &str {
    match cmd_name {
        "init" => i18n::t("cmd.init.usage"),
        "add" => i18n::t("cmd.add.usage"),
        "rm" => i18n::t("cmd.rm.usage"),
        "mv" => i18n::t("cmd.mv.usage"),
        "free" => i18n::t("cmd.free.usage"),
        "ex" => i18n::t("cmd.ex.usage"),
        "info" => i18n::t("cmd.info.usage"),
        "passwd" => i18n::t("cmd.passwd.usage"),
        "shell" => i18n::t("cmd.shell.usage"),
        _ => "",
    }
}

/// 查找命令选项参数用法
fn cmd_usage_opt(cmd_name: &str) -> &str {
    match cmd_name {
        "init" => i18n::t("cmd.init.usage_opt"),
        "add" => i18n::t("cmd.add.usage_opt"),
        "rm" => i18n::t("cmd.rm.usage_opt"),
        "mv" => i18n::t("cmd.mv.usage_opt"),
        "free" => i18n::t("cmd.free.usage_opt"),
        "ex" => i18n::t("cmd.ex.usage_opt"),
        "info" => i18n::t("cmd.info.usage_opt"),
        "passwd" => i18n::t("cmd.passwd.usage_opt"),
        "shell" => i18n::t("cmd.shell.usage_opt"),
        _ => "",
    }
}


/// 打印错误 + 两种用法（位置参数 / 选项参数），然后退出
fn exit_with_help(error_key: &str, cmd_name: &str) -> ! {
    eprintln!("{}", i18n::t(error_key));
    eprintln!("\n{}:", i18n::t("label.usage"));
    eprintln!("  {}", cmd_usage(cmd_name));
    eprintln!("  {}", cmd_usage_opt(cmd_name));
    std::process::exit(1);
}

fn require_container(container: Option<String>, cmd_name: &str) -> String {
    container.unwrap_or_else(|| exit_with_help("error.require_container", cmd_name))
}

/// 显示版本和安全信息
fn show_version_and_security_info() {
    use colored::Colorize;

    const VERSION: &str = env!("CARGO_PKG_VERSION");

    // 检测构建模式
    #[cfg(debug_assertions)]
    let is_debug = true;
    #[cfg(not(debug_assertions))]
    let is_debug = false;

    // 检测版本类型
    let is_dev = VERSION.contains("dev") || VERSION.contains("alpha") || VERSION.contains("beta");

    println!("\n{} {}", "Veil".cyan().bold(), format!("v{}", VERSION).cyan());

    if is_dev {
        println!("{}", "⚠️  开发版本 - 仅供测试使用".yellow().bold());
    }

    if is_debug {
        println!();
        println!("{}", "⚠️  警告: Debug 构建模式".yellow().bold());
        println!("{}", "   密钥强度: scrypt N=2^12 (较弱)".yellow());
        println!("{}", "   破解速度: ~50 次/秒".yellow());
        println!();
        println!("{}", "💡 强烈建议使用 Release 模式:".bright_yellow());
        println!("   cargo build --release --package veil-cli");
        println!("{}", "   密钥强度提升 64 倍 (scrypt N=2^18)".green());
        println!();
    } else {
        println!("{}", "✅ Release 模式 - 高安全强度 (scrypt N=2^18)".green());
    }

    println!();
}

fn main() {
    // Windows: 启用 UTF-8 控制台模式（Windows 10+ 支持）
    #[cfg(target_os = "windows")]
    {
        unsafe {
            unsafe extern "system" {
                fn SetConsoleOutputCP(wCodePageID: u32) -> i32;
            }
            const CP_UTF8: u32 = 65001;
            SetConsoleOutputCP(CP_UTF8);
        }
    }

    i18n::init();

    // 显示版本和安全信息
    show_version_and_security_info();

    let cmd = build_localized_command();
    let matches = cmd.get_matches();
    let cli = Cli::from_arg_matches(&matches).expect("参数解析失败");

    let result = match cli.command {
        Commands::Init {
            container,
            password,
            password_opt,
        } => {
            let container = require_container(container, "init");
            let pwd = password.or(password_opt);
            commands::init::run(&container, pwd)
        }
        Commands::Add {
            container,
            input_pos,
            output_pos,
            password_pos,
            input,
            output,
            password,
        } => {
            let container = require_container(container, "add");
            let inp = input_pos.or(input);
            let out = output_pos.or(output);
            let pwd = password_pos.or(password);

            if let Some(inp) = inp {
                commands::add::run(&container, &inp, out.as_deref(), pwd)
            } else {
                exit_with_help("error.require_input_path", "add");
            }
        }
        Commands::Rm {
            container,
            path,
            password_pos,
            password,
        } => {
            let container = require_container(container, "rm");
            let pwd = password_pos.or(password);
            if let Some(path) = path {
                commands::rm::run(&container, &path, pwd)
            } else {
                exit_with_help("error.require_delete_path", "rm");
            }
        }
        Commands::Mv {
            container,
            from_pos,
            to_pos,
            password_pos,
            input,
            output,
            password,
        } => {
            let container = require_container(container, "mv");
            let from = from_pos.or(input);
            let to = to_pos.or(output);
            let pwd = password_pos.or(password);

            if let (Some(from), Some(to)) = (from, to) {
                commands::mv::run(&container, &from, &to, pwd)
            } else {
                exit_with_help("error.require_src_dst", "mv");
            }
        }
        Commands::Free {
            container,
            password_pos,
            password,
        } => {
            let container = require_container(container, "free");
            let pwd = password_pos.or(password);
            commands::free::run(&container, pwd)
        }
        Commands::Ex {
            container,
            input_pos,
            output_pos,
            password_pos,
            input,
            output,
            password,
        } => {
            let container = require_container(container, "ex");
            let inp = input_pos.or(input);
            let out = output_pos.or(output);
            let pwd = password_pos.or(password);

            if let Some(out) = out {
                commands::ex::run(&container, inp.as_deref(), &out, pwd)
            } else {
                exit_with_help("error.require_output_path", "ex");
            }
        }
        Commands::Info {
            container,
            password_pos,
            password,
        } => {
            let container = require_container(container, "info");
            let pwd = password_pos.or(password);
            commands::info::run(&container, pwd)
        }
        Commands::Passwd {
            container,
            old_password_pos,
            new_password_pos,
            password,
            new_password,
        } => {
            let container = require_container(container, "passwd");
            let old_pwd = old_password_pos.or(password);
            let new_pwd = new_password_pos.or(new_password);
            commands::passwd::run(&container, old_pwd, new_pwd)
        }
        Commands::Shell {
            container,
            password_pos,
            password,
        } => {
            let container = require_container(container, "shell");
            let pwd = password_pos.or(password);
            commands::shell::run(&container, pwd)
        }
    };

    if let Err(e) = result {
        eprintln!("{}", i18n::t1("error.prefix", "error", &e.to_string()));
        std::process::exit(1);
    }
}
