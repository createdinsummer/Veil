use crate::i18n;
use anyhow::Result;
use colored::Colorize;
use std::path::{Path, PathBuf};
use veil_core::config::GlobalConfig;
use veil_core::link::{portable_workspace_path, VeilLink};

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
        if config.containers.contains_key(&resolved.name) {
            config.register_link(&resolved.name, &output_path)?;
        }
    } else if config.containers.contains_key(&resolved.name) {
        config.register_link(&resolved.name, &output_path)?;
    } else {
        VeilLink::new(
            &resolved.name,
            portable_workspace_path(&resolved.workspace_path),
            "dedicated",
            false,
        )
        .save(Path::new(&output_path))?;
    }

    println!(
        "{}",
        i18n::t1("link.created", "path", &output_path.display().to_string()).green()
    );

    Ok(())
}
