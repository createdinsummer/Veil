//! Veil 命令行程序入口。
//!
//! 本模块负责初始化本地化、构建 clap 命令、解析参数并把各子命令分派到
//! `commands` 模块。命令说明先定义中文兜底文本，再根据 `VEIL_LANG` 在运行时
//! 覆写为实际显示语言。
//!
//! 核心清单：`Cli`/`Commands` 组成 clap 参数树；`build_localized_command` 按当前语言
//! 覆写帮助文本；`parse_matches`/`handle_clap_error` 解析参数并转换错误；
//! `main` 初始化日志与 i18n、分派子命令并按稳定退出码退出。

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
    // ---- 创建容器 ----
    /// 创建新的 Veil 和链接文件。
    Init {
        // 新 Veil 名称或链接输出目标。
        #[arg(value_name = "Veil 名称")]
        veil: Option<String>,
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
        work: Option<String>,
        // 指定工作区根路径并自动登记到当前容器，不会新增命名工作区。
        #[arg(long, value_name = "路径", help = "使用指定工作区路径")]
        work_root: Option<String>,
        // 让 --work-root 指定的目录由当前容器独占。
        #[arg(long, help = "工作区由该容器独占")]
        dedicated: bool,
        // 标记便携模式并影响提示；当前工作流提交位置仍由已选择空间决定。
        #[arg(long, help = "强制将工作区放在链接文件所在的卷")]
        portable: bool,
    },

    // ---- 文件管理：add 导入、rm 删除、mv 移动 ----
    /// 加密并添加本地文件。
    Add {
        // 位置参数形式的 Veil 名称、ID 或链接目标。
        #[arg(value_name = "Veil 名称")]
        veil: Option<String>,
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
        /// 同名目标的处理策略，默认交互选择（相同内容也询问）。
        #[arg(long, value_enum, default_value = "ask")]
        on_conflict: commands::add::ConflictPolicy,
    },

    /// 从容器删除文件。
    Rm {
        // 位置参数形式的容器名称、ID 或链接目标。
        #[arg(value_name = "容器名称")]
        veil: Option<String>,
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
        veil: Option<String>,
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

    // ---- 查看与导出：free 树状视图、ex 解密导出、info 信息、list 清单、exists 存在性 ----
    /// 以树状形式查看容器内容与统计。
    Free {
        // 位置参数形式的容器名称、ID 或链接目标。
        #[arg(value_name = "容器名称")]
        veil: Option<String>,
        // 位置参数形式的密码。
        #[arg(value_name = "密码")]
        password_pos: Option<String>,
        // 选项形式的密码。
        #[arg(short, long, conflicts_with = "password_pos", value_name = "密码")]
        password: Option<String>,
    },

    /// 从容器解密导出文件或文件夹。
    ///
    /// 导出文件夹时，输出路径为目标目录，并保留原文件夹名。
    Ex {
        // 位置参数形式的容器名称、ID 或链接目标。
        #[arg(value_name = "容器名称")]
        veil: Option<String>,
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
        /// 同名目标的处理策略，默认交互选择（相同内容也询问）。
        #[arg(long, value_enum, default_value = "ask")]
        on_conflict: commands::add::ConflictPolicy,
    },

    /// 显示容器身份和内容统计。
    Info {
        // 位置参数形式的容器名称、ID 或链接目标。
        #[arg(value_name = "容器名称")]
        veil: Option<String>,
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
        veil: Option<String>,
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
        veil: Option<String>,
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

    // ---- 凭据与会话：passwd 改密、shell 交互式会话 ----
    /// 修改容器密码。
    Passwd {
        // 位置参数形式的容器名称、ID 或链接目标。
        #[arg(value_name = "容器名称")]
        veil: Option<String>,
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
        veil: Option<String>,
        // 位置参数形式的密码。
        #[arg(value_name = "密码")]
        password_pos: Option<String>,
        // 选项形式的密码。
        #[arg(short, long, conflicts_with = "password_pos", value_name = "密码")]
        password: Option<String>,
    },

    // ---- 备份与迁移：pack 打包、unpack 解包 ----
    /// 打包工作区到 .veil 文件
    Pack {
        // 位置参数形式的容器名称、ID 或链接目标。
        #[arg(value_name = "容器名称")]
        veil: Option<String>,
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
        work: Option<String>,
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

    // ---- 配置与链接：config 设置、link 重建链接 ----
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
        // 已注册 Veil 名称、ID、现有链接路径或 veil_dir。
        #[arg(value_name = "Veil 名、ID、链接或目录")]
        target: String,
        // 新链接文件的输出路径。
        #[arg(short, long, value_name = "链接文件")]
        output: Option<String>,
    },

    // ---- 诊断与帮助：logs 导出日志、help 帮助 ----
    /// 将本地诊断日志导出为 JSON 文件（不发送网络请求）。
    Logs {
        // 可选输出路径；省略时在当前目录生成带 UTC 时间的 JSON 文件。
        #[arg(short, long, value_name = "输出文件")]
        output: Option<std::path::PathBuf>,
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

/// 构造所有CLI子命令的参数树，并用当前语言覆盖帮助展示。
/// 入参：无，基于Cli/Commands派生定义及已初始化i18n。
/// 返回值：clap::Command，含根帮助/版本标志与逐子命令帮助、usage、参数说明。
/// 异常：无Result返回；命令定义不一致等clap内部校验可panic。
/// 注意事项：此阶段只设置解析结构，不执行业务；部分说明为中文兜底，缺翻译回退键名。mut_arg/mut_subcommand名称须与派生定义对应。
fn build_localized_command() -> clap::Command {
    // 先构建中文兜底命令树（clap derive 生成），再按当前语言逐项覆写显示文本。
    let mut cmd = Cli::command()
        .disable_help_flag(true)
        .disable_version_flag(true)
        .arg(localized_help_arg())
        .arg(localized_version_arg())
        .about(i18n::clap_about())
        .long_about(i18n::clap_long_about())
        .help_template(i18n::t("clap.help_template"));

    let sub_template = i18n::t("clap.sub_help_template");
    // 常用参数值名称先查一次，供下面多个子命令复用，避免重复查询。
    let v_veil = i18n::t("arg.veil");
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
            .mut_arg("veil", |a| {
                a.value_name(v_veil).help(i18n::t("help.init.veil"))
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
            .mut_arg("work", |a| {
                a.value_name(i18n::t("arg.work"))
                    .help(i18n::t("help.init.work"))
            })
            .mut_arg("work_root", |a| {
                a.value_name(i18n::t("arg.path"))
                    .help(i18n::t("help.init.work_root"))
            })
            .mut_arg("dedicated", |a| a.help(i18n::t("help.init.dedicated")))
            .mut_arg("portable", |a| a.help(i18n::t("help.init.portable")))
    });

    // --- add ---
    cmd = cmd.mut_subcommand("add", |sub| {
        sub.about(i18n::t("cmd.add.about"))
            .override_usage(i18n::t("cmd.add.usage"))
            .help_template(sub_template)
            .mut_arg("veil", |a| a.value_name(v_veil).help(i18n::t("help.veil")))
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
            .mut_arg("veil", |a| a.value_name(v_veil).help(i18n::t("help.veil")))
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
            .mut_arg("veil", |a| a.value_name(v_veil).help(i18n::t("help.veil")))
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
            .mut_arg("veil", |a| a.value_name(v_veil).help(i18n::t("help.veil")))
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
            .mut_arg("veil", |a| a.value_name(v_veil).help(i18n::t("help.veil")))
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
            .mut_arg("veil", |a| a.value_name(v_veil).help(i18n::t("help.veil")))
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
            .mut_arg("veil", |a| a.value_name(v_veil).help(i18n::t("help.veil")))
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
            .mut_arg("veil", |a| a.value_name(v_veil).help(i18n::t("help.veil")))
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
            .mut_arg("veil", |a| a.value_name(v_veil).help(i18n::t("help.veil")))
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
            .mut_arg("veil", |a| a.value_name(v_veil).help(i18n::t("help.veil")))
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
            .mut_arg("veil", |a| a.value_name(v_veil).help(i18n::t("help.veil")))
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
                a.value_name(i18n::t("arg.veil_name"))
                    .help(i18n::t("help.unpack.name"))
            })
            .mut_arg("work", |a| {
                a.value_name(i18n::t("arg.work"))
                    .help(i18n::t("help.unpack.work"))
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
        "pack", "unpack", "config", "link", "logs", "help",
    ] {
        cmd = cmd.mut_subcommand(command, |sub| {
            sub.disable_help_flag(true).arg(localized_help_arg())
        });
    }

    // 返回完全本地化的命令树，供解析与帮助渲染共用。
    cmd
}

/// 创建本地化--help/-h控制参数。
/// 入参：无。
/// 返回值：Arg，使用ArgAction::Help，供命令树解析时生成显示控制流。
/// 异常：无错误返回。
/// 注意事项：不在此打印或退出；parse_matches处理相应clap显示结果。
fn localized_help_arg() -> Arg {
    Arg::new("help_flag")
        .short('h')
        .long("help")
        .action(ArgAction::Help)
        .help(i18n::t("help.flag"))
}

/// 创建本地化--version/-V控制参数。
/// 入参：无。
/// 返回值：Arg，使用ArgAction::Version，供命令树解析时生成显示控制流。
/// 异常：无错误返回。
/// 注意事项：不在此打印或退出；parse_matches处理相应clap显示结果。
fn localized_version_arg() -> Arg {
    Arg::new("version_flag")
        .short('V')
        .long("version")
        .action(ArgAction::Version)
        .help(i18n::t("version.flag"))
}

/// 为自定义错误提示选择位置参数用法文案。
/// 入参：cmd_name（&str）为子命令名，精确小写匹配。
/// 返回值：&str，已收录命令的本地化usage模板；未收录为空。
/// 异常：无错误返回。
/// 注意事项：只是显示规则，不代表clap实际参数验证；调用方决定空串时回退或直接输出。
fn cmd_usage(cmd_name: &str) -> &str {
    match cmd_name {
        // 只收录有自定义用法的命令；其余命令（config/link/logs/help）由 clap 渲染。
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
        // 未收录的命令返回空串，调用方回退到 clap 渲染的用法。
        _ => "",
    }
}

/// 为自定义错误提示选择选项参数用法文案。
/// 入参：cmd_name（&str）为子命令名，精确小写匹配。
/// 返回值：&str，已收录命令的本地化usage模板；未收录为空。
/// 异常：无错误返回。
/// 注意事项：只是显示规则，不代表clap实际参数验证；调用方决定空串时回退或直接输出。
fn cmd_usage_opt(cmd_name: &str) -> &str {
    match cmd_name {
        // 只有带选项参数的命令才需要第二行用法说明。
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

/// 逐一输出根命令和所有已注册子命令的帮助。
/// 入参：command（&clap::Command）为已本地化命令树。
/// 返回值：全部clap帮助打印成功后Ok(())。
/// 异常：clap print_help I/O失败转CommandError；输出宏失败直接结束进程。
/// 注意事项：克隆命令后渲染，遍历注册的子命令名称每项一次；不递归额外子层。
fn print_all_help(command: &clap::Command) -> crate::error::Result<()> {
    // 克隆一份避免借用冲突：顶层帮助打印后还要遍历子命令。
    let mut root = command.clone();
    root.print_help()?;
    crate::outln!();

    // 收集全部子命令名（按注册顺序），保证输出顺序稳定。
    let names: Vec<_> = command
        .get_subcommands()
        .map(|subcommand| subcommand.get_name().to_string())
        .collect();
    // 逐个打印分隔标题与对应子命令的本地化帮助。
    for name in names {
        if let Some(mut subcommand) = command.find_subcommand(&name).cloned() {
            crate::outln!("\n===== {name} =====\n");
            subcommand.print_help()?;
            crate::outln!();
        }
    }

    Ok(())
}

/// 为解析后缺少业务必填参数的场景报告错误和两种用法并退出。
/// 入参：code（ErrorCode）为固定参数错误；cmd_name（&str）为相关子命令。
/// 返回值：!，process::exit使用该错误exit_code（当前参数错误为2）。
/// 异常：不返回Result，stderr打印仍可能因I/O失败panic。
/// 注意事项：未收录usage会打印空行；直接退出不展开现有作用域析构，也不经过main末尾统一flush。
fn exit_with_help(code: crate::error::ErrorCode, cmd_name: &str) -> ! {
    let error = crate::error::CommandError::coded(code);
    // 先报告带错误码的错误文本。
    eprintln!("{}", error.report());
    // 位置参数与选项参数两种写法都打印，方便用户对照修正命令行。
    eprintln!("\n{}:", i18n::t("label.usage"));
    eprintln!("  {}", cmd_usage(cmd_name));
    eprintln!("  {}", cmd_usage_opt(cmd_name));
    std::process::exit(error.exit_code().into());
}

/// 从进程参数解析本地化命令树，把显示/错误控制流交统一处理。
/// 入参：command（clap::Command）按值传入并克隆用于try_get_matches。
/// 返回值：成功返回ArgMatches；帮助/版本/错误分支不返回。
/// 异常：clap错误交handle_clap_error打印并退出，不作为Result返回。
/// 注意事项：从真实进程环境读取参数；此方法不执行业务，也不转换成Cli结构。
fn parse_matches(mut command: clap::Command) -> clap::ArgMatches {
    match command.clone().try_get_matches() {
        Ok(matches) => matches,
        Err(error) => handle_clap_error(error, &mut command),
    }
}

/// 将帮助/版本或参数解析失败转为本地化输出与进程退出。
/// 入参：error（clap::Error）为解析结果；command（&mut Command）用于渲染usage。
/// 返回值：!，显示帮助/版本后退出0，参数错误按CLI稳定退出码结束。
/// 异常：帮助stdout失败交exit_for_output_error；其他stderr输出失败可能panic。
/// 注意事项：非显示控制流仅记不含参数正文的Info事件；不保证该等级默认落盘。不透传所有clap原始错误，未知/冲突专门映射，其余统一InvalidArguments。
fn handle_clap_error(error: clap::Error, command: &mut clap::Command) -> ! {
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    // 帮助与版本是正常控制流（用户主动请求），不记录错误日志。
    if !matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        // 其余解析错误统一按参数错误留痕，供本地诊断日志检索。
        veil_core::logging::record(
            veil_core::logging::Level::Info,
            "cli",
            "parse_arguments",
            "INVALID_ARGUMENTS",
        );
    }

    /// 从clap错误上下文取得可用的显示文本。
    /// 入参：value（`Option<&ContextValue>`）为可选字段。
    /// 返回值：存在且ToString后非空返回Some(String)，否则None。
    /// 异常：无错误返回。
    /// 注意事项：不负责脱敏；只用于构造参数冲突说明。
    fn context_text(value: Option<&ContextValue>) -> Option<String> {
        value
            .map(ToString::to_string)
            .filter(|text| !text.is_empty())
    }

    /// 为参数错误选用命令用法并退出。
    /// 入参：command为可渲染命令；code（i32）为退出状态。
    /// 返回值：!，输出用法后process::exit(code)。
    /// 异常：无Result；环境args非Unicode或stderr失败可panic。
    /// 注意事项：用进程第二个参数猜子命令；无自定义用法则渲染根命令usage，不解析完整嵌套路径。
    fn print_usage_and_exit(command: &mut clap::Command, code: i32) -> ! {
        eprintln!("\n{}:", i18n::t("label.usage"));
        // 第二个参数位置通常是子命令名，据此选择对应命令的用法文本。
        let subcommand_name = std::env::args().nth(1).unwrap_or_default();
        let usage = cmd_usage(&subcommand_name);
        if usage.is_empty() {
            // 未收录的命令回退到 clap 渲染的用法。
            let rendered = command.render_usage().to_string();
            eprintln!("  {}", rendered.trim_start_matches("Usage: "));
        } else {
            eprintln!("  {}", usage);
        }

        std::process::exit(code);
    }

    match error.kind() {
        // 帮助和版本属于正常显示控制流，不是业务失败。
        // 通过统一写入层输出，确保管道提前关闭时返回稳定 I/O 错误。
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            // 帮助/版本是用户主动请求，渲染后以退出码 0 正常结束。
            let rendered = error.render().to_string();
            let rendered = rendered.trim_end_matches('\n');
            if let Err(error) = output_encoding::write_stdout(format_args!("{rendered}"), true) {
                output_encoding::exit_for_output_error(error);
            }
            std::process::exit(0);
        }
        ErrorKind::UnknownArgument => {
            // 未知参数：把参数名放进模板展示给用户。
            let argument = error
                .get(ContextKind::InvalidArg)
                .map(ToString::to_string)
                .unwrap_or_else(|| "?".to_string());
            let error = crate::cli_error!(UnknownArgument, "arg" => argument);
            eprintln!("{}", error.report());
            print_usage_and_exit(command, error.exit_code().into());
        }
        ErrorKind::ArgumentConflict => {
            // 冲突参数：优先展示互相冲突的两个参数；
            // 只有单个参数信息时用单参数文案，都没有则归为普通参数错误。
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
            eprintln!("{}", mapped.report());
            print_usage_and_exit(command, mapped.exit_code().into());
        }
        _ => {
            // 其余解析错误（缺少必填值等）统一归为参数错误。
            let error = crate::cli_error!(InvalidArguments);
            eprintln!("{}", error.report());
            print_usage_and_exit(command, error.exit_code().into());
        }
    }
}

/// 要求命令已提供容器或初始化目标参数。
/// 入参：veil（`Option<String>`）为解析值；cmd_name为错误用法命令名。
/// 返回值：Some时原样返回String，None报MissingVeil并退出。
/// 异常：无Result返回；缺失直接终止进程。
/// 注意事项：不验证空白/UUID/路径存在，也不检查密码；具体业务层继续校验。
fn require_veil(veil: Option<String>, cmd_name: &str) -> String {
    // 容器参数缺失时直接打印帮助并退出，不进入命令层。
    veil.unwrap_or_else(|| exit_with_help(crate::error::ErrorCode::MissingVeil, cmd_name))
}

/// 在解析参数前输出构建版本及开发版提示。
/// 入参：无，版本取CARGO_PKG_VERSION。
/// 返回值：无。
/// 异常：输出失败由宏以1结束。
/// 注意事项：版本串含dev/alpha/beta才显示警告；与数据库派生参数无直接关系。所有命令甚至帮助/错误之前都会打印横幅。
fn show_version_banner() {
    use colored::Colorize;

    const VERSION: &str = env!("CARGO_PKG_VERSION");

    // 检测版本类型
    // 版本串含 dev/alpha/beta 视为开发版，后续追加醒目警告。
    let is_dev = VERSION.contains("dev") || VERSION.contains("alpha") || VERSION.contains("beta");

    crate::outln!(
        "\n{} {}",
        "Veil".cyan().bold(),
        format!("v{}", VERSION).cyan()
    );

    if is_dev {
        // 开发版提示用户不要用于正式数据。
        crate::outln!("{}", i18n::t("version.dev_warning").yellow().bold());
    }

    crate::outln!();
}

/// 初始化CLI环境、解析并补齐业务参数，再分派命令和决定退出状态。
/// 入参：无显式入参，读取进程参数、环境、TTY及全局配置。
/// 返回值：正常函数返回为进程0；exists缺失结果直接退出1；错误、帮助和版本由相应分支退出。
/// 异常：命令错误本地化报告后按exit_code退出；派生解析不一致expect、线程或其他底层panic不捕获。
/// 注意事项：Windows尝试设置输出UTF-8但忽略系统返回值；日志尽力初始化/flush，不保证全部持久化。process::exit分支不展开局部守卫；敏感作用域可由后续服务登记值，不是禁止保存所有敏感数据。
fn main() {
    // 尽力初始化日志；队列、权限或磁盘故障仍可能丢记录，不能当成持久化保证。
    veil_core::logging::initialize();
    // Windows: 启用 UTF-8 控制台模式（Windows 10+ 支持）
    #[cfg(target_os = "windows")]
    {
        // SAFETY: 这里调用 Windows 控制台 API 设置当前进程输出代码页；参数为
        // 系统定义的 UTF-8 代码页 65001，不涉及指针或跨线程内存访问。
        unsafe {
            // 声明当前进程所需的 Windows 控制台 API。
            unsafe extern "system" {
                /// 请求修改当前 Windows 控制台输出代码页。
                /// 入参：wCodePageID（u32）系统代码页编号，此调用传 UTF-8 的 65001。
                /// 返回值：i32，非零表示成功，0 表示失败；当前调用处未检查。
                /// 异常：系统错误不转 Rust Result。
                /// 注意事项：仅 Windows，修改输出代码页不改变输入代码页，也不保证重定向输出编码自动转换。
                fn SetConsoleOutputCP(wCodePageID: u32) -> i32;
            }
            const CP_UTF8: u32 = 65001;
            SetConsoleOutputCP(CP_UTF8);
        }
    }

    // 初始化 i18n：读取 VEIL_LANG 并加载内嵌消息表，之后所有文案按当前语言渲染。
    i18n::init();

    show_version_banner();

    // 构建本地化命令树并解析参数；解析错误由 handle_clap_error 直接退出。
    let cmd = build_localized_command();
    let matches = parse_matches(cmd.clone());
    let cli = Cli::from_arg_matches(&matches).expect("参数解析失败");
    // 请求级日志作用域：后续日志携带本次命令上下文。
    let _request = veil_core::logging::request_scope();
    // 开始空的敏感作用域，后续服务可登记密码用于脱敏；这不表示禁止缓存受保护副本。
    let _secrets = veil_core::logging::sensitive_scope(&[]);

    // clap 已完成类型解析；这里只负责把位置参数和选项参数合并成命令层输入。
    let result = match cli.command {
        // init 允许位置密码和 --password 两种写法，优先保留用户实际提供的一项。
        Commands::Init {
            veil,
            password,
            password_opt,
            link,
            work,
            work_root,
            dedicated,
            portable,
        } => {
            let veil = require_veil(veil, "init");
            let pwd = password.or(password_opt);
            commands::init::run(
                &veil,
                pwd,
                link.as_deref(),
                work.as_deref(),
                work_root.map(Into::into),
                dedicated,
                portable,
            )
        }
        // add 的容器、输入、输出和密码均有位置/选项两套兼容输入。
        Commands::Add {
            veil,
            input_pos,
            output_pos,
            password_pos,
            input,
            output,
            password,
            on_conflict,
        } => {
            let veil = require_veil(veil, "add");
            let inp = input_pos.or(input);
            let out = output_pos.or(output);
            let pwd = password_pos.or(password);

            if let Some(inp) = inp {
                commands::add::run(&veil, &inp, out.as_deref(), pwd, on_conflict)
            } else {
                exit_with_help(crate::error::ErrorCode::MissingInputPath, "add");
            }
        }
        // rm 至少需要一个容器内路径，缺失时终止前打印两种参数用法。
        Commands::Rm {
            veil,
            path,
            password_pos,
            password,
        } => {
            let veil = require_veil(veil, "rm");
            let pwd = password_pos.or(password);
            if let Some(path) = path {
                commands::rm::run(&veil, &path, pwd)
            } else {
                exit_with_help(crate::error::ErrorCode::MissingDeletePath, "rm");
            }
        }
        // mv 必须同时获得源路径和目标路径，之后才调用元数据重命名。
        Commands::Mv {
            veil,
            from_pos,
            to_pos,
            password_pos,
            input,
            output,
            password,
        } => {
            let veil = require_veil(veil, "mv");
            let from = from_pos.or(input);
            let to = to_pos.or(output);
            let pwd = password_pos.or(password);

            if let (Some(from), Some(to)) = (from, to) {
                commands::mv::run(&veil, &from, &to, pwd)
            } else {
                exit_with_help(crate::error::ErrorCode::MissingSourceDestination, "mv");
            }
        }
        // free 只需要容器和可选密码，适合快速检查容器内容。
        Commands::Free {
            veil,
            password_pos,
            password,
        } => {
            let veil = require_veil(veil, "free");
            let pwd = password_pos.or(password);
            commands::free::run(&veil, pwd)
        }
        // ex 的输出路径是必填项，输入路径可为空并交由命令层报错。
        Commands::Ex {
            veil,
            input_pos,
            output_pos,
            password_pos,
            input,
            output,
            password,
            on_conflict,
        } => {
            let veil = require_veil(veil, "ex");
            let inp = input_pos.or(input);
            let out = output_pos.or(output);
            let pwd = password_pos.or(password);

            if let Some(out) = out {
                commands::ex::run(&veil, inp.as_deref(), &out, pwd, on_conflict)
            } else {
                exit_with_help(crate::error::ErrorCode::MissingOutputPath, "ex");
            }
        }
        // info 只读取元数据并展示身份与统计信息。
        Commands::Info {
            veil,
            password_pos,
            password,
        } => {
            let veil = require_veil(veil, "info");
            let pwd = password_pos.or(password);
            commands::info::run(&veil, pwd)
        }
        // list 与 free 共用工作区解析，但输出更偏机器可读的逐项清单。
        Commands::List {
            veil,
            password_pos,
            password,
        } => {
            let veil = require_veil(veil, "list");
            let pwd = password_pos.or(password);
            commands::list::run(&veil, pwd)
        }
        // exists查询数据库显式文件/目录节点，缺失作为正常bool结果映射退出码1。
        Commands::Exists {
            veil,
            path,
            password_pos,
            password,
        } => {
            let veil = require_veil(veil, "exists");
            let path = path.unwrap_or_else(|| {
                exit_with_help(crate::error::ErrorCode::MissingCheckPath, "exists")
            });
            let pwd = password_pos.or(password);
            // 存在 → Ok(true) 走成功路径（退出码 0）；
            // 不存在 → 直接以退出码 1 结束，适合脚本 if 判断。
            match commands::exists::run(&veil, &path, pwd) {
                Ok(true) => Ok(()),
                Ok(false) => std::process::exit(1),
                Err(error) => Err(error),
            }
        }
        // passwd 同时接受旧、新密码的位置参数和选项参数。
        Commands::Passwd {
            veil,
            old_password_pos,
            new_password_pos,
            password,
            new_password,
            full,
        } => {
            let veil = require_veil(veil, "passwd");
            let old_pwd = old_password_pos.or(password);
            let new_pwd = new_password_pos.or(new_password);
            commands::passwd::run(&veil, old_pwd, new_pwd, full)
        }
        // shell 在密码验证后进入长期交互循环。
        Commands::Shell {
            veil,
            password_pos,
            password,
        } => {
            let veil = require_veil(veil, "shell");
            let pwd = password_pos.or(password);
            commands::shell::run(&veil, pwd)
        }
        // pack 输出路径可选，默认值由命令实现根据容器名生成。
        Commands::Pack {
            veil,
            output,
            password_pos,
            password,
        } => {
            let veil = require_veil(veil, "pack");
            let pwd = password_pos.or(password);
            commands::pack::run(&veil, output.as_deref(), pwd)
        }
        // unpack 输入的 .veil 文件必填，其余名称、工作区和链接路径均可选。
        Commands::Unpack {
            file,
            name,
            work,
            link,
            password_pos,
            password,
        } => {
            let file = file
                .unwrap_or_else(|| exit_with_help(crate::error::ErrorCode::MissingVeil, "unpack"));
            let pwd = password_pos.or(password);
            commands::unpack::run(
                &file,
                name.as_deref(),
                work.as_deref(),
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

        // logs 导出本地诊断日志；省略输出时在当前目录生成带 UTC 时间的 JSON。
        Commands::Logs { output } => commands::logs::run(output),

        // help 支持专门的文件类型主题，未给主题则打印顶层帮助。
        Commands::Help { topic, all } => {
            if all {
                // --all：顶层帮助 + 全部子命令帮助。
                print_all_help(&cmd)
            } else {
                match topic.as_deref() {
                    // `help all` 与 `--all` 等价。
                    Some("all") => print_all_help(&cmd),
                    // `help files`：输出各文件扩展名与容器文件的用途说明。
                    Some("files") => {
                        hints::show_files_help();
                        Ok(())
                    }
                    // 其余主题按子命令名查找对应帮助。
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
                            // 主题不是已知子命令 → 8001 帮助错误。
                            Err(crate::cli_error!(HelpUnknownTopic, "topic" => other))
                        }
                    }
                    // 未给主题：打印顶层帮助。
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

    // 统一错误出口：报告并记录错误后按稳定退出码退出。
    if let Err(e) = result {
        eprintln!("{}", e.report());
        let _ = veil_core::logging::flush();
        std::process::exit(e.exit_code().into());
    }
    // 成功路径刷新日志缓冲后正常返回（退出码 0）。
    let _ = veil_core::logging::flush();
}
