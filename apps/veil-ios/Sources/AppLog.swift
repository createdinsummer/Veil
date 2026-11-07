import Foundation

/// 客户端只向 core 传递安全分类与错误码，不持久化可能包含用户数据的异常文本。
enum AppLog {
    static func capture(_ error: Error, file: StaticString = #fileID, line: UInt = #line) {
        if error is CapturedError || isExpectedCancellation(error) { return }
        let native = error as NSError
        // 服务错误已经在服务边界收集，避免客户端重复记录。
        if case ClientError.service = error { return }
        if case ClientError.unsupportedContent = error { return }
        let category: UInt32
        switch error {
        case is EncodingError: category = 1
        case is DecodingError: category = 2
        case ClientError.invalidResponse: category = 4
        default: category = native.domain == NSCocoaErrorDomain ? 3 : 0
        }
        var message = diagnosticDescription(error)
        var cause = native.userInfo[NSUnderlyingErrorKey] as? NSError
        for _ in 0..<7 {
            guard let current = cause else { break }
            message += "\nCaused by: " + diagnosticDescription(current)
            cause = current.userInfo[NSUnderlyingErrorKey] as? NSError
        }
        record(category, file: file, line: line, nativeCode: Int64(native.code), message: message)
    }

    private static func isExpectedCancellation(_ error: Error) -> Bool {
        if error is CancellationError { return true }
        let native = error as NSError
        return (native.domain == NSCocoaErrorDomain && native.code == NSUserCancelledError)
            || (native.domain == NSURLErrorDomain && native.code == NSURLErrorCancelled)
    }

    /// 错误描述、解码字段名和 NSError.userInfo 都可能含有原始内容，使用白名单分类。
    private static func diagnosticDescription(_ error: Error) -> String {
        switch error {
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
        case NSCocoaErrorDomain: domain = "Cocoa"
        case NSPOSIXErrorDomain: domain = "POSIX"
        case NSURLErrorDomain: domain = "URL"
        case NSOSStatusErrorDomain: domain = "OSStatus"
        default: domain = "local"
        }
        return "客户端异常：\(domain) code=\(native.code)"
    }

    private struct CapturedError: LocalizedError {
        let underlying: Error
        var errorDescription: String? { underlying.localizedDescription }
    }

    /// 在服务适配层记录后添加内存标记，避免错误向上传递时被重复收集。
    static func recorded(_ error: Error, file: StaticString = #fileID, line: UInt = #line) -> Error {
        if error is CapturedError || isExpectedCancellation(error) { return error }
        if case ClientError.service = error { return error }
        if case ClientError.unsupportedContent = error { return error }
        capture(error, file: file, line: line)
        return CapturedError(underlying: error)
    }

    static func validation(_ message: String, file: StaticString = #fileID, line: UInt = #line)
        -> String
    {
        // 正常业务校验仅提示用户，不写入故障日志。
        return message
    }

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
    static func attempt<T>(file: StaticString = #fileID, line: UInt = #line, _ action: () throws -> T)
        -> T?
    {
        do { return try action() } catch {
            capture(error, file: file, line: line)
            return nil
        }
    }

    static func attemptAsync<T>(
        file: StaticString = #fileID, line: UInt = #line, _ action: () async throws -> T
    ) async -> T? {
        do { return try await action() } catch {
            capture(error, file: file, line: line)
            return nil
        }
    }

    static func flush() { veil_logs_flush() }

    static func export() throws -> Data {
        let buffer = veil_logs_export()
        defer { veil_buffer_free(buffer) }
        guard buffer.kind == 0, let data = buffer.data else {
            throw ClientError.message("无法导出错误日志")
        }
        return Data(bytes: data, count: buffer.length)
    }
}
