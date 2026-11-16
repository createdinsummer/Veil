//! `veil passwd` 子命令：修改工作区容器密码。
//! 所属模块：CLI密码管理。核心run协调旧/新密码输入与CliContainer改密；依赖SecretString、容器定位和服务改密，目标是正确区分普通保护层切换与完整轮换。
//!
//! 普通模式用 SQLCipher 原生改密能力，只修改数据库保护层，独立文件的密钥与密文
//! 保持不变；`--full` 模式持久化一把 active 新文件密钥和一把 retiring 旧文件密钥，
//! 逐文件持写租约流式重写密文，全部迁移后再切换数据库密码并移除旧文件密钥。

use crate::error::Result;
use colored::Colorize;

/// 收集旧/新密码并请求普通改密或完整文件密钥轮换。
/// 入参：veil_name（&str）为容器选择器；old_password/new_password（`Option<String>`）为显式旧/新密码；full（bool）决定是否重加密全部文件。
/// 返回值：服务成功后显示改密完成并返回Ok(())。
/// 异常：定位、密码输入/确认、解锁/锁/改密/轮换或数据库错误返回CommandError。
/// 注意事项：旧密码回退VEIL_PASSWORD，新密码回退VEIL_NEW_PASSWORD；参数和环境跳过确认。完整轮换分阶段提交，中断不意味着密码和文件均未改变。
pub fn run(
    veil_name: &str,
    old_password: Option<String>,
    new_password: Option<String>,
    full: bool,
) -> Result<()> {
    // 密码修改必须作用于容器工作区，而不是链接文件本身。
    let resolved = super::resolve_veil(veil_name)?;
    let manager = super::container_client(&resolved);

    crate::outln!("{}", crate::i18n::t("passwd.changing").cyan());

    // 旧密码用于验证身份并解密现有元数据。
    let old_pwd = super::prompt_password(crate::i18n::t("prompt.current_password"), old_password)?;

    // 新密码用于数据库密码保护；完整模式另行生成新的文件数据密钥并迁移引用。
    // 新密码走独立的 VEIL_NEW_PASSWORD 环境变量，避免把旧密码静默复用为新密码。
    let new_pwd = super::prompt_new_password_with_env(new_password, "VEIL_NEW_PASSWORD")?;

    use secrecy::ExposeSecret;
    let old_password_str = old_pwd.expose_secret();
    let new_password_str = new_pwd.expose_secret();

    // core 在修改过程中保持原始 veil_id 和工作区结构不变。
    // 补充：--full 中断后可用必要密码继续，最多保留两把文件密钥。
    manager.change_password(old_password_str, new_password_str, full)?;

    crate::outln!("{}", crate::i18n::t("passwd.changed").green());

    Ok(())
}
