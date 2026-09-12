//! `veil link` 子命令：为容器创建新的便携链接。

use crate::error::Result;
use crate::i18n;
use colored::Colorize;
use std::path::{Path, PathBuf};
use veil_core::config::GlobalConfig;
use veil_core::link::VeilLink;
use veil_core::volume;

/// 根据现有链接、配置记录或 `.veil-meta` 生成新的 `.veil-link`。
///
/// 已解析链接存在时直接复制其文件；已注册但没有现有链接时重新注册；仅能定位到
/// 工作区时会读取明文头部身份并构造新链接。
///
/// # 参数
/// - `target`：容器名、工作区路径或现有链接路径。
/// - `output`：输出链接路径；省略时使用当前目录下的默认文件名。
///
/// # 错误
/// 输出已存在、链接复制、配置读取、元数据读取、链接生成或缓存写入失败时返回错误。
pub fn run(target: &str, output: Option<&str>) -> Result<()> {
    let mut config = GlobalConfig::load()?;
    let resolved = super::resolve_container(target)?;
    if resolved.veil_id.is_empty() {
        crate::cli_bail!(LinkMissingVeilId);
    }
    let manager = super::workspace_manager(&resolved);
    let output_path = output
        .map(PathBuf::from)
        .unwrap_or_else(|| super::default_link_path(&resolved.name));

    if output_path
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("veil-link")
    {
        crate::cli_bail!(LinkInvalidExtension, "path" => output_path.display());
    }
    if output_path.exists() {
        crate::cli_bail!(LinkOutputExists, "path" => output_path.display());
    }

    let creation = (|| -> Result<()> {
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if let Some(existing_link) = resolved.link_path.as_deref() {
            // 已有链接直接逐字节复制，避免重新序列化改变内容。
            std::fs::copy(existing_link, &output_path)?;
            config.cache_link(&output_path)?;
        } else if config.find_container_key(&resolved.veil_id).is_some() {
            // 容器已注册但没有现有链接时，由配置模型生成新链接。
            config.register_link(&resolved.veil_id, &output_path)?;
        } else {
            // 未注册的脱离工作区也使用解析阶段确认过的稳定 ID。
            let volume = volume::volume_for_path(&manager.workspace_path)?;
            let relative_path = manager
                .workspace_path
                .strip_prefix(&volume.mount_path)
                .unwrap_or(&manager.workspace_path)
                .to_path_buf();
            VeilLink::new(
                resolved.veil_id.clone(),
                resolved.name.clone(),
                relative_path,
                volume.volume_id,
                volume.volume_label,
            )
            .save(Path::new(&output_path))?;
            config.cache_link(&output_path)?;
        }
        Ok(())
    })();

    if let Err(error) = creation {
        let _ = std::fs::remove_file(&output_path);
        return Err(error);
    }

    crate::outln!(
        "{}",
        i18n::t1("link.created", "path", &output_path.display().to_string()).green()
    );

    Ok(())
}
