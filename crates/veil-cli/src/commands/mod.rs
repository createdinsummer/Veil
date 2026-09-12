//! Veil 子命令实现及命令层公共辅助函数。
//!
//! 本模块把全局配置解析、密码输入、链接路径生成和提示展示组合到各子命令中。
//! 命令实现只负责用户交互与流程编排，容器加密、元数据和文件访问由
//! `veil-core` 提供。

pub mod add;
pub mod config;
pub mod ex;
pub mod exists_workspace;
pub mod info;
pub mod init;
pub mod link;
pub mod rm;

// 工作区架构命令。
pub mod free_workspace;
pub mod list_workspace;
pub mod mv_workspace;
pub mod pack_workspace;
pub mod passwd_workspace;
pub mod shell_workspace;
pub mod unpack_workspace;

use std::path::PathBuf;
use veil_core::config::{GlobalConfig, ResolvedContainer};
use veil_core::workspace_ops::WorkspaceManager;

use crate::error::Result;

/// 所有需要选择已有容器的命令都必须经过这里。
///
/// 普通命令支持显式 `.veil-link` 路径、唯一容器名称和唯一 `veil_id`。工作区目录
/// 仅由 [`resolve_link_target`] 用于恢复链接。无论输入形式如何，返回前都会读取
/// `.veil-meta` 确认真实 `veil_id`。
///
/// 从配置缓存恢复链接后会展示恢复提示。
///
/// # 错误
/// 全局配置加载或 [`GlobalConfig::resolve_container`] 失败时返回错误。
pub fn resolve_container(input: &str) -> Result<ResolvedContainer> {
    let mut config = GlobalConfig::load()?;
    let resolved = config.resolve_container(input)?;

    // 已恢复的链接需要提示用户。
    if resolved.recovered_link
        && let Some(link_path) = resolved.link_path.as_deref()
    {
        crate::hints::show_link_recovery_hint(link_path);
    }

    Ok(resolved)
}

/// 解析 `veil link` 的目标，支持容器名、ID、链接路径和工作区目录。
pub fn resolve_link_target(input: &str) -> Result<ResolvedContainer> {
    let mut config = GlobalConfig::load()?;
    let resolved = config.resolve_link_target(input)?;

    if resolved.recovered_link
        && let Some(link_path) = resolved.link_path.as_deref()
    {
        crate::hints::show_link_recovery_hint(link_path);
    }

    Ok(resolved)
}

/// 为解析结果创建工作区管理器，并绑定已经确认的稳定容器 ID。
pub fn workspace_manager(resolved: &ResolvedContainer) -> WorkspaceManager {
    WorkspaceManager::for_container(resolved.workspace_path.clone(), resolved.veil_id.clone())
}

/// 返回当前目录下默认的 `<容器名>.veil-link` 路径。
///
/// 当前目录不可读取时以 `.` 作为回退目录；函数不检查该路径是否已存在。
pub fn default_link_path(container_name: &str) -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(format!("{}.veil-link", container_name))
}

/// 当前本地时间，精确到秒，用于重名时生成后缀。
pub fn creation_timestamp() -> String {
    chrono::Local::now().format("%Y%m%d%H%M%S").to_string()
}

/// 为重名容器分配不会覆盖现有文件的默认链接路径。
pub fn default_link_path_for_time(container_name: &str, creation_time: &str) -> PathBuf {
    // 默认名称未冲突时直接使用，保持日常路径最简短。
    let default_path = default_link_path(container_name);
    if !default_path.exists() {
        return default_path;
    }

    // 冲突后加入创建时间，尽量让不同批次生成的链接保持可读顺序。
    let base_name = veil_core::workspace::container_name_with_suffix(container_name, creation_time);
    let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // 同一秒内继续重名时追加递增序号，直到找到不存在的路径。
    for index in 1u32.. {
        let file_name = if index == 1 {
            format!("{}.veil-link", base_name)
        } else {
            format!("{}-{}.veil-link", base_name, index)
        };
        let candidate = current_dir.join(file_name);
        if !candidate.exists() {
            return candidate;
        }
    }

    unreachable!("默认链接名称序号不可能耗尽")
}

/// 显示提示词并读取一行无回显密码。
///
/// 提示词会先转换为当前终端编码；密码内容始终按 UTF-8 字符串读取。
///
/// # 错误
/// 标准输出写入、刷新或终端密码读取失败时返回 [`std::io::Error`]。
fn prompt_password_adaptive(prompt: &str) -> std::io::Result<String> {
    use std::io::Write;

    // 提示文字需要适配旧式 Windows 控制台，密码内容本身保持 UTF-8。
    let prompt_bytes = crate::output_encoding::encode_for_display(prompt);
    std::io::stdout().write_all(&prompt_bytes)?;
    std::io::stdout().flush()?;

    rpassword::read_password()
}

