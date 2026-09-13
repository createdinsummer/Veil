//! `veil unpack` 子命令：把 `.veil` 包还原为工作区容器。

use crate::error::Result;
use colored::Colorize;
use std::fs;
use std::path::{Path, PathBuf};
use veil_core::config::{ContainerConfig, GlobalConfig};
use veil_core::container_format::ContainerUnpacker;
use veil_core::metadata::MetaHeader;
use veil_core::workspace::{WorkspaceConfig, allocate_container_directory};

/// 读取打包文件，验证密码后重建容器、工作区和链接。
pub fn run_workspace(
    container_path: &str,
    container_name: Option<&str>,
    workspace_name: Option<&str>,
    link_output: Option<&str>,
    password: Option<String>,
) -> Result<()> {
    if !Path::new(container_path).exists() {
        crate::cli_bail!(UnpackContainerNotFound, "path" => container_path);
    }

    let name = if let Some(name) = container_name {
        name.to_string()
    } else {
        let stem = Path::new(container_path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| crate::cli_error!(UnpackNameExtractFailed))?;
        stem.strip_suffix(".vault").unwrap_or(stem).to_string()
    };
    if name.trim().is_empty() {
        crate::cli_bail!(UnpackNameExtractFailed);
    }

    let mut config = GlobalConfig::load()?;
    if config.workspace.default.is_none() {
        config.workspace.default = Some(WorkspaceConfig::default_workspace()?);
    }

    let workspace_root = if let Some(workspace_name) = workspace_name {
        if workspace_name == "default" {
            config.workspace.default.as_ref().unwrap().path.clone()
        } else {
            config
                .workspace
                .custom
                .get(workspace_name)
                .ok_or_else(
                    || crate::cli_error!(UnpackWorkspaceNotFound, "name" => workspace_name),
                )?
                .path
                .clone()
        }
    } else {
        config.workspace.default.as_ref().unwrap().path.clone()
    };
    let workspace_root = absolute_path(&workspace_root)?;

    let unpacker = ContainerUnpacker::new(container_path);
    let encrypted_metadata = unpacker.read_encrypted_metadata()?;
    let header = MetaHeader::from_bytes(&encrypted_metadata)?;
    if header.veil_id.is_empty() {
        crate::cli_bail!(UnpackMissingVeilId);
    }
    config.ensure_container_id_available(&header.veil_id)?;

    let creation_time = super::creation_timestamp();
    let link_path = link_output
        .map(PathBuf::from)
        .unwrap_or_else(|| super::default_link_path_for_time(&name, &creation_time));
    if link_path
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("veil-link")
    {
        crate::cli_bail!(UnpackInvalidLinkExtension, "path" => link_path.display());
    }
    if link_path.exists() {
        crate::cli_bail!(LinkOutputExists, "path" => link_path.display());
    }

    let container_dir =
        allocate_container_directory(&workspace_root, &header.veil_id, &creation_time);
    if container_dir.exists() {
        crate::cli_bail!(UnpackDirectoryExists, "path" => container_dir.display());
    }

    crate::outln!("{}", crate::i18n::t("unpack.in_progress").cyan());
    let password_str =
        super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 密码验证和元数据解密必须先于任何目录或文件创建。
    let (metadata, encrypted_metadata) = unpacker.read_metadata(password)?;
    if metadata.veil_id != header.veil_id {
        return Err(veil_core::error::VeilError::InvalidFormat(
            "打包文件身份与元数据不一致".to_string(),
        )
        .into());
    }

    let workspace_root_existed = workspace_root.exists();
    let transaction = (|| -> Result<()> {
        unpacker.unpack_encrypted_files(&container_dir, &metadata)?;

        let meta_path = container_dir.join(".veil-meta");
        veil_core::temp::write_private_file_atomic(&meta_path, &encrypted_metadata)?;

        let container_config = ContainerConfig {
            veil_id: metadata.veil_id.clone(),
            container_name: name.clone(),
            workspace: Some(workspace_name.unwrap_or("default").to_string()),
            container_dir: container_dir
                .file_name()
                .and_then(|directory| directory.to_str())
                .map(ToOwned::to_owned),
            workspace_path: None,
            dedicated: false,
            created_at: chrono::Utc::now().to_rfc3339(),
            last_accessed: None,
            links: Vec::new(),
        };
        config.register_container(container_config)?;

        // 注册链接会写入链接文件并保存完整配置。
        config.register_link_at(&metadata.veil_id, &name, &container_dir, &link_path)?;
        Ok(())
    })();

    if let Err(error) = transaction {
        cleanup_failed_unpack(
            &container_dir,
            workspace_root_existed,
            &workspace_root,
            &link_path,
        );
        return Err(error);
    }

    crate::outln!(
        "{}",
        crate::i18n::t1("unpack.created", "name", &name).green()
    );
    crate::outln!(
        "{}",
        crate::i18n::t2(
            "unpack.stats",
            "count",
            &metadata.files.len().to_string(),
            "path",
            &container_dir.display().to_string()
        )
        .bright_black()
    );

    crate::hints::show_unpack_explain_hint(&name, &link_path, metadata.files.len());
    Ok(())
}

/// 将相对工作区路径固定到当前目录。
fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

/// 解包失败时移除新建容器、链接和空工作区根目录。
fn cleanup_failed_unpack(
    container_dir: &Path,
    workspace_root_existed: bool,
    workspace_root: &Path,
    link_path: &Path,
) {
    let _ = fs::remove_file(link_path);
    let _ = fs::remove_dir_all(container_dir);
    if !workspace_root_existed {
        let _ = fs::remove_dir(workspace_root);
    }
}
