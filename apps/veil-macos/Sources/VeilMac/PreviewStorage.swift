import Darwin
import Foundation

/// 预览会把整份明文解密写入应用临时目录，所需空间等于文件大小（另加安全余量）。
/// 预检用于在开始解密前拦住注定失败的写入，而不是等写到一半才拿到 ENOSPC。
enum PreviewStorage {
    /// 预检余量：文件系统元数据、系统预留空间与其他进程的并发写入。
    static let reserveBytes: UInt64 = 256 * 1024 * 1024

    /// 读取目录所在卷的原始可用空间；读不到时返回 nil，调用方跳过预检。
    static func availableBytes(in directory: URL) -> UInt64? {
        let values = try? directory.resourceValues(forKeys: [.volumeAvailableCapacityKey])
        guard let capacity = values?.volumeAvailableCapacity, capacity > 0 else { return nil }
        return UInt64(capacity)
    }

    /// 空间足够（或无法判断）时返回 nil；不足时返回面向用户的提示。
    /// `action` 是用户正在做的动作（预览、打开），用于生成提示文案。
    static func insufficientSpaceMessage(
        fileSize: UInt64, availableBytes: UInt64?, action: String = "预览"
    ) -> String? {
        guard let availableBytes else { return nil }
        let needed = requiredBytes(for: fileSize)
        guard availableBytes < needed else { return nil }
        return storageMessage(needed: needed, available: availableBytes, fileSize: fileSize, action: action)
    }

    /// 写入阶段仍收到 ENOSPC 时的兜底提示；其余错误保持原有描述。
    static func failureMessage(for error: Error, fileSize: UInt64, action: String = "预览")
        -> String
    {
        guard isStorageFull(error) else { return error.localizedDescription }
        return storageMessage(
            needed: requiredBytes(for: fileSize), available: nil, fileSize: fileSize, action: action)
    }

    static func requiredBytes(for fileSize: UInt64) -> UInt64 {
        let (sum, overflow) = fileSize.addingReportingOverflow(reserveBytes)
        return overflow ? UInt64.max : sum
    }

    private static func storageMessage(
        needed: UInt64, available: UInt64?, fileSize: UInt64, action: String
    ) -> String {
        let requirement = "\(action)需约 \(format(needed)) 临时空间（文件 \(format(fileSize))）"
        guard let available else {
            return "磁盘空间不足：\(requirement)。请清理磁盘空间，或导出到其他磁盘后打开。"
        }
        return "磁盘空间不足：\(requirement)，此磁盘仅剩 \(format(available))。请清理磁盘空间，或导出到其他磁盘后打开。"
    }

    private static func format(_ bytes: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: bytes), countStyle: .file)
    }

    /// 服务层把 ENOSPC 包装成 `I/O 错误: No space left on device (os error 28)`，
    /// 客户端只能按错误链与稳定片段识别，其余错误保持原样。
    private static func isStorageFull(_ error: Error) -> Bool {
        var current: NSError? = error as NSError
        for _ in 0..<8 {
            guard let node = current else { return false }
            if node.domain == NSPOSIXErrorDomain, node.code == Int(ENOSPC) { return true }
            if node.domain == NSCocoaErrorDomain, node.code == NSFileWriteOutOfSpaceError {
                return true
            }
            let description = node.localizedDescription
            if description.contains("No space left on device") || description.contains("os error 28") {
                return true
            }
            current = node.userInfo[NSUnderlyingErrorKey] as? NSError
        }
        return false
    }
}
