//! 工作区 CLI 测试程序
//!
//! 用于测试新的工作区架构命令

use clap::{Parser, Subcommand};
use veil_cli::{commands, i18n};

#[derive(Parser)]
#[command(name = "veil-ws", version, about = "Veil 工作区测试工具")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 初始化新容器
    Init {
        /// 容器名称
        name: String,

        /// 密码
        #[arg(short, long)]
        password: Option<String>,

        /// 工作区名称
        #[arg(short, long)]
        workspace: Option<String>,

        /// 专属工作区路径
        #[arg(long)]
        workspace_path: Option<String>,

        /// 是否为专属工作区
        #[arg(long)]
        dedicated: bool,
    },

    /// 添加文件
    Add {
        /// 容器名称
        container: String,

        /// 文件路径
        file: String,

        /// 密码
        #[arg(short, long)]
        password: Option<String>,
    },

    /// 列出文件
    List {
        /// 容器名称
        container: String,

        /// 密码
        #[arg(short, long)]
        password: Option<String>,
    },

    /// 提取文件
    Extract {
        /// 容器名称
        container: String,

        /// 文件名
        file: String,

        /// 输出路径
        #[arg(short, long)]
        output: String,

        /// 密码
        #[arg(short, long)]
        password: Option<String>,
    },

    /// 删除文件
    Rm {
        /// 容器名称
        container: String,

        /// 文件名
        file: String,

        /// 密码
        #[arg(short, long)]
        password: Option<String>,
    },

    /// 打包容器到 .veil 文件
    Pack {
        /// 容器名称
        container: String,

        /// 输出文件路径
        #[arg(short, long)]
        output: Option<String>,

        /// 密码
        #[arg(short, long)]
        password: Option<String>,
    },

    /// 解包 .veil 文件到工作区
    Unpack {
        /// 容器文件路径
        file: String,

        /// 容器名称（默认从文件名提取）
        #[arg(short, long)]
        name: Option<String>,

        /// 工作区名称
        #[arg(short, long)]
        workspace: Option<String>,

        /// 密码
        #[arg(short, long)]
        password: Option<String>,
    },
}

fn main() {
    i18n::init();

    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Init {
            name,
            password,
            workspace,
            workspace_path,
            dedicated,
        } => {
            let ws_path = workspace_path.map(std::path::PathBuf::from);
            commands::init_workspace::run_workspace(
                &name,
                password,
                workspace.as_deref(),
                ws_path,
                dedicated,
            )
        }
        Commands::Add {
            container,
            file,
            password,
        } => commands::add_workspace::run_workspace(&container, &file, password),
        Commands::List { container, password } => {
            commands::list_workspace::run_workspace(&container, password)
        }
        Commands::Extract {
            container,
            file,
            output,
            password,
        } => commands::extract_workspace::run_workspace(&container, &file, &output, password),
        Commands::Rm {
            container,
            file,
            password,
        } => commands::rm_workspace::run_workspace(&container, &file, password),
        Commands::Pack {
            container,
            output,
            password,
        } => commands::pack_workspace::run_workspace(&container, output.as_deref(), password),
        Commands::Unpack {
            file,
            name,
            workspace,
            password,
        } => commands::unpack_workspace::run_workspace(&file, name.as_deref(), workspace.as_deref(), password),
    };

    if let Err(e) = result {
        eprintln!("错误: {}", e);
        std::process::exit(1);
    }
}
