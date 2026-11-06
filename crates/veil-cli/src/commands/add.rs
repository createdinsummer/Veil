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

fn decide_conflict(
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
        let detail = if conflict.is_directory {
            "目标是目录，不支持覆盖"
        } else if conflict.same_content {
            "内容相同"
        } else {
            "内容不同"
        };
        eprintln!("目标已存在：{}（{}）", conflict.target, detail);
        eprint!("选择 [o]覆盖/[s]跳过/[a]后续全部覆盖/[k]后续全部跳过/[q]取消：");
        io::stderr().flush()?;
        let mut input = String::new();
        if io::stdin().read_line(&mut input)? == 0 {
            return Err(VeilError::Cancelled);
        }
        match input.trim() {
            "o" if !conflict.is_directory => return Ok(ImportDecision::Overwrite),
            "s" => return Ok(ImportDecision::Skip),
            "a" if !conflict.is_directory => {
                *policy = ConflictPolicy::Overwrite;
                return Ok(ImportDecision::Overwrite);
            }
            "k" => {
                *policy = ConflictPolicy::Skip;
                return Ok(ImportDecision::Skip);
            }
            "q" => return Err(VeilError::Cancelled),
            _ => eprintln!("请输入有效选项；目录不能覆盖。"),
        }
    }
}

use veil_service::{ServiceRequest, ServiceResponse};

pub fn run(
    veil_name: &str,
    source: &str,
    dest: Option<&str>,
    password: Option<String>,
    mut conflict_policy: ConflictPolicy,
) -> Result<()> {
    let plan = match super::request(ServiceRequest::PrepareImport {
        selector: veil_name,
        source: Path::new(source),
        destination: dest,
    })? {
        ServiceResponse::ImportPrepared(plan) => plan,
        _ => {
            return Err(
                veil_core::error::VeilError::WorkError("服务响应类型不符合请求".into()).into(),
            );
        }
    };
    if let Some(path) = plan.recovered_link() {
        crate::hints::show_link_recovery_hint(path);
    }
    if plan.is_empty() {
        crate::outln!("{}", crate::i18n::t("add.empty_dir").yellow());
        return Ok(());
    }
    crate::outln!("{}", crate::i18n::t("opening_veil").cyan());
    let password = super::prompt_password(crate::i18n::t("prompt.veil_password"), password)?;
    use secrecy::ExposeSecret;
    let plan = plan.resolve_conflicts(password.expose_secret(), |conflict| {
        decide_conflict(conflict, &mut conflict_policy)
    })?;
    let result = match super::request(ServiceRequest::CommitImport {
        plan,
        password: password.expose_secret(),
    })? {
        ServiceResponse::Imported(result) => result,
        _ => {
            return Err(
                veil_core::error::VeilError::WorkError("服务响应类型不符合请求".into()).into(),
            );
        }
    };
    if result.skipped > 0 {
        crate::outln!("已跳过 {} 个文件", result.skipped);
    }
    if result.targets.is_empty() {
        return Ok(());
    }
    if result.targets.len() == 1 {
        crate::outln!(
            "{}",
            crate::i18n::t1("add.file_added", "path", &result.targets[0]).green()
        );
        crate::outln!(
            "{}",
            crate::i18n::t1("common.encrypted_name", "name", &result.encrypted_names[0])
                .bright_black()
        );
    } else {
        crate::outln!(
            "{}",
            crate::i18n::t1(
                "add.files_added",
                "count",
                &result.encrypted_names.len().to_string()
            )
            .green()
        );
    }
    Ok(())
}
