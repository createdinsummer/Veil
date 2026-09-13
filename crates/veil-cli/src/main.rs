//! Veil 命令行程序入口。
//!
//! 本模块负责初始化本地化、构建 clap 命令、解析参数并把各子命令分派到
//! [`commands`] 模块。命令说明先定义中文兜底文本，再根据 `VEIL_LANG` 在运行时
//! 覆写为实际显示语言。

use clap::{Arg, ArgAction, CommandFactory, FromArgMatches, Parser, Subcommand};

mod commands;
mod error;
mod hints;
mod i18n;
mod output_encoding;

/// 命令行根参数。
///
/// 当前版本要求用户显式选择一个子命令；具体参数由 [`Commands`] 定义。
#[derive(Parser)]
#[command(name = "veil", version, disable_help_subcommand = true)]
struct Cli {
    /// 本次调用要执行的 Veil 子命令。
    #[command(subcommand)]
    command: Commands,
}

/// Veil 支持的子命令及其参数。
///
/// 变体上的文字是 clap 的中文兜底说明，程序启动后会由 [`build_localized_command`]
/// 按当前语言替换。
#[derive(Subcommand)]
enum Commands {
    /// 创建新的工作区容器和链接文件。
    Init {
        // 新容器名称或链接输出目标。
        #[arg(value_name = "容器名称")]
        container: Option<String>,
        // 位置参数形式的密码；实际输入仍由命令层统一处理。
        #[arg(value_name = "密码")]
        password: Option<String>,
        // 选项形式的密码，与位置参数密码互斥。
        #[arg(
            short = 'p',
            long = "password",
            conflicts_with = "password",
            value_name = "密码"
        )]
        password_opt: Option<String>,
        // 自定义 `.veil-link` 输出路径。
        #[arg(long, value_name = "链接文件", help = "自定义 .veil-link 输出路径")]
        link: Option<String>,
        // 已注册的命名工作区查找键，不是文件系统路径。
        #[arg(short = 'w', long, value_name = "工作区", help = "使用命名工作区")]
        workspace: Option<String>,
        // 指定工作区根路径并自动登记到当前容器，不会新增命名工作区。
        #[arg(long, value_name = "路径", help = "使用指定工作区路径")]
        workspace_path: Option<String>,
        // 让 --workspace-path 指定的目录由当前容器独占。
        #[arg(long, help = "工作区由该容器独占")]
        dedicated: bool,
        // 强制把工作区放在链接文件所在卷。
        #[arg(long, help = "强制将工作区放在链接文件所在的卷")]
        portable: bool,
    },

    /// 加密并添加本地文件。
    Add {
        // 位置参数形式的容器名称、ID 或链接目标。
        #[arg(value_name = "容器名称")]
        container: Option<String>,
        // 位置参数形式的源文件路径。
        #[arg(value_name = "源路径")]
        input_pos: Option<String>,
        // 位置参数形式的容器内目标路径；当前由命令层保留兼容接口。
        #[arg(value_name = "目标路径")]
        output_pos: Option<String>,
        // 位置参数形式的密码。
        #[arg(value_name = "密码")]
        password_pos: Option<String>,
        // 选项形式的源文件路径，与 `input_pos` 互斥。
        #[arg(short, long, conflicts_with = "input_pos", value_name = "路径")]
        input: Option<String>,
        // 选项形式的容器内目标路径，与 `output_pos` 互斥。
        #[arg(short, long, conflicts_with = "output_pos", value_name = "路径")]
        output: Option<String>,
        // 选项形式的密码，与位置参数密码互斥。
        #[arg(short, long, conflicts_with = "password_pos", value_name = "密码")]
        password: Option<String>,
    },

    /// 从容器删除文件。
    Rm {
        // 位置参数形式的容器名称、ID 或链接目标。
        #[arg(value_name = "容器名称")]
        container: Option<String>,
        // 要删除的容器内路径。
        #[arg(value_name = "路径")]
        path: Option<String>,
        // 位置参数形式的密码。
        #[arg(value_name = "密码")]
        password_pos: Option<String>,
        // 选项形式的密码。
        #[arg(short, long, conflicts_with = "password_pos", value_name = "密码")]
        password: Option<String>,
    },

    /// 移动或重命名容器内的文件。
    Mv {
        // 位置参数形式的容器名称、ID 或链接目标。
        #[arg(value_name = "容器名称")]
        container: Option<String>,
        // 位置参数形式的源路径。
        #[arg(value_name = "源路径")]
        from_pos: Option<String>,
        // 位置参数形式的目标路径。
        #[arg(value_name = "目标路径")]
        to_pos: Option<String>,
        // 位置参数形式的密码。
        #[arg(value_name = "密码")]
        password_pos: Option<String>,
        // 选项形式的源路径。
        #[arg(short, long, conflicts_with = "from_pos", value_name = "路径")]
        input: Option<String>,
        // 选项形式的目标路径。
        #[arg(short, long, conflicts_with = "to_pos", value_name = "路径")]
        output: Option<String>,
        // 选项形式的密码。
        #[arg(short, long, conflicts_with = "password_pos", value_name = "密码")]
        password: Option<String>,
    },

    /// 以树状形式查看容器内容与统计。
    Free {
        // 位置参数形式的容器名称、ID 或链接目标。
        #[arg(value_name = "容器名称")]
        container: Option<String>,
        // 位置参数形式的密码。
        #[arg(value_name = "密码")]
        password_pos: Option<String>,
        // 选项形式的密码。
        #[arg(short, long, conflicts_with = "password_pos", value_name = "密码")]
        password: Option<String>,
    },

    /// 从容器解密导出文件。
    Ex {
        // 位置参数形式的容器名称、ID 或链接目标。
        #[arg(value_name = "容器名称")]
        container: Option<String>,
        // 位置参数形式的容器内输入路径。
        #[arg(value_name = "输入路径")]
        input_pos: Option<String>,
        // 位置参数形式的本地输出路径。
        #[arg(value_name = "输出路径")]
        output_pos: Option<String>,
        // 位置参数形式的密码。
        #[arg(value_name = "密码")]
        password_pos: Option<String>,
        // 选项形式的容器内输入路径。
        #[arg(short, long, conflicts_with = "input_pos", value_name = "路径")]
        input: Option<String>,
        // 选项形式的本地输出路径。
        #[arg(short, long, conflicts_with = "output_pos", value_name = "路径")]
        output: Option<String>,
        // 选项形式的密码。
        #[arg(short, long, conflicts_with = "password_pos", value_name = "密码")]
        password: Option<String>,
    },

    /// 显示容器身份和内容统计。
    Info {
        // 位置参数形式的容器名称、ID 或链接目标。
        #[arg(value_name = "容器名称")]
        container: Option<String>,
        // 位置参数形式的密码。
        #[arg(value_name = "密码")]
        password_pos: Option<String>,
        // 选项形式的密码。
        #[arg(short, long, conflicts_with = "password_pos", value_name = "密码")]
        password: Option<String>,
    },

    /// 列出容器中的文件
    List {
        // 位置参数形式的容器名称、ID 或链接目标。
        #[arg(value_name = "容器名称")]
        container: Option<String>,
        // 位置参数形式的密码。
        #[arg(value_name = "密码")]
        password_pos: Option<String>,
        // 选项形式的密码。
        #[arg(short, long, conflicts_with = "password_pos", value_name = "密码")]
        password: Option<String>,
    },

    /// 判断容器内文件或目录是否存在。
    Exists {
        // 位置参数形式的容器名称、ID 或链接目标。
        #[arg(value_name = "容器名称")]
        container: Option<String>,
        // 要检查的容器内相对路径。
        #[arg(value_name = "路径")]
        path: Option<String>,
        // 位置参数形式的密码。
        #[arg(value_name = "密码")]
        password_pos: Option<String>,
        // 选项形式的密码。
        #[arg(short, long, conflicts_with = "password_pos", value_name = "密码")]
        password: Option<String>,
    },

    /// 修改容器密码。
    Passwd {
        // 位置参数形式的容器名称、ID 或链接目标。
        #[arg(value_name = "容器名称")]
        container: Option<String>,
        // 位置参数形式的旧密码。
        #[arg(value_name = "旧密码")]
        old_password_pos: Option<String>,
        // 位置参数形式的新密码。
        #[arg(value_name = "新密码")]
        new_password_pos: Option<String>,
        // 选项形式的旧密码。
        #[arg(short, long, conflicts_with = "old_password_pos", value_name = "密码")]
        password: Option<String>,
        // 选项形式的新密码。
        #[arg(
            short = 'n',
            long,
            conflicts_with = "new_password_pos",
            value_name = "密码"
        )]
        new_password: Option<String>,
        /// 轮换数据主密钥并逐个重新加密全部文件。
        #[arg(short = 'f', long = "full", visible_alias = "reencrypt")]
        full: bool,
    },

    /// 打开交互式容器命令会话。
    Shell {
        // 位置参数形式的容器名称、ID 或链接目标。
        #[arg(value_name = "容器名称")]
        container: Option<String>,
        // 位置参数形式的密码。
        #[arg(value_name = "密码")]
        password_pos: Option<String>,
        // 选项形式的密码。
        #[arg(short, long, conflicts_with = "password_pos", value_name = "密码")]
        password: Option<String>,
    },

    /// 打包工作区到 .veil 文件
    Pack {
        // 位置参数形式的容器名称、ID 或链接目标。
        #[arg(value_name = "容器名称")]
        container: Option<String>,
        // 可选的自定义打包文件路径。
        #[arg(short, long, value_name = "输出文件")]
        output: Option<String>,
        // 位置参数形式的密码。
        #[arg(value_name = "密码")]
        password_pos: Option<String>,
        // 选项形式的密码。
        #[arg(short, long, conflicts_with = "password_pos", value_name = "密码")]
        password: Option<String>,
    },

    /// 解包 .veil 文件到工作区
    Unpack {
        // 要解包的 `.veil` 文件路径。
        #[arg(value_name = "容器名称")]
        file: Option<String>,
        // 解包后使用的容器展示名称。
        #[arg(short = 'n', long, value_name = "容器名称")]
        name: Option<String>,
        // 接收工作区的命名工作区。
        #[arg(short = 'w', long, value_name = "工作区")]
        workspace: Option<String>,
        // 解包后生成的 `.veil-link` 路径。
        #[arg(long, value_name = "链接文件", help = "解包后生成 .veil-link 的路径")]
        link: Option<String>,
        // 位置参数形式的密码。
        #[arg(value_name = "密码")]
        password_pos: Option<String>,
        // 选项形式的密码。
        #[arg(short, long, conflicts_with = "password_pos", value_name = "密码")]
        password: Option<String>,
    },

    /// 配置管理
    Config {
        // 设置提示级别，接受 `full`、`brief` 或 `off`。
        #[arg(long, value_name = "级别", help = "设置提示级别 (full/brief/off)")]
        hints: Option<String>,
        // 显式执行 `show` 操作。
        #[arg(value_name = "操作", value_parser = ["show"], help = "显示当前配置 (show)")]
        action: Option<String>,
        // 兼容旧调用方式的隐藏显示开关。
        #[arg(long, hide = true)]
        show: bool,
    },

    /// 重建 .veil-link
    Link {
        // 已注册容器名称、ID、现有链接路径或工作区路径。
        #[arg(value_name = "容器名、ID、链接或工作区路径")]
        target: String,
        // 新链接文件的输出路径。
        #[arg(short, long, value_name = "链接文件")]
        output: Option<String>,
    },

    /// 显示命令或文件类型帮助
    Help {
        // 可选的帮助主题，例如 `files`。
        #[arg(value_name = "主题")]
        topic: Option<String>,
        // 输出全部子命令帮助。
        #[arg(long, help = "显示所有命令帮助")]
        all: bool,
    },
}

