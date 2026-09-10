use anyhow::Result;
use colored::Colorize;
use std::path::{Path, PathBuf};
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
pub fn run(
    target: &str,
    password: Option<String>,
    link_output: Option<&str>,
    workspace_name: Option<&str>,
    workspace_path: Option<PathBuf>,
    dedicated: bool,
) -> Result<()> {
    let target_path = Path::new(target);
    let container_name = if target_path.extension().and_then(|ext| ext.to_str())
        == Some("veil-link")
    {
        target_path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow::anyhow!("{}", crate::i18n::t("init.invalid_link_name")))?
            .to_string()
    } else if target_path
        .parent()
        .is_some_and(|parent| !parent.as_os_str().is_empty())
    {
        target_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow::anyhow!("{}", crate::i18n::t("init.invalid_container_name")))?
            .to_string()
    } else {
        target.to_string()
    };

    let link_path = link_output.map(PathBuf::from).unwrap_or_else(|| {
        if target_path.extension().and_then(|ext| ext.to_str()) == Some("veil-link") {
            target_path.to_path_buf()
        } else {
            super::default_link_path(&container_name)
        }
    });
    if link_path.exists() {
        anyhow::bail!(
            "{}",
            crate::i18n::t1(
                "link.output_exists",
                "path",
                &link_path.display().to_string()
            )
        );
    }

    // 加载全局配置
    let mut config = GlobalConfig::load()?;

    // 确保默认工作区配置存在
    if config.workspace.default.is_none() {
        config.workspace.default = Some(WorkspaceConfig::default_workspace()?);
    }

    // 检查容器是否已存在
    if config.containers.contains_key(&container_name) {
        anyhow::bail!(
            "{}",
            crate::i18n::t1("init.exists", "path", &container_name)
        );
    }

    // 确定工作区路径
    let workspace_root = if let Some(path) = workspace_path {
        path
    } else if let Some(name) = workspace_name {
        if name == "default" {
            config.workspace.default.as_ref().unwrap().path.clone()
        } else {
            config
                .workspace
                .custom
                .get(name)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "{}",
                        crate::i18n::t1("unpack.workspace_not_found", "name", name)
                    )
                })?
                .path
                .clone()
        }
    } else {
        config.workspace.default.as_ref().unwrap().path.clone()
    };

    let container_path = if dedicated {
        workspace_root.clone()
    } else {
        workspace_root.join(&container_name)
    };

    // 检查目录是否已存在
    if container_path.exists() {
        anyhow::bail!(
            "{}",
            crate::i18n::t1("init.exists", "path", &container_path.display().to_string())
        );
    }

    println!("{}", crate::i18n::t("init.creating").cyan());
    let password_str = super::prompt_new_password(password)?;

    // 使用 SecretString 的 expose_secret() 获取字符串
    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 创建工作区管理器并初始化
    let workspace_type = if dedicated {
        "dedicated"
    } else if workspace_name == Some("default") || workspace_name.is_none() {
        "default"
    } else {
        "custom"
    };

    let manager = WorkspaceManager::new(container_path.clone());
    manager.init_container(&container_name, workspace_type, password)?;

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
            links: Vec::new(),
        }
    } else {
        ContainerConfig {
            workspace: Some(workspace_name.unwrap_or("default").to_string()),
            container_dir: Some(container_name.clone()),
            workspace_path: None,
            dedicated: false,
            created_at: chrono::Utc::now().to_rfc3339(),
            last_accessed: None,
            links: Vec::new(),
        }
    };

    config
        .containers
        .insert(container_name.clone(), container_config);
    config.save()?;
    config.register_link(&container_name, &link_path)?;

    println!(
        "{}",
        crate::i18n::t1("init.created", "path", &container_name).green()
    );
    println!(
        "{}",
        crate::i18n::t1(
            "init.link_created",
            "path",
            &link_path.display().to_string()
        )
        .green()
    );
    println!(
        "{}",
        crate::i18n::t1(
            "init.workspace_path",
            "path",
            &container_path.display().to_string()
        )
        .bright_black()
    );

    if dedicated {
        println!("{}", crate::i18n::t("init.type_dedicated").bright_black());
    } else {
        let ws_name = workspace_name.unwrap_or("default");
        println!(
            "{}",
            crate::i18n::t1("init.type_shared", "workspace", ws_name).bright_black()
        );
    }

    // 显示首次使用提示
    crate::hints::show_first_init_hint(&container_name, &link_path);

    Ok(())
}
