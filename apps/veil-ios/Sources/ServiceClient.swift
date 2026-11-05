import Foundation

struct NativeRequest: Encodable {
    var operation: String
    var space: String? = nil
    var name: String? = nil
    var password: String? = nil
    var path: String? = nil
    var sources: [String]? = nil
    var directory: String? = nil
    var destination: String? = nil
}

enum ClientError: LocalizedError {
    case message(String)
    var errorDescription: String? { if case .message(let text) = self { return text }; return nil }
}

actor ServiceClient {
    private var handle: OpaquePointer?
    init() {
        let directory = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent("Veil", isDirectory: true)
        if directory.path.withCString({ veil_configure_app_data_dir($0) }) { handle = veil_client_new() }
    }
    deinit { if let handle { veil_client_free(handle) } }
    private func perform(_ request: NativeRequest) throws -> (UInt32, Data) {
        guard let handle else { throw ClientError.message("无法初始化服务") }
        var input = try JSONEncoder().encode(request)
        defer { input.resetBytes(in: 0..<input.count) }
        let result = input.withUnsafeBytes { veil_client_call(handle, $0.bindMemory(to: UInt8.self).baseAddress, input.count) }
        defer { veil_buffer_free(result) }
        guard let bytes = result.data else { throw ClientError.message("服务响应无效") }
        let data = Data(bytes: bytes, count: result.length)
        if result.kind == 1 { throw ClientError.message(String(decoding: data, as: UTF8.self)) }
        return (result.kind, data)
    }
    func json<T: Decodable>(_ request: NativeRequest, as: T.Type = T.self) throws -> T {
        let (kind, data) = try perform(request)
        guard kind == 0 else { throw ClientError.message("服务响应无效") }
        return try JSONDecoder().decode(T.self, from: data)
    }
    func execute(_ request: NativeRequest) throws { _ = try perform(request) }
    /// 在服务调用队列中锁定容器，再清除应用私有数据。
    func clearLocalData() throws {
        try execute(NativeRequest(operation: "lock"))
        let files = FileManager.default
        let support = files.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        let caches = files.urls(for: .cachesDirectory, in: .userDomainMask)[0]
        try clearVeilLocalData(support: support, caches: caches, temporary: files.temporaryDirectory)
    }
    func preview(_ name: String) throws -> Data {
        let (kind, data) = try perform(NativeRequest(operation: "preview_file", name: name))
        guard kind == 2 else { throw ClientError.message("服务响应无效") }
        return data
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
