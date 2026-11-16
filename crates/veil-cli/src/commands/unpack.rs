//! `veil unpack` 子命令：接收参数并调用解包服务。
//! 所属模块：CLI命令适配。核心run协调准备与还原服务；依赖密码输入、UnpackOptions和提示，目标是认证后恢复既有身份容器。
//!
//! 在新设备验证密码与容器身份后还原打包容器：校验数据库身份、密文集合与全部分块
//! 认证后发布目录并登记，生成新链接。重复 ID、目标目录冲突和链接冲突均明确报错。

use crate::error::Result;
use colored::Colorize;
use veil_service::workflow::UnpackOptions;
use veil_service::{ServiceRequest, ServiceResponse};

/// 准备还原位置后用密码验证备份，发布容器目录、登记并显示结果。
/// 入参：package_path为包文件；veil_name（`Option<&str>`）为新展示名，None从包名推导；work_name为可选空间ID/名称；link_output为可选新链接；password为可选显式包密码。
/// 返回值：还原、登记、链接完成并展示后Ok(())。
/// 异常：输入/包外层/密码/身份/认证/目标冲突、文件/登记或协议失败转CommandError。
/// 注意事项：保持包内容器ID，不生成独立新身份；计划阶段可初始化应用配置，发布成功后登记失败保留已验证数据，不保证全流程回滚。
pub fn run(
    package_path: &str,
    veil_name: Option<&str>,
    work_name: Option<&str>,
    link_output: Option<&str>,
    password: Option<String>,
) -> Result<()> {
    // 先准备解包计划，尚未还原容器或写新链接；配置选择可能初始化应用库/空间目录。
    let plan = match super::request(ServiceRequest::PrepareUnpack {
        options: UnpackOptions {
            package_path,
            name: veil_name,
            work_name,
            link_output,
        },
    })? {
        ServiceResponse::UnpackPrepared(plan) => plan,
        // 响应类型与请求不匹配属于服务协议错误。
        _ => {
            return Err(
                veil_core::error::VeilError::WorkError("服务响应类型不符合请求".into()).into(),
            );
        }
    };
    crate::outln!("{}", crate::i18n::t("unpack.in_progress").cyan());
    // 提交时需要密码：验证密码与容器身份，校验密文集合与分块认证。
    let password = super::prompt_password(crate::i18n::t("prompt.veil_password"), password)?;
    use secrecy::ExposeSecret;
    let unpacked = match super::request(ServiceRequest::CommitUnpack {
        plan,
        password: password.expose_secret(),
    })? {
        ServiceResponse::Unpacked(unpacked) => unpacked,
        // 响应类型与请求不匹配属于服务协议错误。
        _ => {
            return Err(
                veil_core::error::VeilError::WorkError("服务响应类型不符合请求".into()).into(),
            );
        }
    };
    // 解包成功：输出容器名、文件数量与发布目录。
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
    // 首次解包展示新链接与包文件对应关系的说明提示。
    crate::hints::show_unpack_explain_hint(
        &unpacked.name,
        &unpacked.link_path,
        unpacked.file_count,
    );
    Ok(())
}
