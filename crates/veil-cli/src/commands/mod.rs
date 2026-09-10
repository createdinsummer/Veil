pub mod init;
pub mod add;
pub mod rm;
pub mod mv;
pub mod free;
pub mod ex;
pub mod info;
pub mod passwd;
pub mod shell;

// 工作区架构命令
pub mod init_workspace;
pub mod add_workspace;
pub mod list_workspace;
pub mod rm_workspace;
pub mod extract_workspace;
pub mod pack_workspace;
pub mod unpack_workspace;
pub mod mv_workspace;
pub mod passwd_workspace;
pub mod free_workspace;
pub mod shell_workspace;

/// 密码输入辅助函数（自适应显示编码，跨平台）
///
/// 在所有平台上统一处理密码提示的显示编码转换
fn prompt_password_adaptive(prompt: &str) -> std::io::Result<String> {
    use std::io::Write;

    // 将提示文本编码为显示环境可识别的字节序列（跨平台）
    let prompt_bytes = crate::output_encoding::encode_for_display(prompt);
    std::io::stdout().write_all(&prompt_bytes)?;
    std::io::stdout().flush()?;

    // 使用 rpassword 读取密码（它会处理不回显）
    // 注意：密码输入始终返回 UTF-8 String，不需要编码转换
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
/// - `Err(anyhow::Error)`: 失败（TTY 不可用或读取失败）
///
/// # 示例
/// ```rust
/// let password = prompt_password("请输入密码: ", Some("mypass".to_string()))?;
/// let password = prompt_password("请输入密码: ", None)?; // 环境变量或交互式
/// ```
pub fn prompt_password(
    prompt: &str,
    password_arg: Option<String>,
) -> anyhow::Result<age::secrecy::SecretString> {
    // 优先使用命令行参数
    if let Some(pwd) = password_arg {
        return Ok(age::secrecy::SecretString::from(pwd));
    }

    // 其次从环境变量读取（方便测试和脚本）
    if let Ok(password) = std::env::var("VEIL_PASSWORD") {
        return Ok(age::secrecy::SecretString::from(password));
    }

    // 最后交互式读取（不回显）
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
/// - `Err(anyhow::Error)`: 失败（两次输入不一致、TTY 不可用或读取失败）
///
/// # 示例
/// ```rust
/// let password = prompt_new_password(Some("mypass".to_string()))?;
/// let password = prompt_new_password(None)?; // 环境变量或交互式确认
/// ```
pub fn prompt_new_password(
    password_arg: Option<String>,
) -> anyhow::Result<age::secrecy::SecretString> {
    // 命令行参数优先
    if let Some(pwd) = password_arg {
        return Ok(age::secrecy::SecretString::from(pwd));
    }

    // 从环境变量读取则跳过确认
    if let Ok(password) = std::env::var("VEIL_PASSWORD") {
        return Ok(age::secrecy::SecretString::from(password));
    }

    // 交互式输入并确认（不回显）
    let password = prompt_password_adaptive(crate::i18n::t("prompt.new_password"))?;
    let confirm = prompt_password_adaptive(crate::i18n::t("prompt.confirm_password"))?;

    if password != confirm {
        anyhow::bail!("{}", crate::i18n::t("error.password_mismatch"));
    }

    Ok(age::secrecy::SecretString::from(password))
}
