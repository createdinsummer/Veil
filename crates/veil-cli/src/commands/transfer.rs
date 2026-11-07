//! CLI 传输进度；流水线和冲突判断由 service 执行。
use std::io::{self, IsTerminal, Write};
use veil_core::progress::Snapshot;

pub(super) fn progress(snapshot: Snapshot) {
    if !io::stderr().is_terminal() {
        return;
    }
    let operation = if snapshot.operation == "export" {
        "导出"
    } else {
        "导入"
    };
    let percent = if snapshot.total_files == 0 {
        0
    } else {
        snapshot.completed_files.saturating_mul(100) / snapshot.total_files
    };
    eprint!(
        "\r\x1b[2K{operation}：{} / {}（{percent}%），跳过 {}，待选择 {}{}",
        snapshot.completed_files,
        snapshot.total_files,
        snapshot.skipped_files,
        snapshot.waiting_files,
        if snapshot.scan_complete {
            ""
        } else {
            "，正在遍历"
        }
    );
    if matches!(snapshot.phase, "completed" | "failed" | "cancelled") {
        eprintln!();
    }
    let _ = io::stderr().flush();
}
