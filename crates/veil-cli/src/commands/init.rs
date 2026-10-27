//! `veil init` 子命令：接收参数并调用容器创建服务。

use crate::error::Result;
use colored::Colorize;
use std::path::PathBuf;
use veil_service::workflow::CreateOptions;
use veil_service::{ServiceRequest, ServiceResponse};

/// 创建容器并展示结果。
pub fn run(
    target: &str,
    password: Option<String>,
    link_output: Option<&str>,
    work_name: Option<&str>,
    work_root: Option<PathBuf>,
    dedicated: bool,
    portable: bool,
) -> Result<()> {
    let plan = match super::request(ServiceRequest::PrepareCreate {
        options: CreateOptions {
            target,
            link_output,
            work_name,
            work_root: work_root.as_deref(),
            dedicated,
            portable,
        },
    })? {
        ServiceResponse::CreatePrepared(plan) => plan,
        _ => {
            return Err(
                veil_core::error::VeilError::WorkError("服务响应类型不符合请求".into()).into(),
            );
        }
    };

    crate::outln!("{}", crate::i18n::t("init.creating").cyan());
    if plan.portable_mode() {
        let message_key = if portable {
            "init.portable_work"
        } else {
            "init.external_work"
        };
        crate::outln!(
            "{}",
            crate::i18n::t1(message_key, "path", &plan.directory().display().to_string()).cyan()
        );
    }
    let password = super::prompt_new_password(password)?;
    use secrecy::ExposeSecret;
    let created = match super::request(ServiceRequest::CommitCreate {
        plan,
        password: password.expose_secret(),
    })? {
        ServiceResponse::Created(created) => created,
        _ => {
            return Err(
                veil_core::error::VeilError::WorkError("服务响应类型不符合请求".into()).into(),
            );
        }
    };
    crate::outln!(
        "{}",
        crate::i18n::t1("init.created", "path", &created.name).green()
    );
    crate::hints::show_first_init_hint(&created.name, &created.link_path, &created.directory);
    Ok(())
}
