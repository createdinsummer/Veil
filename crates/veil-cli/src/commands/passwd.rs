use anyhow::Result;
use colored::Colorize;
use veil_core::container::Container;

pub fn run(container_path: &str, password: Option<String>, new_password: Option<String>) -> Result<()> {
    let old_password = super::prompt_password(crate::i18n::t("prompt.current_password"), password)?;

    println!("{}", crate::i18n::t("passwd.verifying").cyan());
    let container = Container::open(container_path, old_password)?;

    let new_password = if let Some(new_pwd) = new_password {
        age::secrecy::SecretString::from(new_pwd)
    } else if let Ok(new_pass) = std::env::var("VEIL_NEW_PASSWORD") {
        age::secrecy::SecretString::from(new_pass)
    } else {
        println!("{}", crate::i18n::t("passwd.set_new").cyan());
        super::prompt_new_password(None)?
    };

    println!("{}", crate::i18n::t("passwd.changing").cyan());
    container.change_password(new_password)?;

    println!("{}", crate::i18n::t("passwd.changed").green());
    Ok(())
}
