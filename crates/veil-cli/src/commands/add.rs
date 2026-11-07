//! `veil add` 子命令：接收输入并调用文件导入服务。

use crate::error::Result;
use colored::Colorize;
use std::{
    io::{self, IsTerminal, Write},
    path::Path,
};
use veil_service::commands::{ImportConflict, ImportDecision};

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum ConflictPolicy {
    Ask,
    Error,
    Skip,
    Overwrite,
}

pub(super) fn decide_conflict(
    conflict: &ImportConflict,
    policy: &mut ConflictPolicy,
) -> veil_core::error::Result<ImportDecision> {
    use veil_core::error::VeilError;
    match policy {
        ConflictPolicy::Error => return Ok(ImportDecision::Error),
        ConflictPolicy::Skip => return Ok(ImportDecision::Skip),
        ConflictPolicy::Overwrite => return Ok(ImportDecision::Overwrite),
        ConflictPolicy::Ask => {}
    }
    if !io::stdin().is_terminal() {
        return Err(VeilError::WorkError(format!(
            "目标路径已存在：{}；非交互环境请指定 --on-conflict error|skip|overwrite",
            conflict.target
        )));
    }
    loop {
        let can_overwrite = !conflict.is_directory && conflict.error.is_none();
        let detail = if let Some(error) = conflict.error.as_deref() {
            error
        } else if conflict.is_directory {
            "目标是目录，不支持覆盖"
        } else if conflict.same_content {
            "内容相同"
        } else {
            "内容不同"
        };
        eprintln!("\n目标已存在：{}（{}）", conflict.target, detail);
        eprint!("选择 [o]覆盖/[s]跳过/[a]后续全部覆盖/[k]后续全部跳过/[q]取消：");
        io::stderr().flush()?;
        let mut input = String::new();
        if io::stdin().read_line(&mut input)? == 0 {
            return Err(VeilError::Cancelled);
        }
        match input.trim() {
            "o" if can_overwrite => return Ok(ImportDecision::Overwrite),
            "s" => return Ok(ImportDecision::Skip),
            "a" if can_overwrite => {
                *policy = ConflictPolicy::Overwrite;
                return Ok(ImportDecision::Overwrite);
            }
            "k" => {
                *policy = ConflictPolicy::Skip;
                return Ok(ImportDecision::Skip);
            }
            "q" => return Err(VeilError::Cancelled),
            _ => eprintln!("请输入有效选项；该目标可能只能跳过或取消。"),
        }
    }
}

pub fn run(
    veil_name: &str,
    source: &str,
    dest: Option<&str>,
    password: Option<String>,
    mut conflict_policy: ConflictPolicy,
) -> Result<()> {
    let container = super::resolve_veil(veil_name)?;
    crate::outln!("{}", crate::i18n::t("opening_veil").cyan());
    let password = super::prompt_password(crate::i18n::t("prompt.veil_password"), password)?;
    use secrecy::ExposeSecret;
    let result = container
        .stream_import(
            Path::new(source),
            dest,
            password.expose_secret(),
            |conflict| decide_conflict(conflict, &mut conflict_policy),
            super::transfer::progress,
        )
        .map_err(super::map_service_error)?;
    if result.total_files == 0 {
        crate::outln!("{}", crate::i18n::t("add.empty_dir").yellow());
        return Ok(());
    }
    if result.skipped_files > 0 {
        crate::outln!("已跳过 {} 个文件", result.skipped_files);
    }
    let imported = result.completed_files - result.skipped_files;
    if imported == 0 {
        return Ok(());
    }
    if let Some(target) = result.target {
        crate::outln!(
            "{}",
            crate::i18n::t1("add.file_added", "path", &target).green()
        );
        if let Some(name) = result.encrypted_name {
            crate::outln!(
                "{}",
                crate::i18n::t1("common.encrypted_name", "name", &name).bright_black()
            );
        }
    } else {
        crate::outln!(
            "{}",
            crate::i18n::t1("add.files_added", "count", &imported.to_string()).green()
        );
    }
    Ok(())
}
