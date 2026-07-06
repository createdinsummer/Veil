pub mod init;
pub mod add;
pub mod rm;
pub mod mv;
pub mod free;
pub mod ex;
pub mod info;
pub mod passwd;

/// 提示用户输入密码（不回显）
///
/// 支持三种方式（优先级从高到低）：
/// 1. 命令行参数：直接使用传入的密码
/// 2. 环境变量：从 VEIL_PASSWORD 读取（用于脚本/测试）
/// 3. 交互式：从 TTY 读取（不回显）
pub fn prompt_password(prompt: &str, password_arg: Option<String>) -> anyhow::Result<age::secrecy::SecretString> {
    // 优先使用命令行参数
    if let Some(pwd) = password_arg {
        return Ok(age::secrecy::SecretString::from(pwd));
    }

    // 其次从环境变量读取（方便测试和脚本）
    if let Ok(password) = std::env::var("VEIL_PASSWORD") {
        return Ok(age::secrecy::SecretString::from(password));
    }

    // 最后交互式读取
    let password = rpassword::prompt_password(prompt)?;
    Ok(age::secrecy::SecretString::from(password))
}

/// 提示用户输入密码并确认（用于创建容器或修改密码）
///
/// 支持三种方式（优先级从高到低）：
/// 1. 命令行参数：直接使用传入的密码（跳过确认）
/// 2. 环境变量：从 VEIL_PASSWORD 读取（用于脚本/测试，跳过确认）
/// 3. 交互式：从 TTY 读取两次密码并确认
pub fn prompt_new_password(password_arg: Option<String>) -> anyhow::Result<age::secrecy::SecretString> {
    // 命令行参数优先
    if let Some(pwd) = password_arg {
        return Ok(age::secrecy::SecretString::from(pwd));
    }

    // 从环境变量读取则跳过确认
    if let Ok(password) = std::env::var("VEIL_PASSWORD") {
        return Ok(age::secrecy::SecretString::from(password));
    }

    // 交互式输入并确认
    let password = rpassword::prompt_password("请输入密码: ")?;
    let confirm = rpassword::prompt_password("请再次输入密码: ")?;

    if password != confirm {
        anyhow::bail!("两次输入的密码不一致");
    }

    Ok(age::secrecy::SecretString::from(password))
}
