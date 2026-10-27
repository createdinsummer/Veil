//! `veil exists` 子命令：判断容器内文件或目录是否存在。

use crate::error::Result;
use colored::Colorize;
use veil_service::veil_ops::ContainerPathKind;

/// 判断容器内路径是否存在。
///
/// 返回 `true` 表示文件或目录存在，返回 `false` 表示路径不存在。两种结果都会输出
/// 可读信息，调用方可以同时使用退出码和输出进行脚本判断。
pub fn run(veil_name: &str, path: &str, password: Option<String>) -> Result<bool> {
    let resolved = super::resolve_veil(veil_name)?;
    let password_str = super::prompt_password(crate::i18n::t("prompt.veil_password"), password)?;

    use secrecy::ExposeSecret;
    let password = password_str.expose_secret();
    let manager = super::container_client(&resolved);
    match manager.path_kind(path, password)? {
        ContainerPathKind::File(path) => {
            crate::outln!("{}", crate::i18n::t1("exists.file", "path", &path).green());
            Ok(true)
        }
        ContainerPathKind::Directory(path) => {
            crate::outln!(
                "{}",
                crate::i18n::t1("exists.directory", "path", &path).green()
            );
            Ok(true)
        }
        ContainerPathKind::Missing(path) => {
            crate::outln!(
                "{}",
                crate::i18n::t1("exists.missing", "path", &path).yellow()
            );
            Ok(false)
        }
    }
}
