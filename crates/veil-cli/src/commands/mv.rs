//! `veil mv` 子命令：移动或重命名容器内的文件。
//! 所属模块：CLI逻辑树调整。核心run提交源/目标路径并显示实际落点；依赖服务move_path、密码输入和本地化，目标是在不搬动密文的情况下移动或改名节点。
//!
//! 业务流程：只修改 veil.db 中目录树节点的父关系或名称，磁盘上的随机加密文件名
//! 与密文内容都不动；目标是已有文件时拒绝覆盖，已有目录则移入其下，目录不能移入自身后代。

use crate::error::Result;
use colored::Colorize;

/// 移动或重命名容器内部节点并显示实际落点。
/// 入参：veil_name为选择器；from/to（&str）为内部源/目标；password为可选显式密码。
/// 返回值：数据库操作成功后显示最终路径并返回Ok(())。
/// 异常：定位/密码/路径/租约冲突、成环、同级名称唯一约束或数据库错误返回CommandError。
/// 注意事项：目标是目录则移入其下，目标为源自身可无操作成功；密文不搬动，不追加内容版本。提交失败可能不确定，不应承诺所有Err都完全未改动。
pub fn run(veil_name: &str, from: &str, to: &str, password: Option<String>) -> Result<()> {
    // mv 只改变元数据中的原始路径，不复制或重新加密 blob。
    let resolved = super::resolve_veil(veil_name)?;
    let manager = super::container_client(&resolved);

    let password_str = super::prompt_password(crate::i18n::t("prompt.veil_password"), password)?;

    use secrecy::ExposeSecret;
    let pwd = password_str.expose_secret();

    // 尽早显示目标容器，方便用户确认链接解析结果。
    crate::outln!(
        "{}",
        crate::i18n::t1("mv.opening_named", "name", veil_name).cyan()
    );

    // 服务在短写事务中执行移动；成功后显示实际落点，提交错误不能统一理解为已经确认回滚。
    // 返回的第二项是规范化后的最终目标路径（可能带 ./ 前缀清理）。
    let (_, final_target) = manager.move_path(from, to, pwd)?;

    crate::outln!(
        "{}",
        crate::i18n::t2("mv.moved", "from", from, "to", &final_target).green()
    );

    Ok(())
}