/// 构建使用当前语言显示的 clap 命令树。
///
/// clap derive 先生成中文兜底命令，本函数再覆写 about、usage、帮助模板以及各参数的
/// 显示名称和说明。子命令的 `--help` 标志也在此统一注入，以保证本地化后的输出一致。
fn build_localized_command() -> clap::Command {
    let mut cmd = Cli::command()
        .disable_help_flag(true)
        .disable_version_flag(true)
        .arg(localized_help_arg())
        .arg(localized_version_arg())
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
                a.value_name(v_password).help(i18n::t("help.init.password"))
            })
            .mut_arg("password_opt", |a| {
                a.value_name(v_password)
                    .help(i18n::t("help.init.password_opt"))
            })
            .mut_arg("link", |a| {
                a.value_name(i18n::t("arg.link_file"))
                    .help(i18n::t("help.init.link"))
            })
            .mut_arg("workspace", |a| {
                a.value_name(i18n::t("arg.workspace"))
                    .help(i18n::t("help.init.workspace"))
            })
            .mut_arg("workspace_path", |a| {
                a.value_name(i18n::t("arg.path"))
                    .help(i18n::t("help.init.workspace_path"))
            })
            .mut_arg("dedicated", |a| a.help(i18n::t("help.init.dedicated")))
            .mut_arg("portable", |a| a.help(i18n::t("help.init.portable")))
    });

    // --- add ---
    cmd = cmd.mut_subcommand("add", |sub| {
        sub.about(i18n::t("cmd.add.about"))
            .override_usage(i18n::t("cmd.add.usage"))
            .help_template(sub_template)
            .mut_arg("container", |a| {
                a.value_name(v_container).help(i18n::t("help.container"))
            })
            .mut_arg("input_pos", |a| {
                a.value_name(v_input).help(i18n::t("help.add.input_pos"))
            })
            .mut_arg("output_pos", |a| {
                a.value_name(v_output).help(i18n::t("help.add.output_pos"))
            })
            .mut_arg("password_pos", |a| {
                a.value_name(v_password).help(i18n::t("help.password_pos"))
            })
            .mut_arg("input", |a| {
                a.value_name(v_path).help(i18n::t("help.add.input"))
            })
            .mut_arg("output", |a| {
                a.value_name(v_path).help(i18n::t("help.add.output"))
            })
            .mut_arg("password", |a| {
                a.value_name(v_password).help(i18n::t("help.password_opt"))
            })
    });

    // --- rm ---
    cmd = cmd.mut_subcommand("rm", |sub| {
        sub.about(i18n::t("cmd.rm.about"))
            .override_usage(i18n::t("cmd.rm.usage"))
            .help_template(sub_template)
            .mut_arg("container", |a| {
                a.value_name(v_container).help(i18n::t("help.container"))
            })
            .mut_arg("path", |a| {
                a.value_name(v_path).help(i18n::t("help.rm.path"))
            })
            .mut_arg("password_pos", |a| {
                a.value_name(v_password).help(i18n::t("help.password_pos"))
            })
            .mut_arg("password", |a| {
                a.value_name(v_password).help(i18n::t("help.password_opt"))
            })
    });

    // --- mv ---
    cmd = cmd.mut_subcommand("mv", |sub| {
        sub.about(i18n::t("cmd.mv.about"))
            .override_usage(i18n::t("cmd.mv.usage"))
            .help_template(sub_template)
            .mut_arg("container", |a| {
                a.value_name(v_container).help(i18n::t("help.container"))
            })
            .mut_arg("from_pos", |a| {
                a.value_name(v_source).help(i18n::t("help.mv.from_pos"))
            })
            .mut_arg("to_pos", |a| {
                a.value_name(v_dest).help(i18n::t("help.mv.to_pos"))
            })
            .mut_arg("password_pos", |a| {
                a.value_name(v_password).help(i18n::t("help.password_pos"))
            })
            .mut_arg("input", |a| {
                a.value_name(v_path).help(i18n::t("help.mv.input"))
            })
            .mut_arg("output", |a| {
                a.value_name(v_path).help(i18n::t("help.mv.output"))
            })
            .mut_arg("password", |a| {
                a.value_name(v_password).help(i18n::t("help.password_opt"))
            })
    });

    // --- free ---
    cmd = cmd.mut_subcommand("free", |sub| {
        sub.about(i18n::t("cmd.free.about"))
            .override_usage(i18n::t("cmd.free.usage"))
            .help_template(sub_template)
            .mut_arg("container", |a| {
                a.value_name(v_container).help(i18n::t("help.container"))
            })
            .mut_arg("password_pos", |a| {
                a.value_name(v_password).help(i18n::t("help.password_pos"))
            })
            .mut_arg("password", |a| {
                a.value_name(v_password).help(i18n::t("help.password_opt"))
            })
    });

    // --- ex ---
    cmd = cmd.mut_subcommand("ex", |sub| {
        sub.about(i18n::t("cmd.ex.about"))
            .override_usage(i18n::t("cmd.ex.usage"))
            .help_template(sub_template)
            .mut_arg("container", |a| {
                a.value_name(v_container).help(i18n::t("help.container"))
            })
            .mut_arg("input_pos", |a| {
                a.value_name(v_input).help(i18n::t("help.ex.input_pos"))
            })
            .mut_arg("output_pos", |a| {
                a.value_name(v_output).help(i18n::t("help.ex.output_pos"))
            })
            .mut_arg("password_pos", |a| {
                a.value_name(v_password).help(i18n::t("help.password_pos"))
            })
            .mut_arg("input", |a| {
                a.value_name(v_path).help(i18n::t("help.ex.input"))
            })
            .mut_arg("output", |a| {
                a.value_name(v_path).help(i18n::t("help.ex.output"))
            })
            .mut_arg("password", |a| {
                a.value_name(v_password).help(i18n::t("help.password_opt"))
            })
    });

    // --- info ---
    cmd = cmd.mut_subcommand("info", |sub| {
        sub.about(i18n::t("cmd.info.about"))
            .override_usage(i18n::t("cmd.info.usage"))
            .help_template(sub_template)
            .mut_arg("container", |a| {
                a.value_name(v_container).help(i18n::t("help.container"))
            })
            .mut_arg("password_pos", |a| {
                a.value_name(v_password).help(i18n::t("help.password_pos"))
            })
            .mut_arg("password", |a| {
                a.value_name(v_password).help(i18n::t("help.password_opt"))
            })
    });

    // --- list ---
    cmd = cmd.mut_subcommand("list", |sub| {
        sub.about(i18n::t("cmd.list.about"))
            .override_usage(i18n::t("cmd.list.usage"))
            .help_template(sub_template)
            .mut_arg("container", |a| {
                a.value_name(v_container).help(i18n::t("help.container"))
            })
            .mut_arg("password_pos", |a| {
                a.value_name(v_password).help(i18n::t("help.password_pos"))
            })
            .mut_arg("password", |a| {
                a.value_name(v_password).help(i18n::t("help.password_opt"))
            })
    });

    // --- exists ---
    cmd = cmd.mut_subcommand("exists", |sub| {
        sub.about(i18n::t("cmd.exists.about"))
            .override_usage(i18n::t("cmd.exists.usage"))
            .help_template(sub_template)
            .mut_arg("container", |a| {
                a.value_name(v_container).help(i18n::t("help.container"))
            })
            .mut_arg("path", |a| {
                a.value_name(v_path).help(i18n::t("help.exists.path"))
            })
            .mut_arg("password_pos", |a| {
                a.value_name(v_password).help(i18n::t("help.password_pos"))
            })
            .mut_arg("password", |a| {
                a.value_name(v_password).help(i18n::t("help.password_opt"))
            })
    });

    // --- passwd ---
    cmd = cmd.mut_subcommand("passwd", |sub| {
        sub.about(i18n::t("cmd.passwd.about"))
            .override_usage(i18n::t("cmd.passwd.usage"))
            .help_template(sub_template)
            .mut_arg("container", |a| {
                a.value_name(v_container).help(i18n::t("help.container"))
            })
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
            .mut_arg("full", |a| a.help(i18n::t("help.passwd.full")))
    });

    // --- shell ---
    cmd = cmd.mut_subcommand("shell", |sub| {
        sub.about(i18n::t("cmd.shell.about"))
            .override_usage(i18n::t("cmd.shell.usage"))
            .help_template(sub_template)
            .mut_arg("container", |a| {
                a.value_name(v_container).help(i18n::t("help.container"))
            })
            .mut_arg("password_pos", |a| {
                a.value_name(v_password).help(i18n::t("help.password_pos"))
            })
            .mut_arg("password", |a| {
                a.value_name(v_password).help(i18n::t("help.password_opt"))
            })
    });

    // --- pack ---
    cmd = cmd.mut_subcommand("pack", |sub| {
        sub.about(i18n::t("cmd.pack.about"))
            .override_usage(i18n::t("cmd.pack.usage"))
            .help_template(sub_template)
            .mut_arg("container", |a| {
                a.value_name(v_container).help(i18n::t("help.container"))
            })
            .mut_arg("output", |a| {
                a.value_name(i18n::t("arg.output_file"))
                    .help(i18n::t("help.pack.output"))
            })
            .mut_arg("password_pos", |a| {
                a.value_name(v_password).help(i18n::t("help.password_pos"))
            })
            .mut_arg("password", |a| {
                a.value_name(v_password).help(i18n::t("help.password_opt"))
            })
    });

    // --- unpack ---
    cmd = cmd.mut_subcommand("unpack", |sub| {
        sub.about(i18n::t("cmd.unpack.about"))
            .override_usage(i18n::t("cmd.unpack.usage"))
            .help_template(sub_template)
            .mut_arg("file", |a| {
                a.value_name(i18n::t("arg.package_file"))
                    .help(i18n::t("help.unpack.file"))
            })
            .mut_arg("name", |a| {
                a.value_name(i18n::t("arg.container_name"))
                    .help(i18n::t("help.unpack.name"))
            })
            .mut_arg("workspace", |a| {
                a.value_name(i18n::t("arg.workspace"))
                    .help(i18n::t("help.unpack.workspace"))
            })
            .mut_arg("link", |a| {
                a.value_name(i18n::t("arg.link_file"))
                    .help(i18n::t("help.unpack.link"))
            })
            .mut_arg("password_pos", |a| {
                a.value_name(v_password).help(i18n::t("help.password_pos"))
            })
            .mut_arg("password", |a| {
                a.value_name(v_password).help(i18n::t("help.password_opt"))
            })
    });

    // --- config ---
    cmd = cmd.mut_subcommand("config", |sub| {
        sub.about(i18n::t("cmd.config.about"))
            .override_usage(i18n::t("cmd.config.usage"))
            .help_template(sub_template)
            .mut_arg("hints", |a| {
                a.value_name(i18n::t("arg.hints"))
                    .help(i18n::t("help.config.hints"))
            })
            .mut_arg("action", |a| {
                a.value_name(i18n::t("arg.action"))
                    .help(i18n::t("help.config.action"))
            })
    });

    // --- link ---
    cmd = cmd.mut_subcommand("link", |sub| {
        sub.about(i18n::t("cmd.link.about"))
            .override_usage(i18n::t("cmd.link.usage"))
            .help_template(sub_template)
            .mut_arg("target", |a| {
                a.value_name(i18n::t("arg.target"))
                    .help(i18n::t("help.link.target"))
            })
            .mut_arg("output", |a| {
                a.value_name(i18n::t("arg.link_file"))
                    .help(i18n::t("help.link.output"))
            })
    });

    // --- help ---
    cmd = cmd.mut_subcommand("help", |sub| {
        sub.about(i18n::t("cmd.help.about"))
            .override_usage(i18n::t("cmd.help.usage"))
            .help_template(sub_template)
            .mut_arg("topic", |a| {
                a.value_name(i18n::t("arg.topic"))
                    .help(i18n::t("help.help.topic"))
            })
            .mut_arg("all", |a| a.help(i18n::t("help.help.all")))
    });

    // clap 的内置 help 参数无法本地化；先禁用，再为每个子命令注入当前语言版本。
    for command in [
        "init", "add", "rm", "mv", "free", "ex", "info", "list", "exists", "passwd", "shell",
        "pack", "unpack", "config", "link", "help",
    ] {
        cmd = cmd.mut_subcommand(command, |sub| {
            sub.disable_help_flag(true).arg(localized_help_arg())
        });
    }

    cmd
}

