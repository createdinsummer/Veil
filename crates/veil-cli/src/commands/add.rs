//! `veil add` 子命令：接收输入并调用文件导入服务。

use crate::error::Result;
use colored::Colorize;
use std::path::Path;
use veil_service::{ServiceRequest, ServiceResponse};

pub fn run(
    veil_name: &str,
    source: &str,
    dest: Option<&str>,
    password: Option<String>,
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
