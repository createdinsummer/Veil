/// 桌面端传输故障诊断记录的创建、限量保存、读取和导出。
/// 所属模块：客户端日志与用户支持；仅保存故障类别、阶段、进度和版本等可分析信息，不保存原始错误路径或文件名。
/// 核心类型：TransferDiagnosticRecord 是可编码记录；TransferDiagnostics 管理诊断目录和最多 20 条记录。
/// 依赖 Foundation JSON 编解码、文件权限设置和 AppLog 错误记录。整体目标是在不泄露用户文件信息的前提下，提供可供用户导出的故障上下文。
import Foundation

/// 一次文件或容器传输失败时可安全导出的摘要。
struct TransferDiagnosticRecord: Codable, Identifiable, Sendable {
    /// 每条诊断记录的唯一标识。
    let id: String
    /// 失败发生时间，ISO 8601 格式。
    let occurredAt: String
    /// 应用显示版本。
    let appVersion: String
    /// 应用构建号。
    let appBuild: String
    /// 当前操作系统版本文案。
    let operatingSystem: String
    /// 脱敏后的业务动作类别，例如导入文件或导出容器。
    let operation: String
    /// 失败阶段中文标签。
    let phase: String
    /// 稳定错误类别代码，不包含原始路径和文件名。
    let failureCode: String
    /// 可直接展示给支持人员的安全摘要。
    let summary: String
    /// 已完成文件数。
    let completedFiles: UInt64
    /// 任务计划文件总数。
    let totalFiles: UInt64
    /// 因重复或用户选择而跳过的文件数。
    let skippedFiles: UInt64
    /// 已完成冲突检查的文件数。
    let checkedFiles: UInt64
    /// 尚待处理的文件数。
    let pendingFiles: UInt64
    /// 正等待冲突决定的文件数。
    let waitingFiles: UInt64
    /// 是否已完成源目录扫描。
    let scanComplete: Bool
    /// 本次任务配置的并行工作线程数。
    let parallelWorkers: UInt64
}

/// 将传输故障摘要保存为 JSON，供界面查看最近故障或导出给支持人员。
/// 无实例和共享可变内存；目录与条数策略为静态配置，磁盘读写没有额外互斥锁。
enum TransferDiagnostics {
    /// 读取候选和写入后修剪的目标上限；删除失败或并发写入时不保证目录严格只有 20 条。
    private static let maximumRecords = 20
    /// 首次访问时确定诊断目录：优先绝对路径 VEIL_APP_DATA_DIR，否则使用用户 Application Support。
    private static let directory: URL = {
        if let root = ProcessInfo.processInfo.environment["VEIL_APP_DATA_DIR"], root.hasPrefix("/") {
            return URL(fileURLWithPath: root, isDirectory: true)
                .appendingPathComponent("diagnostics", isDirectory: true)
        }
        return FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Veil/diagnostics", isDirectory: true)
    }()

