//! `veil unpack` 子命令：把 `.veil` 包还原为 Work 中的 Veil。

use crate::error::Result;
use colored::Colorize;
use std::fs;
use std::path::{Path, PathBuf};
use veil_core::config::{GlobalConfig, VeilConfig};
use veil_core::metadata::MetaHeader;
use veil_core::veil_package::VeilUnpacker;
use veil_core::work::{WorkConfig, allocate_veil_directory};

/// 读取打包文件，验证密码后重建 `veil_dir`、配置记录和链接。
pub fn run(
    package_path: &str,
    veil_name: Option<&str>,
    work_name: Option<&str>,
    link_output: Option<&str>,
    password: Option<String>,
) -> Result<()> {
    if !Path::new(package_path).exists() {
        crate::cli_bail!(UnpackVeilNotFound, "path" => package_path);
    }

    let name = if let Some(name) = veil_name {
        name.to_string()
    } else {
        let stem = Path::new(package_path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| crate::cli_error!(UnpackNameExtractFailed))?;
        stem.strip_suffix(".vault").unwrap_or(stem).to_string()
    };
    if name.trim().is_empty() {
        crate::cli_bail!(UnpackNameExtractFailed);
    }

    let mut config = GlobalConfig::load()?;
    if config.work.default.is_none() {
        config.work.default = Some(WorkConfig::default_work()?);
    }

    let work_root = if let Some(work_name) = work_name {
        if work_name == "default" {
            config.work.default.as_ref().unwrap().path.clone()
        } else {
            config
                .work
                .custom
                .get(work_name)
                .ok_or_else(|| crate::cli_error!(UnpackWorkNotFound, "name" => work_name))?
                .path
                .clone()
        }
    } else {
        config.work.default.as_ref().unwrap().path.clone()
    };
    let work_root = absolute_path(&work_root)?;

    let unpacker = VeilUnpacker::new(package_path);
    let encrypted_metadata = unpacker.read_encrypted_metadata()?;
    let header = MetaHeader::from_bytes(&encrypted_metadata)?;
    if header.veil_id.is_empty() {
        crate::cli_bail!(UnpackMissingVeilId);
    }
    config.ensure_veil_id_available(&header.veil_id)?;

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

    let veil_dir = allocate_veil_directory(&work_root, &header.veil_id, &creation_time);
    if veil_dir.exists() {
        crate::cli_bail!(UnpackDirectoryExists, "path" => veil_dir.display());
    }

    crate::outln!("{}", crate::i18n::t("unpack.in_progress").cyan());
    let password_str = super::prompt_password(crate::i18n::t("prompt.veil_password"), password)?;

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

    let work_root_existed = work_root.exists();
    let transaction = (|| -> Result<()> {
        unpacker.unpack_encrypted_files(&veil_dir, &metadata)?;

        let meta_path = veil_dir.join(".veil-meta");
        veil_core::temp::write_private_file_atomic(&meta_path, &encrypted_metadata)?;

        let veil_config = VeilConfig {
            veil_id: metadata.veil_id.clone(),
            veil_name: name.clone(),
            work: Some(work_name.unwrap_or("default").to_string()),
            veil_dir_name: veil_dir
                .file_name()
                .and_then(|directory| directory.to_str())
                .map(ToOwned::to_owned),
            veil_dir: None,
            dedicated_work: false,
            created_at: chrono::Utc::now().to_rfc3339(),
            last_accessed: None,
            links: Vec::new(),
        };
        config.register_veil(veil_config)?;

        // 注册链接会写入链接文件并保存完整配置。
        config.register_link_at(&metadata.veil_id, &name, &veil_dir, &link_path)?;
        Ok(())
    })();

    if let Err(error) = transaction {
        cleanup_failed_unpack(&veil_dir, work_root_existed, &work_root, &link_path);
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
            &veil_dir.display().to_string()
        )
        .bright_black()
    );

    crate::hints::show_unpack_explain_hint(&name, &link_path, metadata.files.len());
    Ok(())
}

/// 将相对 Work 根路径固定到当前目录。
fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

/// 解包失败时移除新建 `veil_dir`、链接和空 `work_root`。
fn cleanup_failed_unpack(
    veil_dir: &Path,
    work_root_existed: bool,
    work_root: &Path,
    link_path: &Path,
) {
    let _ = fs::remove_file(link_path);
    let _ = fs::remove_dir_all(veil_dir);
    if !work_root_existed {
        let _ = fs::remove_dir(work_root);
    }
}