/// 构造本地化的 `--help` 参数。
fn localized_help_arg() -> Arg {
    Arg::new("help_flag")
        .short('h')
        .long("help")
        .action(ArgAction::Help)
        .help(i18n::t("help.flag"))
}

/// 构造本地化的 `--version` 参数。
fn localized_version_arg() -> Arg {
    Arg::new("version_flag")
        .short('V')
        .long("version")
        .action(ArgAction::Version)
        .help(i18n::t("version.flag"))
}

/// 返回指定子命令的位置参数用法文本。
///
/// 未收录的命令返回空字符串，由调用方原样输出。
fn cmd_usage(cmd_name: &str) -> &str {
    match cmd_name {
        "init" => i18n::t("cmd.init.usage"),
        "add" => i18n::t("cmd.add.usage"),
        "rm" => i18n::t("cmd.rm.usage"),
        "mv" => i18n::t("cmd.mv.usage"),
        "free" => i18n::t("cmd.free.usage"),
        "ex" => i18n::t("cmd.ex.usage"),
        "info" => i18n::t("cmd.info.usage"),
        "list" => i18n::t("cmd.list.usage"),
        "exists" => i18n::t("cmd.exists.usage"),
        "passwd" => i18n::t("cmd.passwd.usage"),
        "shell" => i18n::t("cmd.shell.usage"),
        "pack" => i18n::t("cmd.pack.usage"),
        "unpack" => i18n::t("cmd.unpack.usage"),
        _ => "",
    }
}

