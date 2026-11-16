//! 用户主动导出本地诊断记录。
//! 所属模块：CLI命令适配。核心run导出JSON，failed映射并记录失败；依赖滚动日志、排他文件创建和同步，目标是提供可供排查的记录。
//!
//! 把滚动日志导出为JSON：默认记录WARN/ERROR，标准记录入口按脱敏规则处理敏感值，
//! 导出不对历史记录重新全面净化。文件不覆盖；Unix创建权限0600；写入失败不改变
//! 业务操作结果。省略 `--output` 时在当前目录生成带 UTC 时间的 JSON 文件。
use crate::error::Result;
use std::{fs::OpenOptions, io::Write, path::PathBuf};

/// 导出当前滚动诊断记录到一个新JSON文件。
/// 入参：output（`Option<PathBuf>`）为输出路径，None使用当前目录下UTC秒级文件名。
/// 返回值：写完JSON并sync_all成功后输出路径、返回Ok(())。
/// 异常：快照、排他创建、写入/同步失败返回已标日志的CommandError；错误后删除半成品仅尽力。
/// 注意事项：默认同秒名称可冲突，不自动加序号；不创建父目录。Unix创建权限0600，受umask影响；不额外同步父目录，不能保证断电持久性。导出含已有记录，不重新全面净化历史正文。
pub fn run(output: Option<PathBuf>) -> Result<()> {
    // 未指定路径时用当前 UTC 时间命名，避免覆盖历史导出文件。
    let output = output.unwrap_or_else(|| {
        PathBuf::from(format!(
            "veil-logs-{}.json",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        ))
    });
    // 导出滚动日志记录快照为JSON；底层可能读取现存日志文件，不只取内存缓冲。
    let data = veil_core::logging::export().map_err(|error| failed("read_log_snapshot", error))?;
    let mut options = OpenOptions::new();
    // create_new 拒绝覆盖已有文件，符合"不覆盖已有导出"的业务语义。
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // 日志可能含诊断上下文，Unix 下权限收紧为仅属主可读写。
        options.mode(0o600);
    }
    let mut file = options
        .open(&output)
        .map_err(|error| failed("create_log_export", error))?;
    // 写完后请求同步文件；父目录未额外同步，实际断电持久性仍取决于平台和设备。
    let result = file.write_all(&data).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = result {
        // 写入失败清理半成品，避免留下损坏的日志文件。
        let _ = std::fs::remove_file(&output);
        return Err(failed("write_log_export", error));
    }
    crate::outln!("诊断日志已保存：{}", output.display());
    Ok(())
}

/// 为日志导出失败记录固定阶段诊断并构造命令错误。
/// 入参：phase（&'static str）为内部固定步骤标签；error（io::Error）为原失败。
/// 返回值：带already_reported标记的CommandError。
/// 异常：构造无Result返回，日志写入自身不在此返回错误。
/// 注意事项：不清理输出或恢复文件；标记避免CLI边界重复记录，不改变其他容器业务状态。
fn failed(phase: &'static str, error: std::io::Error) -> crate::error::CommandError {
    veil_core::logging::failure(
        veil_core::logging::Level::Error,
        "cli",
        phase,
        "LOG_EXPORT_FAILED",
        &error,
        &[],
    );
    crate::error::CommandError::from(error).already_reported()
}
