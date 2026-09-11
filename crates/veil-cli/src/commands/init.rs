//! `veil init` 子命令：创建新的工作区容器和链接。

use anyhow::Result;
use colored::Colorize;
use std::path::{Path, PathBuf};
use veil_core::config::GlobalConfig;
use veil_core::workspace::WorkspaceConfig;
use veil_core::workspace_ops::WorkspaceManager;

/// 创建新的加密容器（工作区架构）。
///
/// 创建一个工作区目录，初始化 `.veil-meta` 元数据文件，并登记全局配置。
/// 工作区中的每个文件拥有独立密文，容器身份由启动时生成的稳定 ID 表示。
///
/// # 参数
/// - `target`: 容器名称、链接文件名或工作区目标路径。
/// - `password`: 用户密码；`None` 时按命令层规则从环境变量或终端读取。
/// - `link_output`: 自定义 `.veil-link` 输出路径。
/// - `workspace_name`: 可选的命名工作区。
/// - `workspace_path`: 可选的显式工作区根路径。
/// - `dedicated`: 是否让新工作区由当前容器独占。
/// - `portable`: 是否强制把工作区放在链接文件所在卷。
///
/// # 返回
/// - `Ok(())`: 容器创建成功
/// - `Err(anyhow::Error)`: 名称、路径、密码、配置或容器写入失败。
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
    // 链接目标使用文件名，目录目标使用最后一级名称，普通名称原样保留。
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

    let veil_id = veil_core::metadata::generate_veil_id();
    let creation_time = super::creation_timestamp();
    let link_path = if let Some(path) = link_output {
        PathBuf::from(path)
    } else if target_path.extension().and_then(|ext| ext.to_str()) == Some("veil-link") {
        target_path.to_path_buf()
    } else {
        super::default_link_path_for_time(&container_name, &creation_time)
    };
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
    // 外部卷或显式 portable 参数会自动把默认工作区放到链接所在卷。
    let portable_mode =
        workspace_name.is_none() && workspace_path.is_none() && (portable || external_location);

    let mut config = GlobalConfig::load()?;

    if config.workspace.default.is_none() {
        config.workspace.default = Some(WorkspaceConfig::default_workspace()?);
    }

    let workspace_root = if portable_mode {
        // 便携模式固定使用链接目录下的隐藏工作区，保证链接与数据一起移动。
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

    // 共享工作区以 veil_id 作为目录名，专属工作区则直接使用 workspace_root。
    let container_dir = (!dedicated).then(|| veil_id.clone());
    let container_path = if dedicated {
        workspace_root.clone()
    } else {
        workspace_root.join(&veil_id)
    };

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

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    let workspace_type = if dedicated {
        "dedicated"
    } else if workspace_name == Some("default") || workspace_name.is_none() {
        "default"
    } else {
        "custom"
    };

    let manager = WorkspaceManager::new(container_path.clone());
    let metadata =
        manager.init_container_with_id(&veil_id, &container_name, workspace_type, password)?;

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
            container_dir,
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
    // 注册链接时会再次写入配置，确保容器与链接两侧都能独立恢复。
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

    crate::hints::show_first_init_hint(&container_name, &link_path, &container_path);

    Ok(())
}

/// 判断路径是否位于当前平台识别出的外部卷。
///
/// 路径不存在时使用最近的已存在祖先进行探测；无法取得设备信息时返回 `false`。
fn is_external_volume(path: &Path) -> bool {
    // 目标目录可能尚未创建，因此先找到最近的真实路径再读取卷信息。
    let probe = nearest_existing_path(path);
    if probe.as_os_str().is_empty() {
        return false;
    }

    #[cfg(target_os = "macos")]
    {
        // macOS 约定 /Volumes 下是挂载卷。
        if probe.starts_with("/Volumes") {
            return true;
        }
    }

    #[cfg(target_os = "windows")]
    {
        use std::path::Component;

        /// 提取路径盘符并转换为字符串。
        fn root_key(path: &Path) -> Option<String> {
            match path.components().next()? {
                Component::Prefix(prefix) => {
                    Some(prefix.as_os_str().to_string_lossy().into_owned())
                }
                _ => None,
            }
        }

        // 比较目标盘符与用户主目录盘符，不同盘即视为外部位置。
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

        // Unix 设备号变化表示跨文件系统，通常对应外部挂载卷。
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

/// 返回路径自身或最近的已存在祖先；没有任何可用路径时返回空路径。
fn nearest_existing_path(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    loop {
        // 返回第一个存在路径，并尽量 canonicalize 去掉符号链接和相对段。
        if current.exists() {
            return std::fs::canonicalize(&current).unwrap_or(current);
        }
        // pop 到根后仍不存在，说明调用方给出的是无效路径。
        if !current.pop() {
            return PathBuf::new();
        }
    }
}
