/// ============================================================================
/// Veil macOS 客户端 · 错误日志收集（AppLog）
///
/// 文件作用：把客户端异常统一收集后交给 Rust 侧（veil_log_client_error）持久化，
///   并提供「错误已收集」标记、日志落盘（flush）与导出（export）能力。
/// 所属模块：客户端服务层（日志与诊断基础设施）。
/// 文件内核心类型清单：
///   - AppLog        日志工具枚举（无实例，只提供静态方法）
///   - CapturedError 已收集错误的包装，避免带此包装的错误重复提交
/// 依赖能力：Foundation（NSError）、CVeilBridge（C ABI 日志函数）。
/// 整体业务目标：故障日志只保存安全的错误分类与错误码，绝不持久化可能
///   包含用户数据（文件名、路径、密码）的原始错误文案；正常业务校验不落日志。
/// ============================================================================
import CVeilBridge
import Foundation

/// 客户端只向 core 传递安全分类与错误码，不持久化可能包含用户数据的异常文本。
enum AppLog {
    /// 为客户端故障提交不含原始错误文案的分类信息。
    /// - Parameters: error: Error 原始错误；file: StaticString 来源文件标识，默认 #fileID；line: UInt 来源行号，默认 #line。
    /// - Returns: Void；已包装、取消、服务错误和不支持内容直接跳过，其余提交给 Rust 日志入口。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 不保证持久化成功；最多读取七层底层错误，只输出白名单分类及数值码，不输出 userInfo 或原始域文本。
    static func capture(_ error: Error, file: StaticString = #fileID, line: UInt = #line) {
        // 已收集过或属于用户主动取消的错误，不再重复记录。
        if error is CapturedError || isExpectedCancellation(error) { return }
        let native = error as NSError
        // 服务错误已经在服务边界收集，避免客户端重复记录。
        if case ClientError.service = error { return }
        // 不支持的内容（如无法解码的预览）属于业务提示，同样不落故障日志。
        if case ClientError.unsupportedContent = error { return }
        let category: UInt32
        switch error {
        // 编码/解码失败与无效响应分别归类，便于日志侧聚合统计。
        case is EncodingError: category = 1
        case is DecodingError: category = 2
        case ClientError.invalidResponse: category = 4
        // Cocoa 错误归为 3，其余客户端错误归为 0。
        default: category = native.domain == NSCocoaErrorDomain ? 3 : 0
        }
        // 只拼「客户端异常：域 code=码」这类脱敏描述，原始文案不进日志。
        var message = diagnosticDescription(error)
        // 沿 NSUnderlyingErrorKey 错误链最多追溯 7 层，拼出完整原因链（仍然脱敏）。
        var cause = native.userInfo[NSUnderlyingErrorKey] as? NSError
        for _ in 0..<7 {
            guard let current = cause else { break }
            message += "\nCaused by: " + diagnosticDescription(current)
            cause = current.userInfo[NSUnderlyingErrorKey] as? NSError
        }
        record(category, file: file, line: line, nativeCode: Int64(native.code), message: message)
    }

    /// 区分正常取消与需要诊断的故障。
    /// - Parameters: error: Error 待分类错误。
    /// - Returns: Bool；Swift CancellationError 或 Cocoa/URL 指定取消码为 true，其余 false。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 取消码只能说明错误分类，不能保证操作由用户主动发起。
    private static func isExpectedCancellation(_ error: Error) -> Bool {
        if error is CancellationError { return true }
        let native = error as NSError
        // Cocoa / URL 两套取消码都视为正常取消。
        return (native.domain == NSCocoaErrorDomain && native.code == NSUserCancelledError)
            || (native.domain == NSURLErrorDomain && native.code == NSURLErrorCancelled)
    }

    /// 生成可提交日志的固定类别说明，避免泄露错误正文。
    /// - Parameters: error: Error 需要描述的错误。
    /// - Returns: String；编码解码错误返回固定标签，其他错误返回白名单域短名和数字码，未知域统一 local。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 不使用 localizedDescription，不包含解码字段名；仅用于诊断摘要。
    private static func diagnosticDescription(_ error: Error) -> String {
        switch error {
        // 编码/解码错误只报类别，不带具体值（值里可能含用户数据）。
        case EncodingError.invalidValue: return "编码失败：invalid_value"
        case DecodingError.keyNotFound: return "解码失败：missing_key"
        case DecodingError.typeMismatch: return "解码失败：type_mismatch"
        case DecodingError.valueNotFound: return "解码失败：missing_value"
        case DecodingError.dataCorrupted: return "解码失败：corrupted_data"
        default: break
        }
        let native = error as NSError
        let domain: String
        switch native.domain {
        // 把系统错误域映射成简短可读名称，域与码之外的信息一律丢弃。
        case NSCocoaErrorDomain: domain = "Cocoa"
        case NSPOSIXErrorDomain: domain = "POSIX"
        case NSURLErrorDomain: domain = "URL"
        case NSOSStatusErrorDomain: domain = "OSStatus"
        default: domain = "local"
        }
        return "客户端异常：\(domain) code=\(native.code)"
    }

