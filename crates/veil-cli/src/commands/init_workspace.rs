use anyhow::Result;
use colored::Colorize;
use veil_core::config::GlobalConfig;
use veil_core::workspace::WorkspaceConfig;
use veil_core::workspace_ops::WorkspaceManager;
use std::path::PathBuf;

/// 创建新的加密容器（工作区架构）。
///
/// 创建一个工作区目录，初始化 .veil-meta 元数据文件。
/// 工作区中的文件独立加密，支持并发读写。
///
/// # 参数
/// - `container_name`: 容器名称（如 "photos"）
/// - `password`: 用户密码（`Option<String>`，`None` 则交互式输入）
/// - `workspace_name`: 工作区名称（`None` 使用默认工作区）
/// - `workspace_path`: 专属工作区路径（`None` 则非专属）
///
/// # 返回
/// - `Ok(())`: 容器创建成功
/// - `Err(anyhow::Error)`: 创建失败
///
/// # 示例
/// ```bash
/// # 使用默认工作区
/// veil init photos
///
/// # 使用自定义工作区
/// veil init project-a --workspace work
///
/// # 创建专属工作区
/// veil init secrets --workspace ~/EncryptedVolume/veil --dedicated
/// ```
pub fn run_workspace(
    container_name: &str,
    password: Option<String>,
    workspace_name: Option<&str>,
    workspace_path: Option<PathBuf>,
    dedicated: bool,
) -> Result<()> {
    // 加载全局配置
    let mut config = GlobalConfig::load()?;

    // 确保默认工作区配置存在
    if config.workspace.default.is_none() {
        config.workspace.default = Some(WorkspaceConfig::default_workspace()?);
    }

    // 检查容器是否已存在
    if config.containers.contains_key(container_name) {
        anyhow::bail!("容器 '{}' 已存在", container_name);
    }

    // 确定工作区路径
    let workspace_root = if let Some(path) = workspace_path {
        // 使用指定的路径
        path
    } else if let Some(name) = workspace_name {
        // 使用命名工作区
        if name == "default" {
            config.workspace.default.as_ref().unwrap().path.clone()
        } else {
            config
                .workspace
                .custom
                .get(name)
                .ok_or_else(|| anyhow::anyhow!("工作区 '{}' 不存在", name))?
                .path
                .clone()
        }
    } else {
        // 使用默认工作区
        config.workspace.default.as_ref().unwrap().path.clone()
    };

    // 确定容器目录路径
    let container_path = if dedicated {
        // 专属工作区：直接使用工作区根目录
        workspace_root.clone()
    } else {
        // 共享工作区：在工作区下创建子目录
        workspace_root.join(container_name)
    };

    // 检查目录是否已存在
    if container_path.exists() {
        anyhow::bail!("目录已存在: {}", container_path.display());
    }

    println!("{}", "正在创建加密工作区...".cyan());
    let password_str = super::prompt_new_password(password)?;

    // 使用 SecretString 的 expose_secret() 获取字符串
    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 创建工作区管理器并初始化
    let manager = WorkspaceManager::new(container_path.clone());
    let workspace_type = if dedicated {
        "dedicated"
    } else if workspace_name == Some("default") || workspace_name.is_none() {
        "default"
    } else {
        "custom"
    };

    manager.init_container(container_name, workspace_type, password)?;

    // 更新全局配置
    use veil_core::config::ContainerConfig;
    let container_config = if dedicated {
        ContainerConfig {
            workspace: None,
            container_dir: None,
            workspace_path: Some(workspace_root),
            dedicated: true,
            created_at: chrono::Utc::now().to_rfc3339(),
            last_accessed: None,
        }
    } else {
        ContainerConfig {
            workspace: Some(workspace_name.unwrap_or("default").to_string()),
            container_dir: Some(container_name.to_string()),
            workspace_path: None,
            dedicated: false,
            created_at: chrono::Utc::now().to_rfc3339(),
            last_accessed: None,
        }
    };

    config.containers.insert(container_name.to_string(), container_config);
    config.save()?;

    println!("{}", format!("✓ 已创建容器 '{}'", container_name).green());
    println!("{}", format!("  工作区: {}", container_path.display()).bright_black());

    if dedicated {
        println!("{}", "  类型: 专属工作区".bright_black());
    } else {
        let ws_name = workspace_name.unwrap_or("default");
        println!("{}", format!("  类型: 共享工作区 ({})", ws_name).bright_black());
    }

    Ok(())
}
