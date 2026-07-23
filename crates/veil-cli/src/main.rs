use clap::{Parser, Subcommand};

mod commands;

/// Veil - 一个简单、安全、高效的文件加密容器工具
#[derive(Parser)]
#[command(name = "veil")]
#[command(version)]
#[command(about = format!("Veil v{} - 一个简单、安全、高效的文件加密容器工具", env!("CARGO_PKG_VERSION")))]
#[command(long_about = format!("Veil v{}\n一个简单、安全、高效的文件加密容器工具", env!("CARGO_PKG_VERSION")))]
struct Cli {
    /// 显示版本信息
    #[arg(short = 'v', long = "version", action = clap::ArgAction::Version)]
    version: (),

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 创建新容器
    Init {
        /// 容器文件路径（如 photos.veil）
        container: String,

        /// 容器密码（位置参数或 -p 选项）
        password: Option<String>,

        /// 容器密码（选项方式）
        #[arg(short, long, conflicts_with = "password")]
        password_opt: Option<String>,
    },

    /// 添加文件或目录到容器
    Add {
        /// 容器文件路径
        container: String,

        /// 源文件或目录路径（容器外部，位置参数或 -i）
        input_pos: Option<String>,

        /// 容器内的目标路径（位置参数或 -o，可选）
        output_pos: Option<String>,

        /// 容器密码（位置参数，可选）
        password_pos: Option<String>,

        /// 源文件或目录路径（选项方式）
        #[arg(short, long, conflicts_with = "input_pos")]
        input: Option<String>,

        /// 容器内的目标路径（选项方式）
        #[arg(short, long, conflicts_with = "output_pos")]
        output: Option<String>,

        /// 容器密码（选项方式）
        #[arg(short, long, conflicts_with = "password_pos")]
        password: Option<String>,
    },

    /// 从容器删除文件或目录
    Rm {
        /// 容器文件路径
        container: String,

        /// 要删除的虚拟路径
        path: String,

        /// 容器密码（位置参数，可选）
        password_pos: Option<String>,

        /// 容器密码（选项方式）
        #[arg(short, long, conflicts_with = "password_pos")]
        password: Option<String>,
    },

    /// 移动/重命名容器内的文件
    Mv {
        /// 容器文件路径
        container: String,

        /// 源路径（位置参数或 -i，容器内）
        from_pos: Option<String>,

        /// 目标路径（位置参数或 -o，容器内）
        to_pos: Option<String>,

        /// 容器密码（位置参数，可选）
        password_pos: Option<String>,

        /// 源路径（选项方式，容器内）
        #[arg(short, long, conflicts_with = "from_pos")]
        input: Option<String>,

        /// 目标路径（选项方式，容器内）
        #[arg(short, long, conflicts_with = "to_pos")]
        output: Option<String>,

        /// 容器密码（选项方式）
        #[arg(short, long, conflicts_with = "password_pos")]
        password: Option<String>,
    },

    /// 树状显示容器内容
    Free {
        /// 容器文件路径
        container: String,

        /// 容器密码（位置参数，可选）
        password_pos: Option<String>,

        /// 容器密码（选项方式）
        #[arg(short, long, conflicts_with = "password_pos")]
        password: Option<String>,
    },

    /// 导出文件或目录
    Ex {
        /// 容器文件路径
        container: String,

        /// 容器内的虚拟路径（位置参数或 -i）
        input_pos: Option<String>,

        /// 导出到的目标路径（位置参数或 -o）
        output_pos: Option<String>,

        /// 容器密码（位置参数，可选）
        password_pos: Option<String>,

        /// 容器内的虚拟路径（选项方式）
        #[arg(short, long, conflicts_with = "input_pos")]
        input: Option<String>,

        /// 导出到的目标路径（选项方式）
        #[arg(short, long, conflicts_with = "output_pos")]
        output: Option<String>,

        /// 导出全部内容
        #[arg(short, long)]
        all: bool,

        /// 容器密码（选项方式）
        #[arg(short, long, conflicts_with = "password_pos")]
        password: Option<String>,
    },

    /// 显示容器信息
    Info {
        /// 容器文件路径
        container: String,

        /// 容器密码（位置参数，可选）
        password_pos: Option<String>,

        /// 容器密码（选项方式）
        #[arg(short, long, conflicts_with = "password_pos")]
        password: Option<String>,
    },

    /// 修改容器密码
    Passwd {
        /// 容器文件路径
        container: String,

        /// 当前密码（位置参数，可选）
        old_password_pos: Option<String>,

        /// 新密码（位置参数，可选）
        new_password_pos: Option<String>,

        /// 当前密码（选项方式）
        #[arg(short, long, conflicts_with = "old_password_pos")]
        password: Option<String>,

        /// 新密码（选项方式）
        #[arg(short = 'n', long, conflicts_with = "new_password_pos")]
        new_password: Option<String>,
    },

    /// 交互式 shell 模式（批处理）
    Shell {
        /// 容器文件路径
        container: String,

        /// 容器密码（位置参数，可选）
        password_pos: Option<String>,

        /// 容器密码（选项方式）
        #[arg(short, long, conflicts_with = "password_pos")]
        password: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Init { container, password, password_opt } => {
            let pwd = password.or(password_opt);
            commands::init::run(&container, pwd)
        }
        Commands::Add { container, input_pos, output_pos, password_pos, input, output, password } => {
            let inp = input_pos.or(input);
            let out = output_pos.or(output);
            let pwd = password_pos.or(password);

            if let Some(inp) = inp {
                commands::add::run(&container, &inp, out.as_deref(), pwd)
            } else {
                eprintln!("❌ 错误: 请指定输入路径（位置参数或 -i）");
                std::process::exit(1);
            }
        }
        Commands::Rm { container, path, password_pos, password } => {
            let pwd = password_pos.or(password);
            commands::rm::run(&container, &path, pwd)
        }
        Commands::Mv { container, from_pos, to_pos, password_pos, input, output, password } => {
            let from = from_pos.or(input);
            let to = to_pos.or(output);
            let pwd = password_pos.or(password);

            if let (Some(from), Some(to)) = (from, to) {
                commands::mv::run(&container, &from, &to, pwd)
            } else {
                eprintln!("❌ 错误: 请指定源路径和目标路径");
                std::process::exit(1);
            }
        }
        Commands::Free { container, password_pos, password } => {
            let pwd = password_pos.or(password);
            commands::free::run(&container, pwd)
        }
        Commands::Ex { container, input_pos, output_pos, password_pos, input, output, all, password } => {
            let inp = input_pos.or(input);
            let out = output_pos.or(output);
            let pwd = password_pos.or(password);

            if let Some(out) = out {
                commands::ex::run(&container, inp.as_deref(), &out, all, pwd)
            } else {
                eprintln!("❌ 错误: 请指定输出路径（位置参数或 -o）");
                std::process::exit(1);
            }
        }
        Commands::Info { container, password_pos, password } => {
            let pwd = password_pos.or(password);
            commands::info::run(&container, pwd)
        }
        Commands::Passwd { container, old_password_pos, new_password_pos, password, new_password } => {
            let old_pwd = old_password_pos.or(password);
            let new_pwd = new_password_pos.or(new_password);
            commands::passwd::run(&container, old_pwd, new_pwd)
        }
        Commands::Shell { container, password_pos, password } => {
            let pwd = password_pos.or(password);
            commands::shell::run(&container, pwd)
        }
    };

    if let Err(e) = result {
        eprintln!("❌ 错误: {}", e);
        std::process::exit(1);
    }
}
