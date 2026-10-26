//! `veil init` 子命令：创建新的 Veil 和链接。

use crate::error::Result;
use colored::Colorize;
use std::fs;
use std::path::{Path, PathBuf};
use veil_core::config::GlobalConfig;
use veil_core::metadata::VEIL_INFO_FILE;
use veil_core::veil_ops::VeilManager;

/// 创建新的加密 Veil。
///
/// 创建 `veil_dir`，初始化 `.veil-info` 元数据文件，并登记全局配置。
/// Veil 中的每个文件拥有独立 `.enc` 密文，身份由启动时生成的稳定 ID 表示。
///
/// # 参数
/// - `target`: Veil 名称、链接文件名或带目录的目标名称。
/// - `password`: 用户密码；`None` 时按命令层规则从环境变量或终端读取。
/// - `link_output`: 自定义 `.veil-link` 输出路径。
/// - `work_name`: 可选的已注册命名工作区名称，不是路径。
/// - `work_root`: 可选的显式工作区根路径，自动登记工作区并关联当前 Veil。
/// - `dedicated`: 是否让 `work_root` 指定的目录由当前 Veil 独占。
/// - `portable`: 是否强制把 Veil 目录放在链接文件所在卷。
///
/// # 返回
/// - `Ok(())`: Veil 创建成功
/// - `Err(CommandError)`: 名称、路径、密码、配置或磁盘写入失败。
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
    work_name: Option<&str>,
    work_root: Option<PathBuf>,
    dedicated: bool,
    portable: bool,
) -> Result<()> {
    // 工作区来源必须唯一，避免参数解析成功但其中一项被静默忽略。
    if work_name.is_some() && work_root.is_some() {
        crate::cli_bail!(InitConflictingWorkOptions);
    }
    if dedicated && work_root.is_none() {
        crate::cli_bail!(InitDedicatedRequiresWorkPath);
    }
    if portable && (work_name.is_some() || work_root.is_some()) {
        crate::cli_bail!(InitPortableConflictsWork);
    }

    let target_path = Path::new(target);
    // 链接目标使用文件名，目录目标使用最后一级名称，普通名称原样保留。
    let veil_name = if target_path.extension().and_then(|ext| ext.to_str()) == Some("veil-link") {
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
            .ok_or_else(|| crate::cli_error!(InitInvalidVeilName))?
            .to_string()
    } else {
        target.to_string()
    };
    if veil_name.trim().is_empty() {
        crate::cli_bail!(InitInvalidVeilName);
    }

    let veil_id = veil_core::metadata::generate_veil_id();
    let creation_time = super::creation_timestamp();
    let link_path = if let Some(path) = link_output {
        PathBuf::from(path)
    } else if target_path.extension().and_then(|ext| ext.to_str()) == Some("veil-link") {
        target_path.to_path_buf()
    } else {
        super::default_link_path_for_time(&veil_name, &creation_time)
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
    let (mut config, work) =
        GlobalConfig::creation_work(link_dir, work_name, work_root.as_deref(), dedicated)?;
    config.ensure_veil_id_available(&veil_id)?;
    let portable_mode = portable || veil_core::volume::volume_for_path(link_dir)?.if_out_volume;
    let work_root = config.resolve_work_root(&work)?;
    let work_root_existed = work_root.exists();
    let dedicated = matches!(work.work_type, veil_core::work::WorkType::Dedicated);
    if dedicated && work_root_existed && !is_empty_directory(&work_root)? {
        crate::cli_bail!(InitDedicatedPathNotEmpty, "path" => work_root.display());
    }
    let veil_dir = work_root.join(&veil_id);
    let veil_dir_existed = veil_dir.exists();
    if veil_dir_existed {
        crate::cli_bail!(InitVeilExists, "path" => veil_dir.display());
    }

    crate::outln!("{}", crate::i18n::t("init.creating").cyan());
    if portable_mode {
        let message_key = if portable {
            "init.portable_work"
        } else {
            "init.external_work"
        };
        crate::outln!(
            "{}",
            crate::i18n::t1(message_key, "path", &veil_dir.display().to_string()).cyan()
        );
    }
    let password_str = super::prompt_new_password(password)?;

    use secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    let manager = VeilManager::for_veil(veil_dir.clone(), veil_id.clone());
    use veil_core::config::VeilConfig;
    let creation_result = (|| -> Result<()> {
        let metadata = manager
            .init_veil_with_id(&veil_id, &veil_name, password)
            .map_err(|error| {
                crate::cli_error!(
                    InitWorkCreateFailed,
                    "path" => veil_dir.display(),
                    "error" => error
                )
            })?;

        let veil_config = VeilConfig {
            veil_id: metadata.veil_id.clone(),
            veil_name: veil_name.clone(),
            created_at: metadata.created_at.clone(),
            last_accessed: None,
            links: Vec::new(),
        };

        config.register_veil(veil_config)?;
        // register_link_at 将链接写入后再保存配置；任一步失败都可整体回滚。
        config
            .register_link_at(&metadata.veil_id, &veil_name, &veil_dir, &link_path)
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
            &veil_dir,
            veil_dir_existed,
            &work_root,
            work_root_existed,
            &link_path,
        );
        return Err(error);
    }

    crate::outln!(
        "{}",
        crate::i18n::t1("init.created", "path", &veil_name).green()
    );

    crate::hints::show_first_init_hint(&veil_name, &link_path, &veil_dir);

    Ok(())
}

/// 判断路径是否是可直接交给专属容器的空目录。
fn is_empty_directory(path: &Path) -> Result<bool> {
    if !path.is_dir() {
        return Ok(false);
    }

    Ok(fs::read_dir(path)?.next().transpose()?.is_none())
}

/// 初始化失败时移除本次创建的链接、元数据和 `veil_dir`。
///
/// 已存在且为空的专属目录不能整体删除，只清理本次初始化可能写入的元数据文件。
fn cleanup_failed_init(
    veil_dir: &Path,
    veil_dir_existed: bool,
    work_root: &Path,
    work_root_existed: bool,
    link_path: &Path,
) {
    let _ = fs::remove_file(link_path);

    if veil_dir_existed {
        let _ = fs::remove_file(veil_dir.join(VEIL_INFO_FILE));
    } else {
        let _ = fs::remove_dir_all(veil_dir);
    }

    // 共享工作区根目录如果是本次创建且仍为空，也一并删除，避免失败后留下空目录。
    if !work_root_existed && work_root != veil_dir {
        let _ = fs::remove_dir(work_root);
    }
}
