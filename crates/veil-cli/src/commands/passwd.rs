use anyhow::Result;
use colored::Colorize;
use veil_core::container::Container;

pub fn run(container_path: &str, password: Option<String>, new_password: Option<String>) -> Result<()> {
    let old_password = super::prompt_password("请输入当前密码: ", password)?;

    println!("{}", "正在验证密码...".cyan());
    let container = Container::open(container_path, old_password)?;

    let new_password = if let Some(new_pwd) = new_password {
        age::secrecy::SecretString::from(new_pwd)
    } else if let Ok(new_pass) = std::env::var("VEIL_NEW_PASSWORD") {
        age::secrecy::SecretString::from(new_pass)
    } else {
        println!("{}", "请设置新密码".cyan());
        super::prompt_new_password(None)?
    };

    println!("{}", "正在修改密码...".cyan());
    container.change_password(new_password)?;

    println!("{} 密码修改成功", "✓".green());
    Ok(())
}