/// 返回指定子命令的选项参数用法文本。
///
/// 未收录的命令返回空字符串。
fn cmd_usage_opt(cmd_name: &str) -> &str {
    match cmd_name {
        "init" => i18n::t("cmd.init.usage_opt"),
        "add" => i18n::t("cmd.add.usage_opt"),
        "rm" => i18n::t("cmd.rm.usage_opt"),
        "mv" => i18n::t("cmd.mv.usage_opt"),
        "free" => i18n::t("cmd.free.usage_opt"),
        "ex" => i18n::t("cmd.ex.usage_opt"),
        "info" => i18n::t("cmd.info.usage_opt"),
        "exists" => i18n::t("cmd.exists.usage_opt"),
        "passwd" => i18n::t("cmd.passwd.usage_opt"),
        "shell" => i18n::t("cmd.shell.usage_opt"),
        _ => "",
    }
}

/// 输出顶层帮助和全部子命令帮助。
fn print_all_help(command: &clap::Command) -> crate::error::Result<()> {
    let mut root = command.clone();
    root.print_help()?;
    crate::outln!();

    let names: Vec<_> = command
        .get_subcommands()
        .map(|subcommand| subcommand.get_name().to_string())
        .collect();
    for name in names {
        if let Some(mut subcommand) = command.find_subcommand(&name).cloned() {
            crate::outln!("\n===== {name} =====\n");
            subcommand.print_help()?;
            crate::outln!();
        }
    }

    Ok(())
}

