//! `veil ex` 子命令：通过共享流水线解密导出文件或文件夹。
//! 所属模块：CLI明文导出。核心run连接ContainerHandle.stream_export、冲突选择及进度回调；依赖服务导出管线和终端交互，目标是有反馈地发布文件/目录并保留已发布成果。
//!
//! 业务流程：解析容器并验证密码后，按精确虚拟路径解密并写入本地输出路径；
//! 输出路径为必填项，使用支持目录递归的共享导出流水线，同名冲突复用 add 的
//! 询问逻辑（--on-conflict 策略一致）。

use crate::error::Result;
use colored::Colorize;
use std::path::Path;

/// 通过共享导出流水线解密一个选定文件或目录，终端同步处理冲突和进度。
/// 入参：veil_name为选择器；file_name（`Option<&str>`）须Some内部路径；output（&str）为本地目标；password为可选显式密码；conflict_policy为Ask/Error/Skip/Overwrite，交互可改后续策略。
/// 返回值：成功或全部文件跳过为Ok(())，输出跳过数及导出结果。
/// 异常：缺输入路径、定位/密码、冲突拒绝/取消、认证/路径/租约/I/O/流水线错误转CommandError。
/// 注意事项：同名内容也询问；单文件output是精确文件位置，目录保留顶层名称。已发布成果不整体回滚；空目录可创建且没有文件完成计数。
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
    // 跳过的文件单独汇报数量。
    if result.skipped_files > 0 {
        crate::outln!("已跳过 {} 个文件", result.skipped_files);
    }
    // 全部文件都被跳过时不打印成功信息。
    if result.completed_files == result.skipped_files && result.total_files > 0 {
        return Ok(());
    }

    // 导出成功：单文件显示文件名，整目录显示目标目录路径。
    crate::outln!(
        "{}",
        crate::i18n::t1("ex.file_exported", "path", output).green()
    );

    Ok(())
}
