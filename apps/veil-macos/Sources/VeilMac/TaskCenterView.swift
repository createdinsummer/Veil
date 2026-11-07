import AppKit
import SwiftUI

enum VeilWindowID {
    static let taskCenter = "task-center"
}

extension TransferProgress {
    var isFileTransfer: Bool { operation == "import" || operation == "export" }

    var fractionCompleted: Double? {
        if isFileTransfer, totalFiles > 0 {
            return min(1, Double(completedFiles) / Double(totalFiles))
        }
        if let totalBytes, totalBytes > 0 {
            return min(1, Double(processedBytes) / Double(totalBytes))
        }
        return nil
    }

    var percentageText: String? {
        guard let fractionCompleted else { return nil }
        return "\(Int(fractionCompleted * 100))%"
    }
}

struct TaskCenterView: View {
    @EnvironmentObject private var store: AppStore

    private var heading: String {
        if store.transferTitle != nil { return "进行中的任务" }
        if store.lastTransferFailure != nil || store.lastTransferError != nil || store.notice != nil { return "最近任务" }
        return "任务中心"
    }

    private var subtitle: String {
        if store.transferTitle != nil { return "任务会在后台继续运行，关闭此窗口不会中止任务。" }
        if store.lastTransferFailure != nil { return "失败记录可导出并附在反馈中，日志不包含文件名和本机路径。" }
        if store.lastTransferError != nil { return "任务失败详情会保留在这里。" }
        if store.notice != nil { return "最近一次任务的处理结果。" }
        return "开始导入或导出文件后，任务进度会显示在这里。"
    }

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

    private func exportErrorLogs() {
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.json]
        panel.nameFieldStringValue = "Veil-错误日志.json"
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            store.exportErrorLogs(to: url)
        }
    }

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

private struct TaskSummaryCard: View {
    @EnvironmentObject private var store: AppStore
    let title: String
    let progress: TransferProgress?

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

    private var queueText: String? {
        guard let progress, progress.isFileTransfer else { return nil }
        var items: [String] = []
        if progress.waitingFiles > 0 { items.append("待处理：\(progress.waitingFiles) 个") }
        if progress.operation == "import", progress.pendingFiles > 0 {
            items.append("已加密，待提交：\(progress.pendingFiles) 个")
        }
        return items.isEmpty ? nil : items.joined(separator: " · ")
    }

    private var canCancel: Bool {
        guard let progress, !progress.cancelRequested else { return false }
        return ["preparing", "processing", "verifying", "snapshot", "packing", "unpacking"].contains(progress.phase)
    }

    private var cancelText: String {
        if progress?.cancelRequested == true { return "正在中止…" }
        guard canCancel else { return progress == nil ? "正在准备…" : "正在完成…" }
        if progress?.operation == "export" || progress?.operation == "export_package" { return "中止导出" }
        if progress?.operation == "import" || progress?.operation == "import_package" { return "中止导入" }
        return "中止任务"
    }

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