    /// 为一次传输失败生成可保存和分享的脱敏摘要。
    /// - Parameters: error: Error 失败原因，仅在内存匹配锁竞争关键词；operation: String 服务动作键；progress: TransferProgress? 当时快照，nil 时计数为零、扫描状态 false、阶段未知。
    /// - Returns: TransferDiagnosticRecord，含新 UUID、当前时间、应用系统版本、白名单动作和阶段及进度计数。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 错误分类是文本启发式判断，可能误判；不保存原文，调用方提供的进度不代表失败瞬间的原子快照。
    static func makeRecord(error: Error, operation: String, progress: TransferProgress?) -> TransferDiagnosticRecord {
        // 原始错误只在内存中用于关键字分类，不作为记录字段保存；关键字匹配不是 SQLite 错误码校验。
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

    /// 为故障详情界面读取最近一条可解码的本地摘要。
    /// - Parameters: 无。
    /// - Returns: TransferDiagnosticRecord?；按文件名倒序检查至多 20 个 .json 条目，返回第一条有效记录，没有则 nil。
    /// - Throws: 不向调用方抛错；目录不存在不记日志，其他读取和解码失败尝试收集。
    /// - 注意事项: 按保存文件名排序，不使用记录 occurredAt 或文件修改时间；第 21 条之后即使有效也不读取。
    static func latestRecord() -> TransferDiagnosticRecord? {
        // 枚举结果每项是一个目录条目 URL；稍后仅按 .json 后缀筛选，不预先确认一定是普通文件。
        let files: [URL]
        do {
            files = try FileManager.default.contentsOfDirectory(
                at: directory, includingPropertiesForKeys: nil)
        } catch {
            let native = error as NSError
            // 尚未发生过传输故障时，诊断目录不存在属于正常状态。
            if native.domain != NSCocoaErrorDomain || native.code != NSFileReadNoSuchFileError {
                AppLog.capture(error)
            }
            return nil
        }
        let decoder = JSONDecoder()
        // 按文件名倒序查看最近记录；遍历目的在于跳过损坏文件，找到首条可用记录。
        for file in files.filter({ $0.pathExtension == "json" })
            .sorted(by: { $0.lastPathComponent > $1.lastPathComponent }).prefix(maximumRecords) {
            if let record = AppLog.attempt({
                try decoder.decode(TransferDiagnosticRecord.self, from: Data(contentsOf: file))
            }) { return record }
        }
        return nil
    }

    /// 保存本次故障摘要，并尽力减少旧记录占用。
    /// - Parameters: record: TransferDiagnosticRecord 要保存的摘要，调用方应提供可信的脱敏字段和安全 id。
    /// - Returns: Void；保存或权限设置失败被记录，修剪失败也不向调用方报告。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 原子替换只针对单个文件，不覆盖整个写入与修剪事务；无并发锁，不能严格保证条数。写入后设置 0600 失败时文件仍可能存在。
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

            // 收集候选条目，按文件名中的保存时间倒序保留；删除失败只记录，不中断后续修剪。
            let files = (AppLog.attempt({ try FileManager.default.contentsOfDirectory(
                at: directory,
                includingPropertiesForKeys: [.contentModificationDateKey]
            ) })) ?? []
            // 记录数超过上限时保留最新项，逐个删除更旧的 JSON 诊断文件。
            for file in files.filter({ $0.pathExtension == "json" })
                .sorted(by: { $0.lastPathComponent > $1.lastPathComponent })
                .dropFirst(maximumRecords) {
                AppLog.attempt({ try FileManager.default.removeItem(at: file) })
            }
        } catch { AppLog.capture(error);
            // 诊断记录失败不能改变原任务的结果。
        }
    }

    /// 把用户选择的故障摘要保存为可分享的 JSON 文件。
    /// - Parameters: record: TransferDiagnosticRecord 选中的摘要；url: URL 用户选择的保存目标。
    /// - Returns: Void，编码和原子写入成功后结束。
    /// - Throws: JSON 编码或文件写入失败时传播底层错误。
    /// - 注意事项: 可能替换目标已有文件；不创建父目录，不额外设置权限或申请安全作用域访问，调用方负责目标授权。
    static func export(_ record: TransferDiagnosticRecord, to url: URL) throws {
        try encoded(record).write(to: url, options: .atomic)
    }

    /// 生成便于支持人员阅读和比较的诊断 JSON。
    /// - Parameters: record: TransferDiagnosticRecord 待编码摘要。
    /// - Returns: Data，缩进排版且对象键排序的 JSON 字节。
    /// - Throws: JSONEncoder 编码失败时传播错误。
    /// - 注意事项: 只编码现有字段，不再过滤敏感文本；调用方不能把任意原始错误正文填入记录。
    private static func encoded(_ record: TransferDiagnosticRecord) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return try encoder.encode(record)
    }

    /// 把服务动作键限制为可公开展示的业务类别。
    /// - Parameters: operation: String 动作键，识别 import、export、import_package、export_package。
    /// - Returns: String，已知键对应中文标签，其他输入统一为“文件传输任务”。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 不回显未知输入，避免动作字段夹带路径或用户文本。
    private static func operationLabel(_ operation: String) -> String {
        switch operation {
        case "import": "导入文件"
        case "export": "导出文件"
        case "import_package": "导入容器"
        case "export_package": "导出容器"
        default: "文件传输任务"
        }
    }

    /// 把传输阶段键限制为支持人员可读的固定标签。
    /// - Parameters: phase: String? 服务阶段；支持 preparing、processing、verifying、snapshot、packing、unpacking、finalizing、cancelled。
    /// - Returns: String，对应中文阶段；nil 或未知键返回“未知”。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 标签用于诊断展示，不据此控制任务执行或推断任务必然成功。
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
