use anyhow::Result;
use colored::Colorize;
use veil_core::config::GlobalConfig;

/// 重命名容器
pub fn run_workspace(
    old_name: &str,
    new_name: &str,
) -> Result<()> {
    // 加载配置
    let mut config = GlobalConfig::load()?;

    // 检查旧容器是否存在
    if !config.containers.contains_key(old_name) {
        anyhow::bail!("容器 '{}' 不存在", old_name);
    }

    // 检查新名称是否已被使用
    if config.containers.contains_key(new_name) {
        anyhow::bail!("容器 '{}' 已存在", new_name);
    }

    // 获取容器配置
    let container_config = config.containers.get(old_name).unwrap().clone();
    let workspace_path = config.get_container_workspace_path(old_name)?;

    // 计算新路径
    let new_workspace_path = workspace_path.parent().unwrap().join(new_name);

    // 检查新目录是否已存在
    if new_workspace_path.exists() {
        anyhow::bail!("目录已存在: {}", new_workspace_path.display());
    }

    println!("{}", format!("正在重命名容器 '{}' -> '{}'...", old_name, new_name).cyan());

    // 重命名目录
    std::fs::rename(&workspace_path, &new_workspace_path)?;

    // 更新配置
    config.containers.remove(old_name);

    let mut new_config = container_config;
    if let Some(ref mut dir) = new_config.container_dir {
        *dir = new_name.to_string();
    }

    config.containers.insert(new_name.to_string(), new_config);
    config.save()?;

    println!("{}", format!("✓ 容器已重命名: {} -> {}", old_name, new_name).green());

    Ok(())
}