/// 输出本地化错误和两种参数写法后终止进程。
///
/// 该函数用于 clap 解析完成后的必填参数校验；固定以状态码 1 退出。
fn exit_with_help(code: crate::error::ErrorCode, cmd_name: &str) -> ! {
    let error = crate::error::CommandError::coded(code);
    eprintln!("{}", error.render());
    eprintln!("\n{}:", i18n::t("label.usage"));
    eprintln!("  {}", cmd_usage(cmd_name));
    eprintln!("  {}", cmd_usage_opt(cmd_name));
    std::process::exit(error.exit_code().into());
}

/// 解析命令行；帮助和版本正常退出，其余错误按 Veil 的交互规则处理。
fn parse_matches(mut command: clap::Command) -> clap::ArgMatches {
    match command.clone().try_get_matches() {
        Ok(matches) => matches,
        Err(error) => handle_clap_error(error, &mut command),
    }
}

/// 将 clap 的解析错误转换为本地化提示。
fn handle_clap_error(error: clap::Error, command: &mut clap::Command) -> ! {
    use clap::error::{ContextKind, ContextValue, ErrorKind};

    /// 读取 clap 上下文字段，并转换为可直接显示的用户文本。
    fn context_text(value: Option<&ContextValue>) -> Option<String> {
        value
            .map(ToString::to_string)
            .filter(|text| !text.is_empty())
    }

    /// 输出本地化错误后打印当前命令的用法并退出。
    fn print_usage_and_exit(command: &mut clap::Command, code: i32) -> ! {
        eprintln!("\n{}:", i18n::t("label.usage"));
        let subcommand_name = std::env::args().nth(1).unwrap_or_default();
        let usage = cmd_usage(&subcommand_name);
        if usage.is_empty() {
            let rendered = command.render_usage().to_string();
            eprintln!("  {}", rendered.trim_start_matches("Usage: "));
        } else {
            eprintln!("  {}", usage);
        }

        std::process::exit(code);
    }

    match error.kind() {
        // Help and version are successful control-flow exits, not errors.
        // 通过统一写入层输出，确保管道提前关闭时返回稳定 I/O 错误。
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            let rendered = error.render().to_string();
            let rendered = rendered.trim_end_matches('\n');
            if let Err(error) = output_encoding::write_stdout(format_args!("{rendered}"), true) {
                output_encoding::exit_for_output_error(error);
            }
            std::process::exit(0);
        }
        ErrorKind::UnknownArgument => {
            let argument = error
                .get(ContextKind::InvalidArg)
                .map(ToString::to_string)
                .unwrap_or_else(|| "?".to_string());
            let error = crate::cli_error!(UnknownArgument, "arg" => argument);
            eprintln!("{}", error.render());
            print_usage_and_exit(command, error.exit_code().into());
        }
        ErrorKind::ArgumentConflict => {
            let invalid = context_text(
                error
                    .get(ContextKind::InvalidArg)
                    .or_else(|| error.get(ContextKind::InvalidSubcommand)),
            );
            let prior = context_text(error.get(ContextKind::PriorArg));
            let mapped = match (invalid, prior) {
                (Some(invalid), Some(prior)) => {
                    crate::cli_error!(ArgumentConflict, "arg" => invalid, "prior" => prior)
                }
                (Some(arg), None) => crate::cli_error!(ArgumentConflictSingle, "arg" => arg),
                _ => crate::cli_error!(InvalidArguments),
            };
            eprintln!("{}", mapped.render());
            print_usage_and_exit(command, mapped.exit_code().into());
        }
        _ => {
            let error = crate::cli_error!(InvalidArguments);
            eprintln!("{}", error.render());
            print_usage_and_exit(command, error.exit_code().into());
        }
    }
}

