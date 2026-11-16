//! `veil exists` 子命令：判断容器内文件或目录是否存在。
//! 所属模块：CLI命令适配。核心run查询PathKind并返回bool，由main决定退出码；依赖容器适配器和密码输入，目标是支持人和脚本的存在判断。
//!
//! 输出可读信息并配合退出码供脚本判断：文件或目录存在时返回 true（退出码 0），
//! 不存在时输出「不存在」并返回 false（退出码 1），适合直接用于 `if veil exists ...`。

use crate::error::Result;
use colored::Colorize;
use veil_service::veil_ops::ContainerPathKind;

/// 检查内部路径是否存在，并展示文件/目录/缺失结果。
/// 入参：veil_name为容器选择器；path（&str）为内部逻辑路径；password为可选显式密码。
/// 返回值：存在文件/目录为Ok(true)，缺失为Ok(false)，由main映射退出码0/1。
/// 异常：定位、密码、路径或数据库错误返回CommandError，与普通缺失不同。
/// 注意事项：目录为数据库显式节点；未锁定未来状态，脚本随后操作仍须处理路径变化。
pub fn run(veil_name: &str, path: &str, password: Option<String>) -> Result<bool> {
    let resolved = super::resolve_veil(veil_name)?;
    let password_str = super::prompt_password(crate::i18n::t("prompt.veil_password"), password)?;

    use secrecy::ExposeSecret;
    let password = password_str.expose_secret();
    let manager = super::container_client(&resolved);
    // 服务层解析显式目录树节点，返回文件、目录或缺失，不依文件路径前缀临时推断目录。
    match manager.path_kind(path, password)? {
        ContainerPathKind::File(path) => {
            // 命中文件：绿色确认，返回 true（最终退出码 0）。
            crate::outln!("{}", crate::i18n::t1("exists.file", "path", &path).green());
            Ok(true)
        }
        ContainerPathKind::Directory(path) => {
            // 命中目录：绿色确认，返回 true（最终退出码 0）。
            crate::outln!(
                "{}",
                crate::i18n::t1("exists.directory", "path", &path).green()
            );
            Ok(true)
        }
        ContainerPathKind::Missing(path) => {
            // 路径缺失：黄色提示，返回 false（最终退出码 1）。
            crate::outln!(
                "{}",
                crate::i18n::t1("exists.missing", "path", &path).yellow()
            );
            Ok(false)
        }
    }
}
