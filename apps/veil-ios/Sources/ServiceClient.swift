import Foundation

struct NativeRequest: Encodable, Sendable {
    var operation: String
    var id: String? = nil
    var resolutions: [ImportResolution]? = nil
    var space: String? = nil
    var name: String? = nil
    var password: String? = nil
    var path: String? = nil
    var names: [String]? = nil
    var sources: [String]? = nil
    var directory: String? = nil
    var destination: String? = nil
}

struct ImportConflict: Decodable, Identifiable, Sendable {
    let target: String
    let sameContent: Bool
    let isDirectory: Bool
    let error: String?
    var canOverwrite: Bool { !isDirectory && error == nil }
    var id: String { target }
    var detail: String {
        if let error { return "无法确定：\(error)" }
        return isDirectory ? "文件与文件夹类型冲突，只能跳过" : (sameContent ? "内容相同" : "内容不同")
    }
}

struct ImportInspection: Decodable, Identifiable, Sendable {
    let id: String
    let totalFiles: Int
    let conflicts: [ImportConflict]
}

enum ImportAction: String, Encodable, Sendable { case overwrite, skip }
struct ImportResolution: Encodable, Sendable {
    let target: String
    let action: ImportAction
}

enum ClientError: LocalizedError {
    case message(String)
    case service(String)
    case unsupportedContent(String)
    case invalidResponse
    var errorDescription: String? {
        switch self {
        case .message(let text), .service(let text), .unsupportedContent(let text): return text
        case .invalidResponse: return "服务响应无效"
        }
    }
}

struct ImportTransferProgress: Decodable {
    let operation: String
    let phase: String
    let completed_files: UInt64
    let skipped_files: UInt64
    let total_files: UInt64
    let scan_complete: Bool
    let waiting_files: UInt64
    let checked_files: UInt64
}

private final class RustHandle: @unchecked Sendable {
    let pointer: OpaquePointer?
    init() {
        let directory = FileManager.default.urls(
            for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent(
                "Veil", isDirectory: true)
        pointer =
            directory.path.withCString({ veil_configure_app_data_dir($0) }) ? veil_client_new() : nil
    }
    deinit { if let pointer { veil_client_free(pointer) } }
}

actor ServiceClient {
    nonisolated private let rust = RustHandle()
    nonisolated func cancelTransfer() -> Bool {
        guard let pointer = rust.pointer else { return false }
        return veil_client_cancel_transfer(pointer)
    }
    nonisolated func transferProgress() -> ImportTransferProgress? {
        guard let pointer = rust.pointer else { return nil }
        let response = veil_client_progress(pointer)
        defer { veil_buffer_free(response) }
        guard response.kind == 0, let data = response.data else { return nil }
        let bytes = Data(bytes: data, count: response.length)
        if bytes == Data("null".utf8) { return nil }
        do { return try JSONDecoder().decode(ImportTransferProgress.self, from: bytes) } catch {
            AppLog.capture(error)
            return nil
        }
    }
    private func perform(_ request: NativeRequest) async throws -> (UInt32, Data) {
        let rust = self.rust
        do {
            return try await Task.detached {
                guard let handle = rust.pointer else { throw ClientError.message("无法初始化服务") }
                var input = try JSONEncoder().encode(request)
                defer { input.resetBytes(in: 0..<input.count) }
                let result = input.withUnsafeBytes {
                    veil_client_call(handle, $0.bindMemory(to: UInt8.self).baseAddress, input.count)
                }
                defer { veil_buffer_free(result) }
                guard let bytes = result.data else { throw ClientError.invalidResponse }
                let data = Data(bytes: bytes, count: result.length)
                if result.kind == 1 { throw ClientError.service(String(decoding: data, as: UTF8.self)) }
                return (result.kind, data)
            }.value
        } catch {
            throw AppLog.recorded(error)
        }
    }
    func json<T: Decodable>(_ request: NativeRequest, as: T.Type = T.self) async throws -> T {
        do {
            let (kind, data) = try await perform(request)
            guard kind == 0 else { throw ClientError.invalidResponse }
            return try JSONDecoder().decode(T.self, from: data)

        } catch {
            throw AppLog.recorded(error)
        }
    }
    func execute(_ request: NativeRequest) async throws {
        do {
            let (kind, _) = try await perform(request)
            guard kind == 0 || (kind == 4 && request.operation == "export_file") else {
                throw ClientError.invalidResponse
            }
        } catch {
            throw AppLog.recorded(error)
        }
    }
    /// 在服务调用队列中锁定容器，再清除应用私有数据。
    func clearLocalData() async throws {
        do {
            try await execute(NativeRequest(operation: "lock"))
            let files = FileManager.default
            let support = files.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            let caches = files.urls(for: .cachesDirectory, in: .userDomainMask)[0]
            try clearVeilLocalData(support: support, caches: caches, temporary: files.temporaryDirectory)

        } catch {
            throw AppLog.recorded(error)
        }
    }
    func preview(_ name: String) async throws -> Data {
        do {
            let (kind, data) = try await perform(NativeRequest(operation: "preview_file", name: name))
            guard kind == 2 || kind == 3 else { throw ClientError.invalidResponse }
            return data

        } catch {
            throw AppLog.recorded(error)
        }
    }
}

/// 仅清理当前应用私有目录；不触及用户导出的外部文件。
func clearVeilLocalData(support: URL, caches: URL, temporary: URL) throws {
    let files = FileManager.default
    let vaultData = support.appendingPathComponent("Veil", isDirectory: true)
    if files.fileExists(atPath: vaultData.path) { try files.removeItem(at: vaultData) }
    for directory in [caches, temporary] where files.fileExists(atPath: directory.path) {
        for item in try files.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil) {
            try files.removeItem(at: item)
        }
    }
}
