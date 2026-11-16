/// 文件预览临时空间预检与错误文案。
/// 所属模块：客户端预览前置检查；估算解密明文副本所需空间，识别磁盘写满错误并生成可操作提示。
/// 核心数据：reserveBytes 是为文件系统与并发写入保留的额外容量；其余函数按文件大小和可用容量返回提示。
/// 依赖 Darwin / Foundation 的卷容量、POSIX errno 与字节格式化能力。整体目标是尽量在解密前发现空间不足，并在真实写入失败时保留原错误信息。
import Darwin
import Foundation

/// 业务定位：为“预览 / 打开”估算整份明文临时副本所需的磁盘容量，并统一转换空间不足提示。
/// 数据与职责：只保存固定容量余量，其余方法读取容量或计算文案，不保存预览文件和用户状态。
/// 使用场景：解密前预检、解密写入失败后生成提示；容量检查只是瞬时快照，不能预留实际空间。
enum PreviewStorage {
    /// 预检余量：文件系统元数据、系统预留空间与其他进程的并发写入。
    static let reserveBytes: UInt64 = 256 * 1024 * 1024

    /// 功能：查询预览临时目录所在卷的容量，供解密前估算是否容得下完整明文副本。
    /// - Parameter directory: URL 类型，预览临时文件将写入的本地目录。
    /// - Returns: UInt64 类型的正数可用字节数；读取失败、容量缺失或容量为零时返回 nil。
    /// - Throws: 不向外抛错，系统资源查询错误由 try? 转为 nil。
    /// - Note: 零容量也会返回 nil 并跳过预检；之后仍须处理真正的写入失败。
    static func availableBytes(in directory: URL) -> UInt64? {
        // 只读取卷容量，不创建明文文件；读取权限或卷信息不可用时交给写入阶段处理。
        let values = try? directory.resourceValues(forKeys: [.volumeAvailableCapacityKey])
        guard let capacity = values?.volumeAvailableCapacity, capacity > 0 else { return nil }
        return UInt64(capacity)
    }

    /// 功能：在已知磁盘容量小于“明文大小 + 预留余量”时，生成阻止本次预览的提示。
    /// - Parameters:
    ///   - fileSize: UInt64 类型，待解密文件的完整明文字节数，允许为零。
    ///   - availableBytes: UInt64? 类型，目标卷的可用字节数；nil 表示无法预检。
    ///   - action: String 类型，提示中的用户动作，默认“预览”，也可传“打开”。
    /// - Returns: 容量不足时返回 String 提示；容量未知或大于等于需求时返回 nil。
    /// - Throws: 不抛出错误。
    /// - Note: nil 代表本次预检未阻止操作，不保证实际写入一定成功；并发写入可能消耗剩余空间。
    static func insufficientSpaceMessage(
        fileSize: UInt64, availableBytes: UInt64?, action: String = "预览"
    ) -> String? {
        guard let availableBytes else { return nil }
        // 需求包含固定余量；加法溢出时以最大值估计，避免低估超大文件。
        let needed = requiredBytes(for: fileSize)
        guard availableBytes < needed else { return nil }
        return storageMessage(needed: needed, available: availableBytes, fileSize: fileSize, action: action)
    }

    /// 功能：把预览写入阶段的空间不足错误转换成可操作提示，其余错误保留原描述。
    /// - Parameters:
    ///   - error: Error 类型，临时明文写入或服务层包装后传回的错误。
    ///   - fileSize: UInt64 类型，本次完整明文副本的字节数。
    ///   - action: String 类型，发生失败的用户动作，默认“预览”。
    /// - Returns: 识别为空间不足时返回容量提示，否则返回 error.localizedDescription。
    /// - Throws: 不重新抛出 error，也不执行重试。
    /// - Note: 本函数只转换文案，临时文件清理仍由预览流程负责。
    static func failureMessage(for error: Error, fileSize: UInt64, action: String = "预览")
        -> String
    {
        guard isStorageFull(error) else { return error.localizedDescription }
        return storageMessage(
            needed: requiredBytes(for: fileSize), available: nil, fileSize: fileSize, action: action)
    }

    /// 计算文件大小加预留空间后的预计写入量。
    /// - Parameter fileSize: 明文文件字节数。
    /// - Returns: UInt64 类型的需求字节数；加法溢出时返回 UInt64.max。
    /// - Throws: 不抛出错误，溢出通过标志处理。
    /// - Note: 上限值只是保守估计；当传入可用容量同为 UInt64.max 时，上层比较不会判为不足。
    static func requiredBytes(for fileSize: UInt64) -> UInt64 {
        let (sum, overflow) = fileSize.addingReportingOverflow(reserveBytes)
        return overflow ? UInt64.max : sum
    }

    /// 功能：为容量预检和真实写满错误生成统一的用户提示。
    /// - Parameters:
    ///   - needed: UInt64 类型，包含余量的预计需求字节数。
    ///   - available: UInt64? 类型，可用字节数；nil 时省略剩余容量说明。
    ///   - fileSize: UInt64 类型，明文文件自身的字节数，用于解释额外空间需求。
    ///   - action: String 类型，用户正在执行的动作名称。
    /// - Returns: 包含预计需求、文件大小及清理建议的 String。
    /// - Throws: 不抛出错误。
    /// - Note: 仅生成文案，不查询容量或清理用户文件。
    private static func storageMessage(
        needed: UInt64, available: UInt64?, fileSize: UInt64, action: String
    ) -> String {
        let requirement = "\(action)需约 \(format(needed)) 临时空间（文件 \(format(fileSize))）"
        guard let available else {
            return "磁盘空间不足：\(requirement)。请清理磁盘空间，或导出到其他磁盘后打开。"
        }
        return "磁盘空间不足：\(requirement)，此磁盘仅剩 \(format(available))。请清理磁盘空间，或导出到其他磁盘后打开。"
    }

    /// 功能：将容量数字转换为系统本地化的文件大小文案。
    /// - Parameter bytes: UInt64 类型的字节数。
    /// - Returns: String 类型的容量文案，单位由系统按文件大小选择。
    /// - Throws: 不抛出错误。
    /// - Note: 超过 Int64.max 的值先截到该上限，显示值可能低于实际 UInt64 容量。
    private static func format(_ bytes: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(clamping: bytes), countStyle: .file)
    }

    /// 功能：识别系统或服务层包装的空间不足错误，决定是否替换成容量提示。
    /// - Parameter error: Error 类型，写入阶段返回的任意错误。
    /// - Returns: 最多检查 8 个 NSError 节点，命中 POSIX / Cocoa 错误码或指定文本时为 true，否则 false。
    /// - Throws: 不抛出错误。
    /// - Note: 服务层有时只保留错误文本，因此保留字符串兜底；它不是对所有错误格式的完整解析。
    private static func isStorageFull(_ error: Error) -> Bool {
        // current 保存当前包装层；每轮只检查这一层，然后沿 NSUnderlyingErrorKey 走向底层错误。
        var current: NSError? = error as NSError
        // 服务层可能层层包装 I/O 错误，按底层错误链逐层检查稳定错误码和文案。
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
            // 未命中当前节点时检查下一层；没有底层错误或链形成环时，由空值或八层上限结束。
            current = node.userInfo[NSUnderlyingErrorKey] as? NSError
        }
        return false
    }
}
