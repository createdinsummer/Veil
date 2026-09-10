use anyhow::Result;
use colored::Colorize;
use veil_core::config::GlobalConfig;
use veil_core::workspace::WorkspaceConfig;
use veil_core::workspace_ops::WorkspaceManager;

/// 创建新的加密容器（工作区架构）。
///
/// 创建一个工作区目录，初始化 .veil-meta 元数据文件。
/// 工作区中的文件独立加密，支持并发读写。
///
/// # 参数
/// - `container_name`: 容器名称（如 "photos"）
/// - `password`: 用户密码（`Option<String>`，`None` 则交互式输入）
///
/// # 返回
/// - `Ok(())`: 容器创建成功
/// - `Err(anyhow::Error)`: 创建失败
///
/// # 示例
/// ```bash
/// # 交互式输入密码
/// veil init photos
///
/// # 命令行提供密码
/// veil init photos mypassword
/// ```
pub fn run(container_name: &str, password: Option<String>) -> Result<()> {
    // 加载全局配置
    let mut config = GlobalConfig::load()?;

    // 确保默认工作区配置存在
    if config.workspace.default.is_none() {
        config.workspace.default = Some(WorkspaceConfig::default_workspace()?);
    }

    // 检查容器是否已存在
    if config.containers.contains_key(container_name) {
        anyhow::bail!("{}", crate::i18n::t1("init.exists", "path", container_name));
    }

    // 使用默认工作区
    let workspace_root = config.workspace.default.as_ref().unwrap().path.clone();
    let container_path = workspace_root.join(container_name);

    // 检查目录是否已存在
    if container_path.exists() {
        anyhow::bail!("{}", crate::i18n::t1("init.exists", "path", &container_path.display().to_string()));
    }

    println!("{}", crate::i18n::t("init.creating").cyan());
    let password_str = super::prompt_new_password(password)?;

    // 使用 SecretString 的 expose_secret() 获取字符串
    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 创建工作区管理器并初始化
    let manager = WorkspaceManager::new(container_path.clone());
    manager.init_container(container_name, "default", password)?;

    // 更新全局配置
    use veil_core::config::ContainerConfig;
    let container_config = ContainerConfig {
        workspace: Some("default".to_string()),
        container_dir: Some(container_name.to_string()),
        workspace_path: None,
        dedicated: false,
        created_at: chrono::Utc::now().to_rfc3339(),
        last_accessed: None,
    };

    config.containers.insert(container_name.to_string(), container_config);
    config.save()?;

    println!("{}", crate::i18n::t1("init.created", "path", container_name).green());
    println!("{}", format!("  工作区: {}", container_path.display()).bright_black());

    Ok(())
}
