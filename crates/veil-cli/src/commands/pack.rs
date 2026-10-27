//! `veil pack` 子命令：把 `veil_dir` 打包为单文件 `.veil`。

use crate::error::Result;
use colored::Colorize;
use veil_service::{ServiceRequest, ServiceResponse};

/// 将 Veil 元数据和所有加密文件封装为可分享的 `.veil` 文件。
///
/// 输出已存在时拒绝覆盖；未指定路径时使用 `<veil-name>.vault.veil`。
///
/// # 错误
/// Veil 解析、输出路径检查、密码读取、元数据读取或打包写入失败时返回错误。
pub fn run(veil_name: &str, output_path: Option<&str>, password: Option<String>) -> Result<()> {
    let plan = match super::request(ServiceRequest::PrepareContainerPack {
        selector: veil_name,
        output: output_path,
    })? {
        ServiceResponse::ContainerPackPrepared(plan) => plan,
        _ => {
            return Err(
                veil_core::error::VeilError::WorkError("服务响应类型不符合请求".into()).into(),
            );
        }
    };
    if let Some(path) = plan.recovered_link() {
        crate::hints::show_link_recovery_hint(path);
    }

    crate::outln!("{}", crate::i18n::t("pack.in_progress").cyan());
    let password_str = super::prompt_password(crate::i18n::t("prompt.veil_password"), password)?;

    use secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    let result = match super::request(ServiceRequest::CommitContainerPack { plan, password })? {
        ServiceResponse::ContainerPacked(result) => result,
        _ => {
            return Err(
                veil_core::error::VeilError::WorkError("服务响应类型不符合请求".into()).into(),
            );
        }
    };
    let output = result.output.to_string_lossy();
    let file_count = result.file_count;
    let output_size = result.size;

    crate::outln!(
        "{}",
        crate::i18n::t1("pack.created", "path", &output).green()
    );
    crate::outln!(
        "{}",
        crate::i18n::t2(
            "pack.stats",
            "count",
            &file_count.to_string(),
            "size",
            &format_size(output_size)
        )
        .bright_black()
    );

    crate::hints::show_pack_explain_hint(
        &result.name,
        result.link_path.as_deref(),
        &output,
        output_size,
    );

    Ok(())
}

/// 将字节数格式化为 B、KB、MB 或 GB，并保留两位小数。
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}
