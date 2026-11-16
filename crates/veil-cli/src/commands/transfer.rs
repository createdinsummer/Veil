//! CLI 传输进度；流水线和冲突判断由 service 执行。
//! 所属模块：CLI命令适配。核心progress只渲染Snapshot；依赖stderr终端检测和进度结构，不持容器数据，不执行传输。
//!
//! 进度只在终端交互时打印到标准错误（stderr），非终端（脚本/管道）保持安静；
//! 每帧用 `\r` + 清屏序列原地刷新同一行，操作结束时换行收尾。
use std::io::{self, IsTerminal, Write};
use veil_core::progress::Snapshot;

/// 将服务推送的传输快照渲染到交互终端stderr。
/// 入参：snapshot（Snapshot）按值传入，包含发现/完成/跳过/待选数量及阶段。
/// 返回值：无；非终端不打印，终态阶段打印换行。
/// 异常：flush错误忽略；eprint/eprintln底层输出失败可能panic，不通过此接口返回。
/// 注意事项：完成数含跳过；扫描中总数增长，百分比不是固定工作量承诺，不主动限到100。只展示，不推进文件任务或改变冲突策略。
pub(super) fn progress(snapshot: Snapshot) {
    // 非终端环境不打印进度，避免污染脚本或管道的输出。
    if !io::stderr().is_terminal() {
        return;
    }
    // 按操作类型显示「导出」或「导入」，其余操作统一归为导入。
    let operation = if snapshot.operation == "export" {
        "导出"
    } else {
        "导入"
    };
    // 百分比：总数为 0 时显示 0；saturating_mul 防止大数乘法溢出。
    let percent = if snapshot.total_files == 0 {
        0
    } else {
        snapshot.completed_files.saturating_mul(100) / snapshot.total_files
    };
    // \r 回到行首、\x1b[2K 清除整行后重画，形成原地刷新的进度条。
    eprint!(
        "\r\x1b[2K{operation}：{} / {}（{percent}%），跳过 {}，待选择 {}{}",
        snapshot.completed_files,
        snapshot.total_files,
        snapshot.skipped_files,
        snapshot.waiting_files,
        // 目录还在遍历时追加「正在遍历」，说明总量仍会增长。
        if snapshot.scan_complete {
            ""
        } else {
            "，正在遍历"
        }
    );
    // 终态阶段（完成/失败/取消）换行，避免后续输出与进度行混在同一行。
    if matches!(snapshot.phase, "completed" | "failed" | "cancelled") {
        eprintln!();
    }
    // 立即刷新，保证进度行及时可见；写入失败不影响流水线结果。
    let _ = io::stderr().flush();
}