/// 提示用户输入密码（不回显）。
///
/// 支持三种方式（优先级从高到低）：
/// 1. 命令行参数：直接使用传入的密码
/// 2. 环境变量：从 `VEIL_PASSWORD` 读取（用于脚本/测试）
/// 3. 交互式：从 TTY 读取（不回显）
///
/// # 参数
/// - `prompt`: 提示文本（如 "请输入容器密码: "）
/// - `password_arg`: 命令行参数提供的密码（`None` 则尝试环境变量或交互式）
///
/// # 返回
/// - `Ok(SecretString)`: 成功获取密码
/// - `Err(CommandError)`: 失败（TTY 不可用或读取失败）
///
/// # 示例
/// ```no_run
/// # fn main() -> veil_cli::error::Result<()> {
/// # use veil_cli::commands::prompt_password;
/// let password = prompt_password("请输入密码: ", Some("mypass".to_string()))?;
/// let password = prompt_password("请输入密码: ", None)?; // 环境变量或交互式
/// # Ok(())
/// # }
/// ```
pub fn prompt_password(
    prompt: &str,
    password_arg: Option<String>,
) -> Result<age::secrecy::SecretString> {
    // 显式参数优先级最高，适用于调用方已经获得密码的场景。
    if let Some(pwd) = password_arg {
        return Ok(age::secrecy::SecretString::from(pwd));
    }

    // 环境变量用于脚本和自动化，读取后立即包装为 SecretString。
    if let Ok(password) = std::env::var("VEIL_PASSWORD") {
        return Ok(age::secrecy::SecretString::from(password));
    }

    // 最后才进入 TTY 读取，rpassword 负责关闭回显。
    let password = prompt_password_adaptive(prompt)?;
    Ok(age::secrecy::SecretString::from(password))
}

/// 提示用户输入密码并确认（用于创建容器或修改密码）。
///
/// 支持三种方式（优先级从高到低）：
/// 1. 命令行参数：直接使用传入的密码（跳过确认）
/// 2. 环境变量：从 `VEIL_PASSWORD` 读取（用于脚本/测试，跳过确认）
/// 3. 交互式：从 TTY 读取两次密码并确认（不回显）
///
/// # 参数
/// - `password_arg`: 命令行参数提供的密码（`None` 则尝试环境变量或交互式）
///
/// # 返回
/// - `Ok(SecretString)`: 成功获取密码
/// - `Err(CommandError)`: 失败（密码为空、两次输入不一致、TTY 不可用或读取失败）
///
/// # 示例
/// ```no_run
/// # fn main() -> veil_cli::error::Result<()> {
/// # use veil_cli::commands::prompt_new_password;
/// let password = prompt_new_password(Some("mypass".to_string()))?;
/// let password = prompt_new_password(None)?; // 环境变量或交互式确认
/// # Ok(())
/// # }
/// ```
pub fn prompt_new_password(password_arg: Option<String>) -> Result<age::secrecy::SecretString> {
    prompt_new_password_with_env(password_arg, "VEIL_PASSWORD")
}

/// 使用指定环境变量获取新密码，并在交互模式下要求输入两次。
///
/// 修改密码时传入 `VEIL_NEW_PASSWORD`，避免把旧密码环境变量静默复用为新密码。
pub fn prompt_new_password_with_env(
    password_arg: Option<String>,
    env_name: &str,
) -> Result<age::secrecy::SecretString> {
    // 显式参数跳过确认，调用方已经负责校验来源。
    if let Some(pwd) = password_arg {
        return validate_new_password(age::secrecy::SecretString::from(pwd));
    }

    // 指定环境变量同样跳过二次输入，便于非交互调用。
    if let Ok(password) = std::env::var(env_name) {
        return validate_new_password(age::secrecy::SecretString::from(password));
    }

    // 交互模式必须输入两次，不一致时不会向命令层返回任何密码。
    let password = prompt_password_adaptive(crate::i18n::t("prompt.new_password"))?;
    let confirm = prompt_password_adaptive(crate::i18n::t("prompt.confirm_password"))?;

    validate_password_confirmation(password, &confirm)
}

/// 校验交互输入的新密码和确认密码完全一致。
fn validate_password_confirmation(
    password: String,
    confirmation: &str,
) -> Result<age::secrecy::SecretString> {
    if password != confirmation {
        crate::cli_bail!(PasswordMismatch);
    }
    validate_new_password(age::secrecy::SecretString::from(password))
}

/// 拒绝可用于新容器的空密码。
///
/// 旧容器仍可使用空密码打开，以保持对历史数据的兼容；只有创建或修改密码时
/// 禁止产生新的无密码容器。
fn validate_new_password(
    password: age::secrecy::SecretString,
) -> Result<age::secrecy::SecretString> {
    use age::secrecy::ExposeSecret;

    if password.expose_secret().is_empty() {
        crate::cli_bail!(PasswordEmpty);
    }

    Ok(password)
}

#[cfg(test)]
mod tests {
    use super::validate_password_confirmation;

    /// 验证两次交互密码不一致时稳定返回密码错误。
    #[test]
    fn mismatched_password_confirmation_is_rejected() {
        use age::secrecy::ExposeSecret;

        let error = validate_password_confirmation("first".to_string(), "second").unwrap_err();
        assert_eq!(error.code(), crate::error::ErrorCode::PasswordMismatch);

        let password = validate_password_confirmation("same".to_string(), "same").unwrap();
        assert_eq!(password.expose_secret(), "same");
    }
}
