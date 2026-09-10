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
    portable: bool,
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

    let absolute_link_path = if link_path.is_absolute() {
        link_path.clone()
    } else {
        std::env::current_dir()?.join(&link_path)
    };
    let link_dir = absolute_link_path
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let external_location = is_external_volume(link_dir);
    let portable_mode =
        workspace_name.is_none() && workspace_path.is_none() && (portable || external_location);

    // 加载全局配置
    let mut config = GlobalConfig::load()?;

    // 确保默认工作区配置存在
    if config.workspace.default.is_none() {
        config.workspace.default = Some(WorkspaceConfig::default_workspace()?);
    }

    // 确定工作区路径
    let workspace_root = if portable_mode {
        link_dir.join(".veil/workspaces/default")
    } else if let Some(path) = workspace_path {
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
    if portable_mode {
        let message_key = if portable {
            "init.portable_workspace"
        } else {
            "init.external_workspace"
        };
        println!(
            "{}",
            crate::i18n::t1(message_key, "path", &container_path.display().to_string()).cyan()
        );
    }
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
    let metadata = manager.init_container(&container_name, workspace_type, password)?;

    // 更新全局配置
    use veil_core::config::ContainerConfig;
    let container_config = if dedicated {
        ContainerConfig {
            veil_id: metadata.veil_id.clone(),
            container_name: container_name.clone(),
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
            veil_id: metadata.veil_id.clone(),
            container_name: container_name.clone(),
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
        .insert(metadata.veil_id.clone(), container_config);
    config.save()?;
    config.register_link_at(
        &metadata.veil_id,
        &container_name,
        &container_path,
        &link_path,
    )?;

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

fn is_external_volume(path: &Path) -> bool {
    let probe = nearest_existing_path(path);
    if probe.as_os_str().is_empty() {
        return false;
    }

    #[cfg(target_os = "macos")]
    {
        if probe.starts_with("/Volumes") {
            return true;
        }
    }

    #[cfg(target_os = "windows")]
    {
        use std::path::Component;

        fn root_key(path: &Path) -> Option<String> {
            match path.components().next()? {
                Component::Prefix(prefix) => {
                    Some(prefix.as_os_str().to_string_lossy().into_owned())
                }
                _ => None,
            }
        }

        if let (Some(location), Some(home)) = (
            root_key(&probe),
            std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .as_deref()
                .and_then(root_key),
        ) {
            return !location.eq_ignore_ascii_case(&home);
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let location_device = std::fs::metadata(&probe).ok().map(|meta| meta.dev());
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let home_device = home
            .as_deref()
            .and_then(|path| std::fs::metadata(path).ok())
            .map(|meta| meta.dev());

        if let (Some(location), Some(home)) = (location_device, home_device) {
            return location != home;
        }
    }

    false
}

fn nearest_existing_path(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    loop {
        if current.exists() {
            return std::fs::canonicalize(&current).unwrap_or(current);
        }
        if !current.pop() {
            return PathBuf::new();
        }
    }
}
