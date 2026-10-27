//! `veil unpack` 子命令：接收参数并调用解包服务。

use crate::error::Result;
use colored::Colorize;
use veil_service::workflow::UnpackOptions;
use veil_service::{ServiceRequest, ServiceResponse};

/// 读取密码并请求服务还原打包容器。
pub fn run(
    package_path: &str,
    veil_name: Option<&str>,
    work_name: Option<&str>,
    link_output: Option<&str>,
    password: Option<String>,
) -> Result<()> {
    let plan = match super::request(ServiceRequest::PrepareUnpack {
        options: UnpackOptions {
            package_path,
            name: veil_name,
            work_name,
            link_output,
        },
    })? {
        ServiceResponse::UnpackPrepared(plan) => plan,
        _ => {
            return Err(
                veil_core::error::VeilError::WorkError("服务响应类型不符合请求".into()).into(),
            );
        }
    };
    crate::outln!("{}", crate::i18n::t("unpack.in_progress").cyan());
    let password = super::prompt_password(crate::i18n::t("prompt.veil_password"), password)?;
    use secrecy::ExposeSecret;
    let unpacked = match super::request(ServiceRequest::CommitUnpack {
        plan,
        password: password.expose_secret(),
    })? {
        ServiceResponse::Unpacked(unpacked) => unpacked,
        _ => {
            return Err(
                veil_core::error::VeilError::WorkError("服务响应类型不符合请求".into()).into(),
            );
        }
    };
    crate::outln!(
        "{}",
        crate::i18n::t1("unpack.created", "name", &unpacked.name).green()
    );
    crate::outln!(
        "{}",
        crate::i18n::t2(
            "unpack.stats",
            "count",
            &unpacked.file_count.to_string(),
            "path",
            &unpacked.directory.display().to_string()
        )
        .bright_black()
    );
    crate::hints::show_unpack_explain_hint(
        &unpacked.name,
        &unpacked.link_path,
        unpacked.file_count,
    );
    Ok(())
}
