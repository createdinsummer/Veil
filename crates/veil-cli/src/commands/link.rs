use crate::i18n;
use anyhow::Result;
use colored::Colorize;
use std::path::{Path, PathBuf};
use veil_core::config::GlobalConfig;
use veil_core::link::VeilLink;
use veil_core::volume;
use veil_core::workspace_ops::WorkspaceManager;

pub fn run(target: &str, output: Option<&str>) -> Result<()> {
    let mut config = GlobalConfig::load()?;
    let resolved = super::resolve_container(target)?;
    let output_path = output
        .map(PathBuf::from)
        .unwrap_or_else(|| super::default_link_path(&resolved.name));

    if output_path.exists() {
        anyhow::bail!(
            "{}",
            i18n::t1(
                "link.output_exists",
                "path",
                &output_path.display().to_string()
            )
        );
    }

    if let Some(existing_link) = resolved.link_path.as_deref() {
        std::fs::copy(existing_link, &output_path)?;
        config.cache_link(&output_path)?;
    } else if config.find_container_key(&resolved.name).is_some() {
        config.register_link(&resolved.name, &output_path)?;
    } else {
        let manager = WorkspaceManager::new(resolved.workspace_path.clone());
        let header = manager.read_meta_header()?;
        let container_name = if header.container_name.is_empty() {
            resolved.name.clone()
        } else {
            header.container_name.clone()
        };
        if header.veil_id.is_empty() {
            anyhow::bail!(".veil-meta 缺少 veil_id");
        }
        let veil_id = header.veil_id.clone();
        let volume = volume::volume_for_path(&resolved.workspace_path)?;
        let relative_path = resolved
            .workspace_path
            .strip_prefix(&volume.mount_path)
            .unwrap_or(&resolved.workspace_path)
            .to_path_buf();
        VeilLink::new(
            veil_id,
            container_name,
            relative_path,
            volume.volume_id,
            volume.volume_label,
        )
        .save(Path::new(&output_path))?;
        config.cache_link(&output_path)?;
    }

    println!(
        "{}",
        i18n::t1("link.created", "path", &output_path.display().to_string()).green()
    );

    Ok(())
}
