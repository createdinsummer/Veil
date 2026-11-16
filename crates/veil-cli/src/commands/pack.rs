//! `veil pack` 子命令：把 `veil_dir` 打包为单文件 `.veil`。
//! 所属模块：CLI命令适配。核心run协调服务备份计划，format_size显示包容量；依赖服务、密码输入、提示和本地化，目标是备份与反馈。
//!
//! 在内容读租约下导出原始加密数据库字节及引用密文，不重写正文；包内数据库可能含运行锁，
//! 还原验证时才清除副本锁记录，源库不受该清除影响。适用于备份及迁移。

use crate::error::Result;
use colored::Colorize;
use veil_service::{ServiceRequest, ServiceResponse};

/// 准备备份计划、取得密码并生成不覆盖的加密容器包，展示统计和说明。
/// 入参：veil_name（&str）为容器选择器；output_path（`Option<&str>`）为输出，None取容器名加.vault.veil；password（`Option<String>`）为显式密码，None尝试环境/TTY。
/// 返回值：打包及输出大小读取成功后Ok(())。
/// 异常：定位/输出冲突、密码/读租约/数据库/包写入/统计或响应协议错误返回CommandError。
/// 注意事项：不重新认证全部文件内容；发布或同步后失败可能仍生成包。原始数据库段可能含运行锁，解包验证时清除副本锁，不修改源容器锁。
pub fn run(veil_name: &str, output_path: Option<&str>, password: Option<String>) -> Result<()> {
    // 两步式：先准备打包计划（解析容器、校验输出路径是否已存在）。
    let plan = match super::request(ServiceRequest::PrepareContainerPack {
        selector: veil_name,
        output: output_path,
    })? {
        ServiceResponse::ContainerPackPrepared(plan) => plan,
        // 响应类型与请求不匹配属于服务协议错误。
        _ => {
            return Err(
                veil_core::error::VeilError::WorkError("服务响应类型不符合请求".into()).into(),
            );
        }
    };
    // 从链接或容器名触发时可能恢复过链接，提示用户缓存正文已写回。
    if let Some(path) = plan.recovered_link() {
        crate::hints::show_link_recovery_hint(path);
    }

    crate::outln!("{}", crate::i18n::t("pack.in_progress").cyan());
    // 打包需要密码以读取加密数据库的一致快照。
    let password_str = super::prompt_password(crate::i18n::t("prompt.veil_password"), password)?;

    use secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 提交打包：服务层输出加密数据库快照与所引用密文组成的 .veil 文件。
    let result = match super::request(ServiceRequest::CommitContainerPack { plan, password })? {
        ServiceResponse::ContainerPacked(result) => result,
        // 响应类型与请求不匹配属于服务协议错误。
        _ => {
            return Err(
                veil_core::error::VeilError::WorkError("服务响应类型不符合请求".into()).into(),
            );
        }
    };
    // 统计信息：输出路径、文件数量与包文件实际字节数。
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

    // 首次打包展示链接与 .veil 文件关系的说明提示。
    crate::hints::show_pack_explain_hint(
        &result.name,
        result.link_path.as_deref(),
        &output,
        output_size,
    );

    Ok(())
}

/// 将字节数换算成便于终端阅读的容量文本。
/// 入参：bytes（u64）为文件或包逻辑字节数。
/// 返回值：String：小于1024用整数B，其余按1024次幂显示KB/MB/GB并保留两位小数。
/// 异常：无Result返回。
/// 注意事项：标签使用KB但计算为二进制1024；超过GB仍用GB，f64转换在极大整数时可能舍入。
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    // 先匹配达到的最大单位阈值，再以1024次幂换算；不改变原始统计值。
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
