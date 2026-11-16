//! `veil init` 子命令：接收参数并调用容器创建服务。
//! 所属模块：CLI容器创建。核心run组装CreateOptions并提交计划；依赖服务工作流、密码输入、颜色及首次提示，目标是统一创建和展示入口。
//!
//! 业务流程分两步：先用 PrepareCreate 让服务层校验名称、链接路径与工作区选项并
//! 生成创建计划（配置选择仍可能写应用库/空间）；获取新密码后，CommitCreate 才创建容器、
//! 生成 veil_dir、写入配置记录并输出 .veil-link 链接文件。

use crate::error::Result;
use colored::Colorize;
use std::path::PathBuf;
use veil_service::workflow::CreateOptions;
use veil_service::{ServiceRequest, ServiceResponse};

/// 准备新容器计划、取得新密码后提交创建，并展示首次使用说明。
/// 入参：target（&str）为名称/链接目标；password（`Option<String>`）为显式密码，None用环境/TTY；link_output为可选链接路径；work_name为可选空间选择器；work_root（`Option<PathBuf>`）为显式根目录；dedicated为专属目录标志；portable为便携提示标志。
/// 返回值：创建及登记完成、结果显示后Ok(())。
/// 异常：选项冲突、名称/路径占用、密码确认、初始化/登记/链接或响应协议错误返回CommandError。
/// 注意事项：准备不写容器/链接，但配置选择可能建应用库/空间；portable标志当前不另改提交位置。初始化后登记失败可能保留容器，输出成功提示不检查额外文件持久性。
pub fn run(
    target: &str,
    password: Option<String>,
    link_output: Option<&str>,
    work_name: Option<&str>,
    work_root: Option<PathBuf>,
    dedicated: bool,
    portable: bool,
) -> Result<()> {
    // 先解析目标、工作区与链接计划，尚未写入容器/链接；配置加载仍可能初始化本机应用数据。
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
        // 响应类型与请求不匹配属于服务协议错误。
        _ => {
            return Err(
                veil_core::error::VeilError::WorkError("服务响应类型不符合请求".into()).into(),
            );
        }
    };

    crate::outln!("{}", crate::i18n::t("init.creating").cyan());
    // 计划为便携/外部工作区时，提前告知用户工作区（veil_dir 所在目录）将落在哪里。
    if plan.portable_mode() {
        // 显式 --portable 与自动判定为便携的提示文案不同。
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
    // 第二步：获取新密码——参数/环境变量直接通过校验，交互模式要求输入两次确认。
    let password = super::prompt_new_password(password)?;
    use secrecy::ExposeSecret;
    // 第三步：提交创建——服务层生成容器、veil_dir、本机登记与链接文件。
    let created = match super::request(ServiceRequest::CommitCreate {
        plan,
        password: password.expose_secret(),
    })? {
        ServiceResponse::Created(created) => created,
        // 响应类型与请求不匹配属于服务协议错误。
        _ => {
            return Err(
                veil_core::error::VeilError::WorkError("服务响应类型不符合请求".into()).into(),
            );
        }
    };
    // 创建成功后输出容器名，并展示链接、工作区与打包解包的使用说明。
    crate::outln!(
        "{}",
        crate::i18n::t1("init.created", "path", &created.name).green()
    );
    crate::hints::show_first_init_hint(&created.name, &created.link_path, &created.directory);
    Ok(())
}
