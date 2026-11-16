/// macOS 任务中心窗口，汇总当前传输进度、最近结果和诊断导出入口。
/// 所属模块：桌面端后台任务反馈；VeilWindowID 标识独立窗口，TransferProgress 扩展计算通用进度，TaskCenterView 与卡片负责显示和取消。
/// 依赖 AppStore 任务快照和日志导出、AppKit 保存面板及 SwiftUI 布局。目标是集中展示任务状态；关闭任务中心不主动取消任务，主窗口关闭的锁定策略由应用入口另行处理。
import AppKit
import SwiftUI

/// 应用内独立窗口的稳定标识；主窗口使用该 ID 打开任务中心。
enum VeilWindowID {
    /// 任务中心在 SwiftUI Window 场景中注册的稳定 ID。
    static let taskCenter = "task-center"
}

/// 为传输快照提供视图复用的进度判断和显示值；这些计算不修改任务状态。
extension TransferProgress {
    /// 是否为按文件数量展示的普通文件导入 / 导出任务。
    var isFileTransfer: Bool { operation == "import" || operation == "export" }

    /// 优先按普通传输的已完成 / 已发现文件数计算，否则按字节数；无有效分母返回 nil，上限为 1。
    /// 扫描中总数可增长，比例可能下降；达到 1 也不表示提交收尾已成功。
    var fractionCompleted: Double? {
        if isFileTransfer, totalFiles > 0 {
            return min(1, Double(completedFiles) / Double(totalFiles))
        }
        if let totalBytes, totalBytes > 0 {
            return min(1, Double(processedBytes) / Double(totalBytes))
        }
        return nil
    }

    /// 把比例乘 100 后截断为整数百分比，不四舍五入；无可计算比例时返回 nil。
    var percentageText: String? {
        guard let fractionCompleted else { return nil }
        return "\(Int(fractionCompleted * 100))%"
    }
}

/// 独立任务状态窗口，读取共享 Store，不持有或调度传输任务。
/// 显示优先级为当前任务、最近失败、notice、空闲；notice 也可能来自其他业务操作，不必然是传输结果。
struct TaskCenterView: View {
    /// 提供实时任务、最近结果及诊断文件导出能力。
    @EnvironmentObject private var store: AppStore

    /// 按当前任务和最近结果生成窗口主标题。
    private var heading: String {
        if store.transferTitle != nil { return "进行中的任务" }
        if store.lastTransferFailure != nil || store.lastTransferError != nil || store.notice != nil { return "最近任务" }
        return "任务中心"
    }

    /// 按当前任务、故障摘要、原始错误提示和 notice 的优先级选择说明；不是任务存活性的检测。
    private var subtitle: String {
        if store.transferTitle != nil { return "任务会在后台继续运行，关闭此窗口不会中止任务。" }
        if store.lastTransferFailure != nil { return "失败记录可导出并附在反馈中，日志不包含文件名和本机路径。" }
        if store.lastTransferError != nil { return "任务失败详情会保留在这里。" }
        if store.notice != nil { return "最近一次任务的处理结果。" }
        return "开始导入或导出文件后，任务进度会显示在这里。"
    }

    /// 展示当前任务或最近结果，并保留常驻的错误日志导出入口。
    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            VStack(alignment: .leading, spacing: 5) {
                Text(heading).font(.system(size: 19, weight: .semibold))
                Text(subtitle)
                    .font(.system(size: 12)).foregroundStyle(VeilTheme.secondary)
            }