    /// 已收集错误的包装：标记这条错误已提交日志收集，向上传递时不再重复收集。
    private struct CapturedError: LocalizedError {
        /// 原始错误对象，供界面继续显示原文；不会作为日志正文提交。
        let underlying: Error
        // 对外仍暴露原始错误的本地化文案，调用方看到的错误文字不变。
        var errorDescription: String? { underlying.localizedDescription }
    }

    /// 在错误继续向上传递前提交日志，并附加防重复提交标记。
    /// - Parameters: error: Error 原始错误；file: StaticString 来源文件标识，默认 #fileID；line: UInt 来源行号，默认 #line。
    /// - Returns: Error；已包装或被过滤的错误原样返回，其余包装为 CapturedError，界面仍可读取原始本地化描述。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 标记表示已调用收集入口，不表示日志已落盘；同一个未包装原始错误再次传入仍可能再次提交。
    static func recorded(_ error: Error, file: StaticString = #fileID, line: UInt = #line) -> Error {
        if error is CapturedError || isExpectedCancellation(error) { return error }
        if case ClientError.service = error { return error }
        if case ClientError.unsupportedContent = error { return error }
        capture(error, file: file, line: line)
        return CapturedError(underlying: error)
    }

    /// 把正常业务校验说明交给界面展示。
    /// - Parameters: message: String 用户可读提示；file: StaticString 和 line: UInt 为兼容调用接口保留，默认调用位置，目前不使用。
    /// - Returns: String，原样返回 message。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 不执行日志提交，也不校验或脱敏文案；调用方负责提示内容。
    static func validation(_ message: String, file: StaticString = #fileID, line: UInt = #line)
        -> String
    {
        // 正常业务校验仅提示用户，不写入故障日志。
        return message
    }

    /// 通过跨语言接口提交已筛选的客户端诊断信息。
    /// - Parameters: category: UInt32 分类码，当前 0 其他、1 编码、2 解码、3 Cocoa、4 无效响应；file: StaticString 来源；line: UInt 行号；nativeCode: Int64 系统错误码，默认 0；message: String 白名单摘要。
    /// - Returns: Void；C 接口不向 Swift 返回写入结果。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 调用方必须先过滤敏感内容；C 字符串仅在 withCString 闭包期间有效，行号钳制到 UInt32 范围。
    private static func record(
        _ category: UInt32, file: StaticString, line: UInt, nativeCode: Int64 = 0, message: String
    ) {
        String(describing: file).withCString {
            source in
            message.withCString { text in
                veil_log_client_error(category, source, UInt32(clamping: line), nativeCode, text)
            }
        }
    }

    @discardableResult
    /// 执行允许失败的同步辅助或清理动作，避免错误打断主流程。
    /// - Parameters: file: StaticString 来源，默认 #fileID；line: UInt 行号，默认 #line；action: () throws -> T 同步业务闭包，T 为调用方结果类型。
    /// - Returns: T?；成功返回闭包结果，抛错时按 capture 规则提交并返回 nil。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 不会重试或回滚闭包已经产生的副作用；取消错误被吞掉且不记日志。
    static func attempt<T>(file: StaticString = #fileID, line: UInt = #line, _ action: () throws -> T)
        -> T?
    {
        do { return try action() } catch {
            capture(error, file: file, line: line)
            return nil
        }
    }

    /// 执行允许失败的异步辅助动作，并把失败转换为空结果。
    /// - Parameters: file: StaticString 来源，默认 #fileID；line: UInt 行号，默认 #line；action: () async throws -> T 可等待的业务闭包。
    /// - Returns: T?；成功返回结果，抛错时尝试收集并返回 nil。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 不创建独立后台任务；沿当前异步调用执行，取消错误也转为 nil，调用方需自行决定是否继续。
    static func attemptAsync<T>(
        file: StaticString = #fileID, line: UInt = #line, _ action: () async throws -> T
    ) async -> T? {
        do { return try await action() } catch {
            capture(error, file: file, line: line)
            return nil
        }
    }

    /// 在锁定或生命周期边界请求 Rust 排空日志缓冲。
    /// - Parameters: 无。
    /// - Returns: Void；不返回底层处理状态。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 同步跨语言调用，可能等待日志处理；不保证磁盘物理同步成功，失败不会通过此接口报告。
    static func flush() { veil_logs_flush() }

    /// 读取 Rust 日志导出结果，供用户保存故障反馈附件。
    /// - Parameters: 无。
    /// - Returns: Data，从 Rust 缓冲区复制的日志 JSON 字节；无论成功或失败都会归还原缓冲区。
    /// - Throws: Rust 返回非成功类型或空数据指针时抛出 ClientError.invalidResponse。
    /// - 注意事项: 依赖 Rust 日志服务；导出内容不是只限本次客户端错误，本方法不再执行脱敏或验证 JSON 结构。
    static func export() throws -> Data {
        let buffer = veil_logs_export()
        // C ABI 返回的缓冲区用完必须归还，避免内存泄漏。
        defer { veil_buffer_free(buffer) }
        guard buffer.kind == 0, let data = buffer.data else {
            throw ClientError.invalidResponse
        }
        return Data(bytes: data, count: buffer.length)
    }
}
