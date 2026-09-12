//! `veil ex` 子命令：从工作区容器解密导出文件。

use crate::error::Result;
use colored::Colorize;
use std::path::Path;
use veil_core::workspace_ops::WorkspaceManager;

/// 导出文件到本地文件系统（工作区架构）。
///
/// 从容器中解密并导出文件到本地文件系统。
///
/// # 参数
/// - `container_name`: 容器名称
/// - `file_name`: 要提取的文件名（`None` 则需要指定）
/// - `output`: 导出到的本地路径
/// - `password`: 容器密码（`None` 则交互式输入）
///
/// # 返回
/// - `Ok(())`: 文件已解密并导出
/// - `Err(CommandError)`: 失败
///
/// # 示例
/// ```bash
/// veil ex photos vacation.jpg -o ./vacation.jpg
/// ```
pub fn run(
    container_name: &str,
    file_name: Option<&str>,
    output: &str,
    password: Option<String>,
) -> Result<()> {
    // 链接可同时指向工作区和容器身份，这里取实际工作区根目录。
    let resolved = super::resolve_container(container_name)?;
    let workspace_path = resolved.workspace_path;

    // 导出必须明确容器内路径，未提供时直接给出命令用法错误。
    let file_name = file_name.ok_or_else(|| crate::cli_error!(ExportPathRequired))?;

    crate::outln!("{}", crate::i18n::t("opening_container").cyan());
    let password_str =
        super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 输出路径由调用方决定，core 负责查找、解密和写入。
    let manager = WorkspaceManager::new(workspace_path);
    let output_path = Path::new(output);

    manager.extract_file(file_name, output_path, password)?;

    crate::outln!(
        "{}",
        crate::i18n::t1("ex.file_exported", "path", output).green()
    );

    Ok(())
}
