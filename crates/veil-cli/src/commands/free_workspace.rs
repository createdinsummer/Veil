use anyhow::Result;
use colored::Colorize;
use veil_core::config::GlobalConfig;

/// 删除容器
pub fn run_workspace(
    container_name: &str,
    force: bool,
) -> Result<()> {
    // 加载配置
    let mut config = GlobalConfig::load()?;

    // 检查容器是否存在
    if !config.containers.contains_key(container_name) {
        anyhow::bail!("容器 '{}' 不存在", container_name);
    }

    // 获取容器工作区路径
    let workspace_path = config.get_container_workspace_path(container_name)?;

    // 如果不是强制删除，需要确认
    if !force {
        println!("{}", format!("⚠️  将删除容器 '{}' 及其所有文件", container_name).yellow());
        println!("{}", format!("   路径: {}", workspace_path.display()).bright_black());
        print!("{}", "确认删除? (y/N): ".yellow());

        use std::io::{self, Write};
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        let input = input.trim().to_lowercase();
        if input != "y" && input != "yes" {
            println!("{}", "已取消".bright_black());
            return Ok(());
        }
    }

    println!("{}", format!("正在删除容器 '{}'...", container_name).cyan());

    // 删除目录
    if workspace_path.exists() {
        std::fs::remove_dir_all(&workspace_path)?;
    }

    // 从配置中移除
    config.containers.remove(container_name);
    config.save()?;

    println!("{}", format!("✓ 容器已删除: {}", container_name).green());

    Ok(())
}
