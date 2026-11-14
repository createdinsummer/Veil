import CVeilBridge
import Foundation
import ImageIO
import UniformTypeIdentifiers

/// 仅描述 macOS 客户端的服务调用，不承载业务规则。
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
    var key: String? = nil
    var value: PreferenceValue? = nil
    var seconds: Int? = nil
    var count: Int? = nil
}

/// 用户设置值：字符串或对象；编码后直接写入 app.db 的 `value_json`。
enum PreferenceValue: Encodable, Sendable, Equatable {
    case string(String)
    case map([String: String])

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .string(let value): try container.encode(value)
        case .map(let value): try container.encode(value)
        }
    }
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

extension ImportInspection {
    /// 把一批冲突按统一动作展开；覆盖不了的目标（目录、校验失败等）按跳过处理。
    func resolutions(for action: ImportAction) -> [ImportResolution] {
        conflicts.map { conflict in
            ImportResolution(
                target: conflict.target,
                action: action == .overwrite && conflict.canOverwrite ? .overwrite : .skip)
        }
    }
}

enum ImportAction: String, Encodable, Sendable { case overwrite, skip }
struct ImportResolution: Encodable, Sendable {
    let target: String
    let action: ImportAction
}

/// 一次同名文件复核的结果；`stickyAction` 非空表示后续批次不再询问，直接沿用该动作。
struct ImportReviewOutcome: Sendable {
    var resolutions: [ImportResolution]?
    var stickyAction: ImportAction?
}

enum ClientError: LocalizedError {
    case service(String)
    case local(String)
    case unsupportedContent(String)
    case invalidResponse

    var errorDescription: String? {
        switch self {
        case .service(let message), .local(let message), .unsupportedContent(let message): message
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
    let scanComplete: Bool
    let waitingFiles: UInt64
    let checkedFiles: UInt64
    let currentFile: String?
    let cancelRequested: Bool
    let parallelWorkers: UInt64

    enum CodingKeys: String, CodingKey {
        case operation, phase
        case scanComplete = "scan_complete"
        case waitingFiles = "waiting_files"
        case checkedFiles = "checked_files"
        case processedBytes = "processed_bytes"
        case totalBytes = "total_bytes"
        case completedFiles = "completed_files"
        case skippedFiles = "skipped_files"
        case pendingFiles = "pending_files"
        case totalFiles = "total_files"
        case currentFile = "current_file"
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
        let data = Data(bytes: pointer, count: response.length)
        if data == Data("null".utf8) { return nil }
        do { return try JSONDecoder().decode(TransferProgress.self, from: data) } catch {
            AppLog.capture(error)
            return nil
        }
    }

    nonisolated func cancelTransfer() -> Bool { veil_client_cancel_transfer(handle.pointer) }

    private func perform(_ request: NativeRequest) async throws -> (UInt32, Data) {
        let handle = self.handle
        do {
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
        } catch {
            throw AppLog.recorded(error)
        }
    }

    func json<T: Decodable & Sendable>(_ request: NativeRequest, as type: T.Type = T.self)
        async throws -> T
    {
        do {
            let (kind, data) = try await perform(request)
            guard kind == 0 else { throw ClientError.invalidResponse }
            return try JSONDecoder().decode(type, from: data)

        } catch {
            throw AppLog.recorded(error)
        }
    }

    func execute(_ request: NativeRequest) async throws {
        do {
            let (kind, _) = try await perform(request)
            guard kind == 0 else { throw ClientError.invalidResponse }

        } catch {
            throw AppLog.recorded(error)
        }
    }

    func preview(name: String) async throws -> Data {
        try await previewWithStatus(name: name).data
    }

    func previewWithStatus(name: String) async throws -> (data: Data, unstable: Bool) {
        do {
            let (kind, data) = try await perform(NativeRequest(operation: "preview_file", name: name))
            guard kind == 2 || kind == 3 else { throw ClientError.invalidResponse }
            return (data, kind == 3)

        } catch {
            throw AppLog.recorded(error)
        }
    }

    func exportWithStatus(name: String, destination: String) async throws -> Bool {
        do {
            let (kind, _) = try await perform(
                NativeRequest(operation: "export_file", name: name, destination: destination))
            guard kind == 0 || kind == 4 else { throw ClientError.invalidResponse }
            return kind == 4

        } catch {
            throw AppLog.recorded(error)
        }
    }

    /// 在后台 actor 中按实际显示尺寸缩小，原图与缩略图均不写入磁盘。
    func thumbnail(name: String) async throws -> Data {
        do {
            let bytes = try await preview(name: name)
            guard
                let source = CGImageSourceCreateWithData(
                    bytes as CFData, [kCGImageSourceShouldCache: false] as CFDictionary),
                let image = CGImageSourceCreateThumbnailAtIndex(
                    source, 0,
                    [
                        kCGImageSourceCreateThumbnailFromImageAlways: true,
                        kCGImageSourceCreateThumbnailWithTransform: true,
                        kCGImageSourceThumbnailMaxPixelSize: 320,
                        kCGImageSourceShouldCacheImmediately: true,
                    ] as CFDictionary)
            else { throw ClientError.unsupportedContent("无法生成图片缩略图") }
            let output = NSMutableData()
            guard
                let destination = CGImageDestinationCreateWithData(
                    output, UTType.png.identifier as CFString, 1, nil)
            else {
                throw ClientError.invalidResponse
            }
            CGImageDestinationAddImage(destination, image, nil)
            guard CGImageDestinationFinalize(destination) else { throw ClientError.invalidResponse }
            return output as Data

        } catch {
            throw AppLog.recorded(error)
        }
    }
}
