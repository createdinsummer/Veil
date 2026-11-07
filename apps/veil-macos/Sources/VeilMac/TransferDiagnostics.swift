import Foundation

struct TransferDiagnosticRecord: Codable, Identifiable, Sendable {
    let id: String
    let occurredAt: String
    let appVersion: String
    let appBuild: String
    let operatingSystem: String
    let operation: String
    let phase: String
    let failureCode: String
    let summary: String
    let completedFiles: UInt64
    let totalFiles: UInt64
    let skippedFiles: UInt64
    let checkedFiles: UInt64
    let pendingFiles: UInt64
    let waitingFiles: UInt64
    let scanComplete: Bool
    let parallelWorkers: UInt64
}

enum TransferDiagnostics {
    private static let maximumRecords = 20
    private static let directory = FileManager.default
        .urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("Veil/diagnostics", isDirectory: true)

    static func makeRecord(error: Error, operation: String, progress: TransferProgress?) -> TransferDiagnosticRecord {
        let description = error.localizedDescription.lowercased()
        let isDatabaseLocked = description.contains("database is locked")
            || description.contains("database table is locked")
            || description.contains("database is busy")
            || description.contains("数据库锁")
        let failureCode = isDatabaseLocked ? "SQLITE_BUSY_OR_LOCKED" : "TRANSFER_FAILED"
        let summary = isDatabaseLocked
            ? "数据库锁竞争导致任务失败（SQLite BUSY/LOCKED）。"
            : "任务执行失败。原始错误内容已省略，以免包含文件名或本机路径。"
        let bundle = Bundle.main
        let now = Date()

        return TransferDiagnosticRecord(
            id: UUID().uuidString,
            occurredAt: ISO8601DateFormatter().string(from: now),
            appVersion: bundle.infoDictionary?["CFBundleShortVersionString"] as? String ?? "unknown",
            appBuild: bundle.infoDictionary?["CFBundleVersion"] as? String ?? "unknown",
            operatingSystem: ProcessInfo.processInfo.operatingSystemVersionString,
            operation: operationLabel(operation),
            phase: phaseLabel(progress?.phase),
            failureCode: failureCode,
            summary: summary,
            completedFiles: progress?.completedFiles ?? 0,
            totalFiles: progress?.totalFiles ?? 0,
            skippedFiles: progress?.skippedFiles ?? 0,
            checkedFiles: progress?.checkedFiles ?? 0,
            pendingFiles: progress?.pendingFiles ?? 0,
            waitingFiles: progress?.waitingFiles ?? 0,
            scanComplete: progress?.scanComplete ?? false,
            parallelWorkers: progress?.parallelWorkers ?? 0
        )
    }

    static func latestRecord() -> TransferDiagnosticRecord? {
        guard let files = try? FileManager.default.contentsOfDirectory(
            at: directory,
            includingPropertiesForKeys: nil
        ) else { return nil }

        let decoder = JSONDecoder()
        return files
            .filter { $0.pathExtension == "json" }
            .sorted { $0.lastPathComponent > $1.lastPathComponent }
            .prefix(maximumRecords)
            .compactMap { try? decoder.decode(TransferDiagnosticRecord.self, from: Data(contentsOf: $0)) }
            .first
    }

    static func append(_ record: TransferDiagnosticRecord) {
        do {
            try FileManager.default.createDirectory(
                at: directory,
                withIntermediateDirectories: true,
                attributes: [.posixPermissions: 0o700]
            )
            try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: directory.path)

            let formatter = DateFormatter()
            formatter.locale = Locale(identifier: "en_US_POSIX")
            formatter.timeZone = TimeZone(secondsFromGMT: 0)
            formatter.dateFormat = "yyyyMMdd'T'HHmmssSSS'Z'"
            let filename = "transfer-failure-\(formatter.string(from: Date()))-\(record.id).json"
            let url = directory.appendingPathComponent(filename)
            try encoded(record).write(to: url, options: .atomic)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)

            let files = (try? FileManager.default.contentsOfDirectory(
                at: directory,
                includingPropertiesForKeys: [.contentModificationDateKey]
            )) ?? []
            for file in files.filter({ $0.pathExtension == "json" })
                .sorted(by: { $0.lastPathComponent > $1.lastPathComponent })
                .dropFirst(maximumRecords) {
                try? FileManager.default.removeItem(at: file)
            }
        } catch {
            // 诊断记录失败不能改变原任务的结果。
        }
    }

    static func export(_ record: TransferDiagnosticRecord, to url: URL) throws {
        try encoded(record).write(to: url, options: .atomic)
    }

    private static func encoded(_ record: TransferDiagnosticRecord) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return try encoder.encode(record)
    }

    private static func operationLabel(_ operation: String) -> String {
        switch operation {
        case "import": "导入文件"
        case "export": "导出文件"
        case "import_package": "导入容器"
        case "export_package": "导出容器"
        default: "文件传输任务"
        }
    }

    private static func phaseLabel(_ phase: String?) -> String {
        switch phase {
        case "preparing": "准备中"
        case "processing": "处理中"
        case "verifying": "核对中"
        case "snapshot": "创建快照"
        case "packing": "打包中"
        case "unpacking": "解包中"
        case "finalizing": "收尾中"
        case "cancelled": "已取消"
        default: "未知"
        }
    }
}
