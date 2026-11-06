import Foundation
import CVeilBridge
import ImageIO
import UniformTypeIdentifiers

/// 仅描述 macOS 客户端的服务调用，不承载业务规则。
struct NativeRequest: Encodable, Sendable {
    var operation: String
    var space: String? = nil
    var name: String? = nil
    var password: String? = nil
    var path: String? = nil
    var sources: [String]? = nil
    var directory: String? = nil
    var destination: String? = nil
    var key: String? = nil
    var value: String? = nil
    var seconds: Int? = nil
    var count: Int? = nil
}

enum ClientError: LocalizedError {
    case service(String)
    case invalidResponse

    var errorDescription: String? {
        switch self {
        case .service(let message): message
        case .invalidResponse: "服务响应无效，请重试。"
        }
    }
}

struct TransferProgress: Decodable, Sendable {
    let operation: String
    let phase: String
    let processedBytes: UInt64
    let totalBytes: UInt64?
    let completedFiles: UInt64
    let pendingFiles: UInt64
    let skippedFiles: UInt64
    let totalFiles: UInt64
    let currentFile: String?
    let cancelRequested: Bool
    let parallelWorkers: UInt64

    enum CodingKeys: String, CodingKey {
        case operation, phase
        case processedBytes = "processed_bytes", totalBytes = "total_bytes"
        case completedFiles = "completed_files", skippedFiles = "skipped_files"
        case pendingFiles = "pending_files"
        case totalFiles = "total_files", currentFile = "current_file"
        case cancelRequested = "cancel_requested"
        case parallelWorkers = "parallel_workers"
    }
}

/// 句柄由 actor 独占使用；最后一个引用释放时销毁 Rust 会话。
private final class RustHandle: @unchecked Sendable {
    let pointer: OpaquePointer
    init() { pointer = veil_client_new()! }
    deinit { veil_client_free(pointer) }
}

/// 耗时调用在后台执行，允许其他请求在导入期间进入服务。
actor ServiceClient {
    nonisolated private let handle = RustHandle()

    /// 不经过 actor 队列，允许操作执行期间并发读取进度。
    nonisolated func transferProgress() -> TransferProgress? {
        let response = veil_client_progress(handle.pointer)
        defer { veil_buffer_free(response) }
        guard response.kind == 0, let pointer = response.data else { return nil }
        return try? JSONDecoder().decode(TransferProgress.self, from: Data(bytes: pointer, count: response.length))
    }

    nonisolated func cancelTransfer() -> Bool { veil_client_cancel_transfer(handle.pointer) }

    private func perform(_ request: NativeRequest) async throws -> (UInt32, Data) {
        let handle = self.handle
        return try await Task.detached {
        var input = try JSONEncoder().encode(request)
        defer { input.resetBytes(in: 0..<input.count) }
        let response = input.withUnsafeBytes { raw in
            veil_client_call(handle.pointer, raw.bindMemory(to: UInt8.self).baseAddress, input.count)
        }
        defer { veil_buffer_free(response) }
        guard let pointer = response.data else { throw ClientError.invalidResponse }
        let data = Data(bytes: pointer, count: response.length)
        if response.kind == 1 {
            throw ClientError.service(String(decoding: data, as: UTF8.self))
        }
        return (response.kind, data)
        }.value
    }

    func json<T: Decodable & Sendable>(_ request: NativeRequest, as type: T.Type = T.self) async throws -> T {
        let (kind, data) = try await perform(request)
        guard kind == 0 else { throw ClientError.invalidResponse }
        return try JSONDecoder().decode(type, from: data)
    }

    func execute(_ request: NativeRequest) async throws {
        let (kind, _) = try await perform(request)
        guard kind == 0 else { throw ClientError.invalidResponse }
    }

    func preview(name: String) async throws -> Data {
        try await previewWithStatus(name: name).data
    }

    func previewWithStatus(name: String) async throws -> (data: Data, unstable: Bool) {
        let (kind, data) = try await perform(NativeRequest(operation: "preview_file", name: name))
        guard kind == 2 || kind == 3 else { throw ClientError.invalidResponse }
        return (data, kind == 3)
    }

    func exportWithStatus(name: String, destination: String) async throws -> Bool {
        let (kind, _) = try await perform(NativeRequest(operation: "export_file", name: name, destination: destination))
        guard kind == 0 || kind == 4 else { throw ClientError.invalidResponse }
        return kind == 4
    }

    /// 在后台 actor 中按实际显示尺寸缩小，原图与缩略图均不写入磁盘。
    func thumbnail(name: String) async throws -> Data {
        let bytes = try await preview(name: name)
        guard let source = CGImageSourceCreateWithData(bytes as CFData, [kCGImageSourceShouldCache: false] as CFDictionary),
              let image = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                kCGImageSourceCreateThumbnailFromImageAlways: true,
                kCGImageSourceCreateThumbnailWithTransform: true,
                kCGImageSourceThumbnailMaxPixelSize: 320,
                kCGImageSourceShouldCacheImmediately: true
              ] as CFDictionary) else { throw ClientError.service("无法生成图片缩略图") }
        let output = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(output, UTType.png.identifier as CFString, 1, nil) else {
            throw ClientError.invalidResponse
        }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else { throw ClientError.invalidResponse }
        return output as Data
    }
}
