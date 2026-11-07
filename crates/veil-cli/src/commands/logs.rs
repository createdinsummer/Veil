//! 用户主动导出本地诊断记录。
use crate::error::Result;
use std::{fs::OpenOptions, io::Write, path::PathBuf};

pub fn run(output: Option<PathBuf>) -> Result<()> {
    let output = output.unwrap_or_else(|| {
        PathBuf::from(format!(
            "veil-logs-{}.json",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        ))
    });
    let data = veil_core::logging::export().map_err(|error| failed("read_log_snapshot", error))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&output)
        .map_err(|error| failed("create_log_export", error))?;
    let result = file.write_all(&data).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = result {
        let _ = std::fs::remove_file(&output);
        return Err(failed("write_log_export", error));
    }
    crate::outln!("诊断日志已保存：{}", output.display());
    Ok(())
}

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
