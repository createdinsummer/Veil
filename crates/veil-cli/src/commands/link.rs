//! `veil link` 子命令：接收参数并调用链接服务。

use crate::error::Result;
use crate::i18n;
use colored::Colorize;
use std::path::Path;
use veil_service::{ServiceRequest, ServiceResponse};

/// 为容器生成链接并展示结果。
pub fn run(target: &str, output: Option<&str>) -> Result<()> {
    if Path::new(target).is_dir() {
        let cached = super::request(ServiceRequest::ResolveContainer { selector: target });
        if cached.is_err() {
            use secrecy::ExposeSecret;
            let password = super::prompt_password(i18n::t("password.prompt"), None)?;
            super::request(ServiceRequest::RegisterContainer {
                directory: Path::new(target),
                password: password.expose_secret(),
            })?;
        }
    }
    let plan = match super::request(ServiceRequest::PrepareLink {
        target,
        output: output.map(Path::new),
    })? {
        ServiceResponse::LinkPrepared(plan) => plan,
        _ => {
            return Err(
                veil_core::error::VeilError::WorkError("服务响应类型不符合请求".into()).into(),
            );
        }
    };
    if let Some(path) = plan.recovered_link() {
        crate::hints::show_link_recovery_hint(path);
    }
    let path = match super::request(ServiceRequest::CommitLink { plan })? {
        ServiceResponse::LinkCreated(path) => path,
        _ => {
            return Err(
                veil_core::error::VeilError::WorkError("服务响应类型不符合请求".into()).into(),
            );
        }
    };
    crate::outln!(
        "{}",
        i18n::t1("link.created", "path", &path.display().to_string()).green()
    );
    Ok(())
}
