//! `veil ex` 子命令：通过共享流水线解密导出文件或文件夹。

use crate::error::Result;
use colored::Colorize;
use std::path::Path;

/// 导出文件或文件夹到本地文件系统。
///
/// 从容器中解密并导出文件到本地文件系统。
///
/// # 参数
/// - `veil_name`: 容器名称、`veil_id` 或 `.veil-link` 路径。
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
    veil_name: &str,
    file_name: Option<&str>,
    output: &str,
    password: Option<String>,
    mut conflict_policy: super::add::ConflictPolicy,
) -> Result<()> {
    // 链接只负责定位，实际操作绑定到确认后的稳定 ID。
    let resolved = super::resolve_veil(veil_name)?;

    // 导出必须明确容器内路径，未提供时直接给出命令用法错误。
    let file_name = file_name.ok_or_else(|| crate::cli_error!(ExportPathRequired))?;

    crate::outln!("{}", crate::i18n::t("opening_veil").cyan());
    let password_str = super::prompt_password(crate::i18n::t("prompt.veil_password"), password)?;

    use secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 复用服务层导出流水线，终端负责进度展示与冲突选择。
    let output_path = Path::new(output);

    let result = resolved
        .stream_export(
            file_name,
            output_path,
            password,
            |conflict| super::add::decide_conflict(conflict, &mut conflict_policy),
            super::transfer::progress,
        )
        .map_err(super::map_service_error)?;
    if result.skipped_files > 0 {
        crate::outln!("已跳过 {} 个文件", result.skipped_files);
    }
    if result.completed_files == result.skipped_files && result.total_files > 0 {
        return Ok(());
    }

    crate::outln!(
        "{}",
        crate::i18n::t1("ex.file_exported", "path", output).green()
    );

    Ok(())
}