            if let title = store.transferTitle {
                TaskSummaryCard(title: title, progress: store.transferProgress)
            } else if store.lastTransferFailure != nil || store.lastTransferError != nil {
                VStack(alignment: .leading, spacing: 8) {
                    Label("最近任务失败", systemImage: "exclamationmark.triangle")
                        .font(.system(size: 13, weight: .medium)).foregroundStyle(VeilTheme.danger)
                    if let failure = store.lastTransferFailure {
                        Text(failure.summary).font(.system(size: 12)).foregroundStyle(VeilTheme.secondary)
                        Text("阶段：\(failure.phase) · 已完成 \(failure.completedFiles) / \(failure.totalFiles) 个文件 · 跳过 \(failure.skippedFiles) 个")
                            .font(.system(size: 11)).foregroundStyle(VeilTheme.secondary).monospacedDigit()
                    }
                    if let error = store.lastTransferError {
                        Text(error).font(.system(size: 11)).foregroundStyle(VeilTheme.secondary)
                            .textSelection(.enabled)
                    }
                    HStack {
                        if let failure = store.lastTransferFailure {
                            Text("诊断编号：\(failure.failureCode)")
                                .font(.system(size: 10, weight: .medium)).foregroundStyle(VeilTheme.secondary)
                                .textSelection(.enabled)
                        }
                        Spacer()
                        if store.lastTransferFailure != nil {
                            Button("导出诊断日志…", action: exportDiagnostic)
                                .buttonStyle(VeilButtonStyle())
                        }
                    }
                }
                .frame(maxWidth: .infinity, minHeight: 112, alignment: .leading)
                .padding(18)
                .background(VeilTheme.card, in: RoundedRectangle(cornerRadius: 10))
                .overlay(RoundedRectangle(cornerRadius: 10).stroke(VeilTheme.border))
            } else if let notice = store.notice {
                VStack(alignment: .leading, spacing: 8) {
                    Label("最近任务", systemImage: "checkmark.circle")
                        .font(.system(size: 13, weight: .medium))
                    Text(notice).font(.system(size: 12)).foregroundStyle(VeilTheme.secondary)
                }
                .frame(maxWidth: .infinity, minHeight: 112, alignment: .leading)
                .padding(18)
                .background(VeilTheme.card, in: RoundedRectangle(cornerRadius: 10))
                .overlay(RoundedRectangle(cornerRadius: 10).stroke(VeilTheme.border))
            } else {
                VStack(alignment: .leading, spacing: 8) {
                    Label("当前没有进行中的任务", systemImage: "checkmark.circle")
                        .font(.system(size: 13, weight: .medium))
                    Text("开始导入或导出文件后，任务进度会显示在这里。")
                        .font(.system(size: 12)).foregroundStyle(VeilTheme.secondary)
                }
                .frame(maxWidth: .infinity, minHeight: 112, alignment: .leading)
                .padding(18)
                .background(VeilTheme.card, in: RoundedRectangle(cornerRadius: 10))
                .overlay(RoundedRectangle(cornerRadius: 10).stroke(VeilTheme.border))
            }
            Spacer(minLength: 0)
            HStack {
                Text("错误日志仅保存在本机，可导出用于反馈。")
                    .font(.system(size: 11)).foregroundStyle(VeilTheme.secondary)
                Spacer()
                Button("导出错误日志…", action: exportErrorLogs)
                    .buttonStyle(VeilButtonStyle())
            }
        }
        .padding(24)
        .frame(minWidth: 520, minHeight: 280, alignment: .topLeading)
        .background(VeilTheme.canvas)
    }

    /// 让用户选择保存位置并请求导出通用错误日志。
    /// - Parameters: 无。
    /// - Returns: Void；异步显示保存面板，用户取消或没有目标 URL 时无操作，确认后交给 Store 导出。
    /// - Throws: 不向调用方抛错，写入失败由 Store 报告。
    /// - 注意事项: 主线程调用；面板返回时才读取日志，不是打开面板瞬间的快照；.json 类型筛选不负责验证导出内容。
    private func exportErrorLogs() {
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.json]
        panel.nameFieldStringValue = "Veil-错误日志.json"
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            store.exportErrorLogs(to: url)
        }
    }

    /// 让用户把最近的传输故障摘要保存为 JSON。
    /// - Parameters: 无；默认文件名从当前 Store 故障状态生成。
    /// - Returns: Void；确认目标后调用 Store 导出，取消不清除记录。
    /// - Throws: 不向调用方抛出异常，实际编码和写入错误由 Store 处理。
    /// - 注意事项: 主线程调用；面板打开前未捕获记录快照，等待期间最近故障可能变化，最终导出的记录由回调执行时的 Store 状态决定。
    private func exportDiagnostic() {
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.json]
        panel.nameFieldStringValue = store.transferDiagnosticFilename
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            store.exportTransferDiagnostic(to: url)
        }
    }
}

/// 当前传输任务卡片，显示阶段、完成比例、队列余量和取消按钮。
private struct TaskSummaryCard: View {
    /// 提交取消请求的共享任务状态。
    @EnvironmentObject private var store: AppStore
    /// 当前任务的用户可读标题。
    let title: String
    /// 最新进度快照；为空表示后端仍在准备任务信息。
    let progress: TransferProgress?

