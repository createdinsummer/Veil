//! `veil exists` 子命令：判断容器内文件或目录是否存在。

use crate::error::Result;
use colored::Colorize;
use veil_core::workspace_ops::normalize_container_path;

/// 判断容器内路径是否存在。
///
/// 返回 `true` 表示文件或目录存在，返回 `false` 表示路径不存在。两种结果都会输出
/// 可读信息，调用方可以同时使用退出码和输出进行脚本判断。
pub fn run_workspace(container_name: &str, path: &str, password: Option<String>) -> Result<bool> {
    let resolved = super::resolve_container(container_name)?;
    let password_str =
        super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();
    let manager = super::workspace_manager(&resolved);
    let metadata = manager.read_meta(password)?;

    // 根路径代表容器本身，始终是存在的目录。
    if path == "." || path == "/" {
        crate::outln!(
            "{}",
            crate::i18n::t1("exists.directory", "path", path).green()
        );
        return Ok(true);
    }

    let normalized = normalize_container_path(path)?;
    if metadata.find_file(&normalized).is_some() {
        crate::outln!(
            "{}",
            crate::i18n::t1("exists.file", "path", &normalized).green()
        );
        return Ok(true);
    }

    let directory_prefix = format!("{normalized}/");
    if metadata
        .files
        .iter()
        .any(|entry| entry.original_name.starts_with(&directory_prefix))
    {
        crate::outln!(
            "{}",
            crate::i18n::t1("exists.directory", "path", &normalized).green()
        );
        return Ok(true);
    }

    crate::outln!(
        "{}",
        crate::i18n::t1("exists.missing", "path", &normalized).yellow()
    );
    Ok(false)
}
