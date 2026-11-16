//! `veil link` 子命令：接收参数并调用链接服务。
//! 所属模块：CLI命令适配。核心run协调定位、必要的密码登记与链接计划；依赖服务和终端提示，目标是为已知容器建立入口。
//!
//! 为容器生成便携入口文件（.veil-link）。已有登记及身份缓存足够时无需输入密码；
//! 目标为容器目录且缓存不足时，通过目录和密码解锁重新登记，再恢复本次访问目标的
//! 完整结构化缓存正文。

use crate::error::Result;
use crate::i18n;
use colored::Colorize;
use std::path::Path;
use veil_service::{ServiceRequest, ServiceResponse};

/// 为已有容器创建不覆盖的便携链接入口。
/// 入参：target（&str）为名称/ID/目录/既有链接；output（`Option<&str>`）为新链接路径，None按服务默认规则。
/// 返回值：计划提交生成链接并展示路径后Ok(())。
/// 异常：定位、目录密码输入/登记、链接路径冲突、文件/缓存或响应协议错误转CommandError。
/// 注意事项：目录目标首次ResolveContainer任意错误都会触发密码登记回退，不仅缓存缺失；准备本身可能恢复旧链接。新链接和缓存不在同一个原子事务。
pub fn run(target: &str, output: Option<&str>) -> Result<()> {
    // 目标是目录时先尝试用本机登记缓存解析，避免不必要的密码询问。
    if Path::new(target).is_dir() {
        let cached = super::request(ServiceRequest::ResolveContainer { selector: target });
        if cached.is_err() {
            // 缓存不足：用目录与密码重新登记，恢复本次访问目标的完整缓存正文。
            use secrecy::ExposeSecret;
            let password = super::prompt_password(i18n::t("password.prompt"), None)?;
            super::request(ServiceRequest::RegisterContainer {
                directory: Path::new(target),
                password: password.expose_secret(),
            })?;
        }
    }
    // 两步式：先准备链接计划（解析目标、校验输出路径与扩展名），再提交生成。
    let plan = match super::request(ServiceRequest::PrepareLink {
        target,
        output: output.map(Path::new),
    })? {
        ServiceResponse::LinkPrepared(plan) => plan,
        // 响应类型与请求不匹配属于服务协议错误。
        _ => {
            return Err(
                veil_core::error::VeilError::WorkError("服务响应类型不符合请求".into()).into(),
            );
        }
    };
    // 链接可能是从配置缓存恢复的，提醒用户缓存正文已写回。
    if let Some(path) = plan.recovered_link() {
        crate::hints::show_link_recovery_hint(path);
    }
    // 提交计划：真正写出链接文件。
    let path = match super::request(ServiceRequest::CommitLink { plan })? {
        ServiceResponse::LinkCreated(path) => path,
        // 响应类型与请求不匹配属于服务协议错误。
        _ => {
            return Err(
                veil_core::error::VeilError::WorkError("服务响应类型不符合请求".into()).into(),
            );
        }
    };
    // 输出最终生成的链接路径。
    crate::outln!(
        "{}",
        i18n::t1("link.created", "path", &path.display().to_string()).green()
    );
    Ok(())
}