    /// 把阶段码、取消状态和导入 / 导出方向转换为简短业务状态。
    private var statusText: String {
        guard let progress else { return "正在准备任务…" }
        if progress.cancelRequested { return "正在停止任务…" }
        if progress.isFileTransfer {
            if !progress.scanComplete { return "正在扫描文件，总数会继续增加" }
            if progress.phase == "verifying" { return "正在核对文件" }
            if progress.phase == "finalizing" { return "正在保存结果" }
            return progress.operation == "import" ? "已统计全部文件，正在导入" : "已统计全部文件，正在导出"
        }
        return switch progress.phase {
        case "snapshot": "正在创建快照"
        case "packing": "正在打包"
        case "unpacking": "正在解包"
        case "verifying": "正在核对"
        case "processing": "正在处理"
        case "finalizing": "正在完成"
        default: "正在准备任务…"
        }
    }

    /// 展示已完成 / 总文件数或已处理 / 总字节数。
    private var countText: String {
        guard let progress else { return "正在获取进度…" }
        if progress.isFileTransfer {
            guard progress.totalFiles > 0 else { return "正在统计文件…" }
            return "已完成 \(progress.completedFiles) / \(progress.totalFiles) 个文件"
        }
        if let totalBytes = progress.totalBytes, totalBytes > 0 {
            let completed = ByteCountFormatter.string(fromByteCount: Int64(clamping: progress.processedBytes), countStyle: .file)
            let total = ByteCountFormatter.string(fromByteCount: Int64(clamping: totalBytes), countStyle: .file)
            return "\(completed) / \(total)"
        }
        return "正在获取进度…"
    }

    /// 汇总待处理队列与已加密待提交条数；没有待办时返回 nil。
    private var queueText: String? {
        guard let progress, progress.isFileTransfer else { return nil }
        // 每项是一个队列计数的展示片段；没有对应计数时不追加，最后合并成单行。
        var items: [String] = []
        if progress.waitingFiles > 0 { items.append("待处理：\(progress.waitingFiles) 个") }
        if progress.operation == "import", progress.pendingFiles > 0 {
            items.append("已加密，待提交：\(progress.pendingFiles) 个")
        }
        return items.isEmpty ? nil : items.joined(separator: " · ")
    }

    /// 依据阶段白名单和取消请求标记启用按钮；这是界面策略，不保证服务收到请求时仍处于可中断阶段。
    private var canCancel: Bool {
        guard let progress, !progress.cancelRequested else { return false }
        return ["preparing", "processing", "verifying", "snapshot", "packing", "unpacking"].contains(progress.phase)
    }

    /// 按任务种类、阶段和取消状态生成按钮文字。
    private var cancelText: String {
        if progress?.cancelRequested == true { return "正在中止…" }
        guard canCancel else { return progress == nil ? "正在准备…" : "正在完成…" }
        if progress?.operation == "export" || progress?.operation == "export_package" { return "中止导出" }
        if progress?.operation == "import" || progress?.operation == "import_package" { return "中止导入" }
        return "中止任务"
    }

    /// 组合任务名称、进度、队列摘要和取消控制。
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Image(systemName: progress?.operation.hasPrefix("export") == true ? "arrow.up.doc" : "arrow.down.doc")
                    .foregroundStyle(VeilTheme.accent)
                Text(title).font(.system(size: 14, weight: .semibold)).lineLimit(1)
                Spacer()
                Text(progress?.percentageText ?? "—")
                    .font(.system(size: 13, weight: .medium)).monospacedDigit()
                    .foregroundStyle(VeilTheme.secondary).frame(width: 44, alignment: .trailing)
            }
            Group {
                if let value = progress?.fractionCompleted {
                    ProgressView(value: value)
                } else {
                    ProgressView()
                }
            }
            .controlSize(.small).frame(height: 8)
            Text(statusText).font(.system(size: 12)).foregroundStyle(VeilTheme.secondary)
                .lineLimit(1).frame(height: 18, alignment: .leading)
            Text(countText).font(.system(size: 12)).foregroundStyle(VeilTheme.secondary)
                .monospacedDigit().lineLimit(1).frame(height: 18, alignment: .leading)
            Text(queueText ?? " ").font(.system(size: 12)).foregroundStyle(VeilTheme.secondary)
                .lineLimit(1).truncationMode(.middle).frame(height: 18, alignment: .leading)
                .accessibilityHidden(queueText == nil)
            HStack {
                Spacer()
                // 只请求取消，不在此等待工作线程结束或撤销已经完成的文件。
                Button(cancelText) { store.cancelTransfer() }
                    .buttonStyle(VeilButtonStyle())
                    .disabled(!canCancel)
            }
        }
        .padding(18)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(VeilTheme.card, in: RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(VeilTheme.border))
    }
}
