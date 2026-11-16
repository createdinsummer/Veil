/// ============================================================================
/// Veil iOS 客户端 · 应用日志（AppLog）
///
/// 文件作用：客户端错误收集与上报入口。所有异常先在这里按“白名单分类”脱敏，
/// 再通过 C ABI（跨语言函数调用接口）交给 Rust 的 veil_log_client_error 持久化。
/// 所属模块：iOS 客户端基础设施层（日志 / 错误处理）。
/// 文件内核心成员清单：
///   - capture            记录一次客户端错误（过滤预期取消与重复记录）
///   - isExpectedCancellation 识别“用户主动取消”类正常交互
///   - diagnosticDescription  白名单化的错误描述（不回显用户数据）
///   - CapturedError      内存中的“已记录”标记错误
///   - recorded           记录后包上标记，避免向上传递时重复收集
///   - validation         业务校验提示（只展示不记日志）
///   - record             底层 C 调用上报
///   - attempt/attemptAsync 失败只记录不抛出的安全执行包装
///   - flush/export       日志落盘与导出
/// 依赖能力：Foundation（NSError 等）、veil_bridge.h 的日志 C ABI。
/// 整体业务目标：保证故障可诊断（有日志可导出）的同时，绝不把密码、文件名、
///   路径等可能包含用户数据的文本落进日志。
/// ============================================================================
import Foundation

/// 客户端只向 core 传递安全分类与错误码，不持久化可能包含用户数据的异常文本。
/// 补充：错误描述、解码字段名和 NSError.userInfo 都可能带原始内容，因此全部走
///   白名单分类，只把“类别 + 来源文件 + 行号 + 错误码”交给 Rust 持久化。
enum AppLog {
    /// 为客户端故障提交不含原始错误文案的分类信息。
    /// - Parameters: error: Error 原始错误；file: StaticString 来源文件标识，默认 #fileID；line: UInt 来源行号，默认 #line。
    /// - Returns: Void；已包装、取消、服务错误和不支持内容直接跳过，其余提交给 Rust 日志入口。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 不保证持久化成功；最多读取七层底层错误，只输出白名单分类及数值码，不输出 userInfo 或原始域文本。
    static func capture(_ error: Error, file: StaticString = #fileID, line: UInt = #line) {
        // 已经带“已记录”标记（CapturedError）或属于预期取消的错误，不重复记录。
        if error is CapturedError || isExpectedCancellation(error) { return }
        let native = error as NSError
        // 服务错误已经在服务边界收集，避免客户端重复记录。
        if case ClientError.service = error { return }
        if case ClientError.unsupportedContent = error { return }
        // 错误类别编码：1 编码失败、2 解码失败、4 无效响应、3 系统框架错误、0 其他。
        let category: UInt32
        switch error {
        case is EncodingError: category = 1
        case is DecodingError: category = 2
        case ClientError.invalidResponse: category = 4
        default: category = native.domain == NSCocoaErrorDomain ? 3 : 0
        }
        // 主错误 + 最多 7 层 NSUnderlyingError 因果链，每层都只用白名单描述。
        var message = diagnosticDescription(error)
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
        // Swift 并发取消、系统文件选择器取消、网络请求取消都视为预期取消。
        if error is CancellationError { return true }
        let native = error as NSError
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
        // 编码 / 解码错误只给固定类别名，不回显具体字段与值。
        case EncodingError.invalidValue: return "编码失败：invalid_value"
        case DecodingError.keyNotFound: return "解码失败：missing_key"
        case DecodingError.typeMismatch: return "解码失败：type_mismatch"
        case DecodingError.valueNotFound: return "解码失败：missing_value"
        case DecodingError.dataCorrupted: return "解码失败：corrupted_data"
        default: break
        }
        // 常见系统错误域映射成短名，未知域统一记 "local"。
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

    /// 内存中的“已记录”标记错误：包住原始错误继续向上抛，避免各层重复收集。
    private struct CapturedError: LocalizedError {
        /// 被包装的原始错误。
        let underlying: Error
        /// 对外仍呈现原始错误文案，上层调用方无感知。
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
        // withCString 把 Swift 字符串转成 NUL 结尾的 C 字符串，供桥接函数读取。
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
    /// - Throws: Rust 返回非成功类型或空数据指针时抛出 ClientError.message("无法导出错误日志")。
    /// - 注意事项: 依赖 Rust 日志服务；导出内容不是只限本次客户端错误，本方法不再执行脱敏或验证 JSON 结构。
    static func export() throws -> Data {
        let buffer = veil_logs_export()
        // 缓冲区用完必须归还 Rust，否则造成内存泄漏。
        defer { veil_buffer_free(buffer) }
        guard buffer.kind == 0, let data = buffer.data else {
            throw ClientError.message("无法导出错误日志")
        }
        return Data(bytes: data, count: buffer.length)
    }
}