/// 返回容器参数，缺失时输出对应子命令的帮助并退出。
fn require_container(container: Option<String>, cmd_name: &str) -> String {
    container.unwrap_or_else(|| exit_with_help(crate::error::ErrorCode::MissingContainer, cmd_name))
}

/// 显示程序版本，并根据构建模式输出安全提示。
///
/// Debug 构建会额外说明测试 KDF 和暴力尝试风险；Release 构建只显示发布状态。
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

    crate::outln!(
        "\n{} {}",
        "Veil".cyan().bold(),
        format!("v{}", VERSION).cyan()
    );

    if is_dev {
        crate::outln!("{}", i18n::t("version.dev_warning").yellow().bold());
    }

    if is_debug {
        crate::outln!();
        crate::outln!("{}", i18n::t("version.debug_warning").yellow().bold());
        crate::outln!("{}", i18n::t("version.debug_key_strength").yellow());
        crate::outln!("{}", i18n::t("version.debug_crack_speed").yellow());
        crate::outln!();
        crate::outln!("{}", i18n::t("version.debug_recommend").bright_yellow());
        crate::outln!("{}", i18n::t("version.debug_command"));
        crate::outln!("{}", i18n::t("version.debug_release_strength").green());
        crate::outln!();
    } else {
        crate::outln!("{}", i18n::t("version.release_status").green());
    }

    crate::outln!();
}

