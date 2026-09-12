//! `veil init` 子命令：创建新的工作区容器和链接。

use crate::error::Result;
use colored::Colorize;
use std::fs;
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
/// - `target`: 容器名称、链接文件名或带目录的目标名称。
/// - `password`: 用户密码；`None` 时按命令层规则从环境变量或终端读取。
/// - `link_output`: 自定义 `.veil-link` 输出路径。
/// - `workspace_name`: 可选的已注册命名工作区名称，不是路径。
/// - `workspace_path`: 可选的显式工作区根路径，自动登记到当前容器，不新增命名工作区。
/// - `dedicated`: 是否让 `workspace_path` 指定的目录由当前容器独占。
/// - `portable`: 是否强制把工作区放在链接文件所在卷。
///
/// # 返回
/// - `Ok(())`: 容器创建成功
/// - `Err(CommandError)`: 名称、路径、密码、配置或容器写入失败。
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
    // 工作区来源必须唯一，避免参数解析成功但其中一项被静默忽略。
    if workspace_name.is_some() && workspace_path.is_some() {
        crate::cli_bail!(InitConflictingWorkspaceOptions);
    }
    if dedicated && workspace_path.is_none() {
        crate::cli_bail!(InitDedicatedRequiresWorkspacePath);
    }
    if portable && (workspace_name.is_some() || workspace_path.is_some()) {
        crate::cli_bail!(InitPortableConflictsWorkspace);
    }

    let target_path = Path::new(target);
    // 链接目标使用文件名，目录目标使用最后一级名称，普通名称原样保留。
    let container_name =
        if target_path.extension().and_then(|ext| ext.to_str()) == Some("veil-link") {
            target_path
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or_else(|| crate::cli_error!(InitInvalidLinkName))?
                .to_string()
        } else if target_path
            .parent()
            .is_some_and(|parent| !parent.as_os_str().is_empty())
        {
            target_path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| crate::cli_error!(InitInvalidContainerName))?
                .to_string()
        } else {
            target.to_string()
        };
    if container_name.trim().is_empty() {
        crate::cli_bail!(InitInvalidContainerName);
    }

    let veil_id = veil_core::metadata::generate_veil_id();
    let creation_time = super::creation_timestamp();
    let link_path = if let Some(path) = link_output {
        PathBuf::from(path)
    } else if target_path.extension().and_then(|ext| ext.to_str()) == Some("veil-link") {
        target_path.to_path_buf()
    } else {
        super::default_link_path_for_time(&container_name, &creation_time)
    };
    if link_path.extension().and_then(|ext| ext.to_str()) != Some("veil-link") {
        crate::cli_bail!(InitInvalidLinkExtension, "path" => link_path.display());
    }
    if link_path.exists() {
        crate::cli_bail!(LinkOutputExists, "path" => link_path.display());
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
    let explicit_workspace_path = workspace_path.is_some();

    let mut config = GlobalConfig::load()?;
    config.ensure_container_id_available(&veil_id)?;

    if config.workspace.default.is_none() {
        config.workspace.default = Some(WorkspaceConfig::default_workspace()?);
    }

    let workspace_root = if portable_mode {
        // 便携模式固定使用链接目录下的隐藏工作区，保证链接与数据一起移动。
        link_dir.join(".veil/workspaces/default")
    } else if let Some(path) = workspace_path {
        // 显式路径直接使用，并把实际容器根路径登记到容器记录；它不会成为新的
        // workspace.custom 命名工作区。
        path
    } else if let Some(name) = workspace_name {
        // 命名工作区只查配置注册表；default 可在首次运行时自动补全，自定义名称
        // 当前没有 CLI 注册命令，必须预先写入配置。
        if name == "default" {
            config.workspace.default.as_ref().unwrap().path.clone()
        } else {
            config
                .workspace
                .custom
                .get(name)
                .ok_or_else(|| crate::cli_error!(InitWorkspaceNotFound, "name" => name))?
                .path
                .clone()
        }
    } else {
        config.workspace.default.as_ref().unwrap().path.clone()
    };
    let workspace_root = make_absolute(&workspace_root)?;
    let workspace_root_existed = workspace_root.exists();

    // 共享工作区以 veil_id 作为目录名，专属工作区则直接使用 workspace_root。
    let container_dir = (!dedicated).then(|| veil_id.clone());
    let container_path = if dedicated {
        workspace_root.clone()
    } else {
        workspace_root.join(&veil_id)
    };

    let container_path_existed = container_path.exists();
    if container_path_existed && (!dedicated || !is_empty_directory(&container_path)?) {
        let error = if dedicated {
            crate::cli_error!(InitDedicatedPathNotEmpty, "path" => container_path.display())
        } else {
            crate::cli_error!(InitContainerExists, "path" => container_path.display())
        };
        return Err(error);
    }

    crate::outln!("{}", crate::i18n::t("init.creating").cyan());
    if portable_mode {
        let message_key = if portable {
            "init.portable_workspace"
        } else {
            "init.external_workspace"
        };
        crate::outln!(
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

    let manager = WorkspaceManager::for_container(container_path.clone(), veil_id.clone());
    use veil_core::config::ContainerConfig;
    let creation_result = (|| -> Result<()> {
        let metadata = manager
            .init_container_with_id(&veil_id, &container_name, workspace_type, password)
            .map_err(|error| {
                crate::cli_error!(
                    InitWorkspaceCreateFailed,
                    "path" => container_path.display(),
                    "error" => error
                )
            })?;

        let container_config = if dedicated {
            ContainerConfig {
                veil_id: metadata.veil_id.clone(),
                container_name: container_name.clone(),
                workspace: None,
                container_dir: None,
                workspace_path: Some(workspace_root.clone()),
                dedicated: true,
                created_at: chrono::Utc::now().to_rfc3339(),
                last_accessed: None,
                links: Vec::new(),
            }
        } else {
            // 显式路径和便携工作区不来自命名工作区，必须保存实际容器根路径供
            // 链接丢失后的配置恢复使用。
            let resolved_workspace_path =
                (explicit_workspace_path || portable_mode).then(|| container_path.clone());
            ContainerConfig {
                veil_id: metadata.veil_id.clone(),
                container_name: container_name.clone(),
                workspace: Some(workspace_name.unwrap_or("default").to_string()),
                container_dir,
                workspace_path: resolved_workspace_path,
                dedicated: false,
                created_at: chrono::Utc::now().to_rfc3339(),
                last_accessed: None,
                links: Vec::new(),
            }
        };

        config.register_container(container_config)?;
        // register_link_at 将链接写入后再保存配置；任一步失败都可整体回滚。
        config
            .register_link_at(
                &metadata.veil_id,
                &container_name,
                &container_path,
                &link_path,
            )
            .map_err(|error| {
                crate::cli_error!(
                    InitLinkRegistrationFailed,
                    "path" => link_path.display(),
                    "error" => error
                )
            })?;
        Ok(())
    })();

    if let Err(error) = creation_result {
        cleanup_failed_init(
            &container_path,
            container_path_existed,
            &workspace_root,
            workspace_root_existed,
            &link_path,
        );
        return Err(error);
    }

    crate::outln!(
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

/// 将用户提供的相对工作区路径固定到当前目录。
fn make_absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

/// 判断路径是否是可直接交给专属容器的空目录。
fn is_empty_directory(path: &Path) -> Result<bool> {
    if !path.is_dir() {
        return Ok(false);
    }

    Ok(fs::read_dir(path)?.next().is_none())
}

/// 初始化失败时移除本次创建的链接、元数据和容器目录。
///
/// 已存在且为空的专属目录不能整体删除，只清理本次初始化可能写入的元数据文件。
fn cleanup_failed_init(
    container_path: &Path,
    container_path_existed: bool,
    workspace_root: &Path,
    workspace_root_existed: bool,
    link_path: &Path,
) {
    let _ = fs::remove_file(link_path);

    if container_path_existed {
        let _ = fs::remove_file(container_path.join(".veil-meta"));
    } else {
        let _ = fs::remove_dir_all(container_path);
    }

    // 共享工作区根目录如果是本次创建且仍为空，也一并删除，避免失败后留下空目录。
    if !workspace_root_existed && workspace_root != container_path {
        let _ = fs::remove_dir(workspace_root);
    }
}
