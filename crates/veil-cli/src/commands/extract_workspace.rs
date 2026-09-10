use anyhow::Result;
use colored::Colorize;
use veil_core::config::GlobalConfig;
use veil_core::workspace_ops::WorkspaceManager;
use std::path::Path;

/// 从容器提取文件
pub fn run_workspace(
    container_name: &str,
    file_name: &str,
    output_path: &str,
    password: Option<String>,
) -> Result<()> {
    // 加载配置
    let config = GlobalConfig::load()?;

    // 获取容器工作区路径
    let workspace_path = config.get_container_workspace_path(container_name)?;

    println!("{}", "正在解密文件...".cyan());
    let password_str = super::prompt_password("请输入容器密码: ", password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 提取文件
    let manager = WorkspaceManager::new(workspace_path);
    let output = Path::new(output_path);

    manager.extract_file(file_name, output, password)?;

    println!("{}", format!("✓ 已提取文件: {}", file_name).green());
    println!("{}", format!("  输出: {}", output_path).bright_black());

    Ok(())
}