/// 初始化终端和国际化环境，解析参数并分派子命令。
///
/// 子命令错误会转换为本地化消息并按失败状态退出；成功路径返回状态码 0。
fn main() {
    // Windows: 启用 UTF-8 控制台模式（Windows 10+ 支持）
    #[cfg(target_os = "windows")]
    {
        // SAFETY: 这里调用 Windows 控制台 API 设置当前进程输出代码页；参数为
        // 系统定义的 UTF-8 代码页 65001，不涉及指针或跨线程内存访问。
        unsafe {
            // 声明当前进程所需的 Windows 控制台 API。
            unsafe extern "system" {
                /// Windows API：设置当前控制台输出代码页。
                fn SetConsoleOutputCP(wCodePageID: u32) -> i32;
            }
            const CP_UTF8: u32 = 65001;
            SetConsoleOutputCP(CP_UTF8);
        }
    }

    i18n::init();

    show_version_and_security_info();

    let cmd = build_localized_command();
    let matches = parse_matches(cmd.clone());
    let cli = Cli::from_arg_matches(&matches).expect("参数解析失败");

    // clap 已完成类型解析；这里只负责把位置参数和选项参数合并成命令层输入。
    let result = match cli.command {
        // init 允许位置密码和 --password 两种写法，优先保留用户实际提供的一项。
        Commands::Init {
            container,
            password,
            password_opt,
            link,
            workspace,
            workspace_path,
            dedicated,
            portable,
        } => {
            let container = require_container(container, "init");
            let pwd = password.or(password_opt);
            commands::init::run(
                &container,
                pwd,
                link.as_deref(),
                workspace.as_deref(),
                workspace_path.map(Into::into),
                dedicated,
                portable,
            )
        }
        // add 的容器、输入、输出和密码均有位置/选项两套兼容输入。
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
                exit_with_help(crate::error::ErrorCode::MissingInputPath, "add");
            }
        }
        // rm 至少需要一个容器内路径，缺失时终止前打印两种参数用法。
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
                exit_with_help(crate::error::ErrorCode::MissingDeletePath, "rm");
            }
        }
        // mv 必须同时获得源路径和目标路径，之后才调用元数据重命名。
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
                commands::mv_workspace::run_workspace(&container, &from, &to, pwd)
            } else {
                exit_with_help(crate::error::ErrorCode::MissingSourceDestination, "mv");
            }
        }
        // free 只需要容器和可选密码，适合快速检查容器内容。
        Commands::Free {
            container,
            password_pos,
            password,
        } => {
            let container = require_container(container, "free");
            let pwd = password_pos.or(password);
            commands::free_workspace::run_workspace(&container, pwd)
        }
        // ex 的输出路径是必填项，输入路径可为空并交由命令层报错。
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
                exit_with_help(crate::error::ErrorCode::MissingOutputPath, "ex");
            }
        }
        // info 只读取元数据并展示身份与统计信息。
        Commands::Info {
            container,
            password_pos,
            password,
        } => {
            let container = require_container(container, "info");
            let pwd = password_pos.or(password);
            commands::info::run(&container, pwd)
        }
        // list 与 free 共用工作区解析，但输出更偏机器可读的逐项清单。
        Commands::List {
            container,
            password_pos,
            password,
        } => {
            let container = require_container(container, "list");
            let pwd = password_pos.or(password);
            commands::list_workspace::run_workspace(&container, pwd)
        }
        // exists 只判断元数据中的文件或隐式目录是否存在。
        Commands::Exists {
            container,
            path,
            password_pos,
            password,
        } => {
            let container = require_container(container, "exists");
            let path = path.unwrap_or_else(|| {
                exit_with_help(crate::error::ErrorCode::MissingCheckPath, "exists")
            });
            let pwd = password_pos.or(password);
            match commands::exists_workspace::run_workspace(&container, &path, pwd) {
                Ok(true) => Ok(()),
                Ok(false) => std::process::exit(1),
                Err(error) => Err(error),
            }
        }
        // passwd 同时接受旧、新密码的位置参数和选项参数。
        Commands::Passwd {
            container,
            old_password_pos,
            new_password_pos,
            password,
            new_password,
            full,
        } => {
            let container = require_container(container, "passwd");
            let old_pwd = old_password_pos.or(password);
            let new_pwd = new_password_pos.or(new_password);
            commands::passwd_workspace::run_workspace(&container, old_pwd, new_pwd, full)
        }
        // shell 在密码验证后进入长期交互循环。
        Commands::Shell {
            container,
            password_pos,
            password,
        } => {
            let container = require_container(container, "shell");
            let pwd = password_pos.or(password);
            commands::shell_workspace::run_workspace(&container, pwd)
        }
        // pack 输出路径可选，默认值由命令实现根据容器名生成。
        Commands::Pack {
            container,
            output,
            password_pos,
            password,
        } => {
            let container = require_container(container, "pack");
            let pwd = password_pos.or(password);
            commands::pack_workspace::run_workspace(&container, output.as_deref(), pwd)
        }
        // unpack 输入的 .veil 文件必填，其余名称、工作区和链接路径均可选。
        Commands::Unpack {
            file,
            name,
            workspace,
            link,
            password_pos,
            password,
        } => {
            let file = file.unwrap_or_else(|| {
                exit_with_help(crate::error::ErrorCode::MissingContainer, "unpack")
            });
            let pwd = password_pos.or(password);
            commands::unpack_workspace::run_workspace(
                &file,
                name.as_deref(),
                workspace.as_deref(),
                link.as_deref(),
                pwd,
            )
        }

        // config 无级别参数时展示；action=show 是兼容显式写法。
        Commands::Config {
            hints,
            action,
            show,
        } => commands::config::run(hints, show || action.as_deref() == Some("show")),

        // link 根据目标解析结果复制或重新生成链接文件。
        Commands::Link { target, output } => commands::link::run(&target, output.as_deref()),

        // help 支持专门的文件类型主题，未给主题则打印顶层帮助。
        Commands::Help { topic, all } => {
            if all {
                print_all_help(&cmd)
            } else {
                match topic.as_deref() {
                    Some("all") => print_all_help(&cmd),
                    Some("files") => {
                        hints::show_files_help();
                        Ok(())
                    }
                    Some(other) => {
                        if let Some(mut subcommand) = cmd.find_subcommand(other).cloned() {
                            match subcommand.print_help() {
                                Ok(()) => {
                                    crate::outln!();
                                    Ok(())
                                }
                                Err(error) => Err(error.into()),
                            }
                        } else {
                            Err(crate::cli_error!(HelpUnknownTopic, "topic" => other))
                        }
                    }
                    None => match cmd.clone().print_help() {
                        Ok(()) => {
                            crate::outln!();
                            Ok(())
                        }
                        Err(error) => Err(error.into()),
                    },
                }
            }
        }
    };

    if let Err(e) = result {
        eprintln!("{}", e.render());
        std::process::exit(e.exit_code().into());
    }
}
