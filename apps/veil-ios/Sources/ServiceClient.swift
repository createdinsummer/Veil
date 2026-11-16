/// ============================================================================
/// Veil iOS 客户端 · Rust 桥接服务客户端（ServiceClient）
///
/// 文件作用：封装 iOS 客户端对 Rust 桥接库（veil-macos-bridge 的 C ABI，即
/// 跨语言函数调用接口）的全部调用。Swift 只负责把业务请求编码成 JSON、
/// 调用 C 函数、取回缓冲区并解码，具体加密与容器逻辑都在 Rust 的 veil-service 里。
/// 所属模块：iOS 客户端服务层，位于 Store（状态中枢）与 Rust 原生库之间。
/// 文件内核心类型清单：
///   - NativeRequest           发给 Rust 服务的统一请求体（按操作名区分用途）
///   - ImportConflict          一条导入 / 导出重名冲突
///   - ImportInspection        一次冲突裁决批次
///   - ImportAction/Resolution 用户对冲突的处理方式与裁决结果
///   - ClientError             客户端自定义错误
///   - ImportTransferProgress  服务端发布的传输进度快照
///   - RustHandle              Rust 服务实例指针的 Swift 封装
///   - ServiceClient           服务调用门面（actor隔离状态，异步等待期间可重入）
///   - clearVeilLocalData      清理应用私有数据的工具函数
/// 依赖能力：Foundation（Data / URL 等）、veil_bridge.h 里声明的 C ABI 函数。
/// 整体业务目标：为上层提供“发 JSON 请求 → 收 JSON / 文件字节”的稳定通道，
///   正确归还Rust缓冲并尽量缩短编码副本寿命；不保证所有Swift字符串/Data副本均零化。
/// ============================================================================
import Foundation

/// 发给 Rust 服务的统一请求体：所有操作共用一个结构，按需填字段。
/// 编码后的 JSON 与 bridge 里 Request 枚举一一对应（operation 决定具体操作）。
struct NativeRequest: Encodable, Sendable {
    /// 操作名（snake_case 风格）：list_spaces、open_container、stream_import_files 等。
    var operation: String
    /// 通用实体 id：容器 / 空间 / 冲突批次等，按具体操作取用。
    var id: String? = nil
    /// 冲突裁决结果列表：仅 resolve_import_conflicts / resolve_export_conflicts 使用。
    var resolutions: [ImportResolution]? = nil
    /// 空间 id：创建容器、导入加密备份等操作的作用域。
    var space: String? = nil
    /// 名称字段：创建空间 / 容器时的显示名，或容器内文件路径。
    var name: String? = nil
    /// 创建/打开/备份密码；会话可能持续持有副本，日志按规则脱敏，不能承诺调用后全部清零。
    var password: String? = nil
    /// 路径字段：解锁容器、导入备份等的本地文件位置。
    var path: String? = nil
    /// 容器内文件路径列表：流式导出时使用。
    var names: [String]? = nil
    /// 源文件路径列表：导入操作使用。
    var sources: [String]? = nil
    /// 目标目录：导入、新建文件夹时使用（"" 表示容器根目录）。
    var directory: String? = nil
    /// 导出目标位置：单文件导出 / 整容器打包时使用。
    var destination: String? = nil
}

/// 一条导入 / 导出冲突：目标位置已存在同名条目，需要用户决定覆盖还是跳过。
/// 来自poll_*_conflicts；相关任务等待选择时，其他无冲突文件仍可能继续处理。
struct ImportConflict: Decodable, Identifiable, Sendable {
    /// 冲突目标路径：导入为容器内部路径，导出为外部位置，同时作本批列表标识。
    let target: String
    /// 本次服务比较是否认为内容相同；瞬时提示，不保证确认后目标不变或覆盖绝无损失。
    let sameContent: Bool
    /// 冲突的另一方是否为文件夹（文件与文件夹重名属于类型冲突）。
    let isDirectory: Bool
    /// 服务端无法判定冲突详情时的说明文字。
    let error: String?
    /// 是否允许“覆盖”：文件夹类型冲突或有判定错误时只能跳过。
    var canOverwrite: Bool { !isDirectory && error == nil }
    /// 列表唯一标识，直接用目标路径。
    var id: String { target }
    /// 展示给用户的冲突说明文字。
    var detail: String {
        if let error { return "无法确定：\(error)" }
        return isDirectory ? "文件与文件夹类型冲突，只能跳过" : (sameContent ? "内容相同" : "内容不同")
    }
}

/// 一次冲突批次：需要客户端裁决的目标集合；不代表整条传输流水线暂停。
/// 客户端展示裁决页，回传每条的覆盖 / 跳过决定后传输继续。
struct ImportInspection: Decodable, Identifiable, Sendable {
    /// 批次 id，回传裁决结果时用于定位本次传输。
    let id: String
    /// 整个传输当前发现的文件总数，扫描中可增长；不等于本批冲突条数。
    let totalFiles: Int
    /// 本批次需要用户裁决的冲突列表。
    let conflicts: [ImportConflict]
}

/// 用户对单个冲突的处理方式：覆盖目标或跳过该文件。
enum ImportAction: String, Encodable, Sendable { case overwrite, skip }
/// 单个冲突的裁决结果：目标路径 + 处理方式。
struct ImportResolution: Encodable, Sendable {
    /// poll返回的冲突目标原文，导入内部路径/导出外部路径，回传时须精确匹配。
    let target: String
    /// 用户选择：覆盖或跳过。
    let action: ImportAction
}

/// 客户端自定义错误：本地环境错误与服务端业务错误统一转成可展示文案。
enum ClientError: LocalizedError {
    /// 本地校验 / 环境错误（直接展示文案）。
    case message(String)
    /// 服务端返回的业务错误（kind=1 缓冲区的 UTF-8 错误正文）。
    case service(String)
    /// 文件类型不支持（例如无法解码的文本预览）。
    case unsupportedContent(String)
    /// 服务返回了无法解析的响应。
    case invalidResponse
    /// 把错误转成用户可见的本地化文案。
    var errorDescription: String? {
        switch self {
        case .message(let text), .service(let text), .unsupportedContent(let text): return text
        case .invalidResponse: return "服务响应无效"
        }
    }
}

/// 传输进度快照：服务端周期性发布的导入 / 导出进度，界面用它渲染进度条与文案。
struct ImportTransferProgress: Decodable {
    /// 当前传输方向："import"（导入）或 "export"（导出）。
    let operation: String
    /// 传输阶段："cancelled"（已取消）等；用于区分用户取消与真实失败。
    let phase: String
    /// 已处理完成数，包含实际提交/发布成功和跳过。
    let completed_files: UInt64
    /// 已跳过文件数（重名冲突按用户选择跳过）。
    let skipped_files: UInt64
    /// 文件总数（扫描未完成时还会持续增长）。
    let total_files: UInt64
    /// 文件遍历是否已经完成。
    let scan_complete: Bool
    /// 正在等待用户裁决的文件数。
    let waiting_files: UInt64
    /// 已经过核对阶段的文件数，与完成/跳过计数不是同一含义。
    let checked_files: UInt64
}

/// 共享Rust服务指针的所有权封装，最后一个Swift引用释放时归还。
/// @unchecked Sendable依赖Rust内部同步和每次调用的强引用，业务请求在await期间可并发。
private final class RustHandle: @unchecked Sendable {
    /// Rust 端 VeilService 实例指针；配置数据目录失败时为 nil。
    let pointer: OpaquePointer?
    /// 配置iOS沙盒数据根后创建Rust句柄。
    /// 入参：无，从应用Application Support取得目录并附Veil。
    /// 返回值：RustHandle，配置失败时pointer=nil。
    /// 异常：无throws；Application Support数组为空会下标失败，Rust初始化异常无法通用恢复。
    /// 注意事项：configure_app_data_dir是进程全局约定，须在应用库使用前完成；不创建已打开会话。
    init() {
        // iOS 使用应用私有目录：Application Support/Veil 作为 Veil 的数据根目录。
        let directory = FileManager.default.urls(
            for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent(
                "Veil", isDirectory: true)
        // 先把私有目录注入 core（否则 Rust 侧不知道数据放哪），成功后才创建服务实例。
        pointer =
            directory.path.withCString({ veil_configure_app_data_dir($0) }) ? veil_client_new() : nil
    }
    // 销毁时把指针还给 Rust，由其释放服务实例与内存中的密码、密钥。
    /// 释放本封装持有的Rust服务资源。
    /// 入参：无显式参数，使用可选pointer。
    /// 返回值：无，nil不调用free。
    /// 异常：无Swift错误返回；Rust析构可能等待后台消费者。
    /// 注意事项：必须在所有调用结束后才最后释放；任务捕获RustHandle保持生命周期。
    deinit { if let pointer { veil_client_free(pointer) } }
}

/// 服务入口actor：Swift访问受隔离，但await detached期间可重入，Rust调用并非全程串行。
/// 业务方只构造 NativeRequest，返回值是 JSON 解码后的 Swift 类型或原始字节。
actor ServiceClient {
    /// 此ServiceClient持有的Rust句柄，不是全进程单例；nonisolated允许并发进度/取消调用。
    nonisolated private let rust = RustHandle()
    /// 向Rust共享进度槽请求协作取消。
    /// 入参：无；使用本客户端持有的句柄。
    /// 返回值：Bool，首次受理true；无可取消任务、已请求取消或桥接失败false。
    /// 异常：不抛出Swift错误。
    /// 注意事项：nonisolated可与长请求并发；不等待任务停止，不撤销已提交文件。iOS句柄未初始化也返回false。
    nonisolated func cancelTransfer() -> Bool {
        guard let pointer = rust.pointer else { return false }
        return veil_client_cancel_transfer(pointer)
    }
    /// 读取共享传输状态用于界面轮询，允许与业务请求并发。
    /// 入参：无，使用持有的Rust句柄。
    /// 返回值：可解码时返回进度结构；无效句柄/类型/指针、兼容null或解码失败为nil。空闲也可返回含idle等阶段的快照。
    /// 异常：不抛错，JSON解码失败交AppLog记录后nil。
    /// 注意事项：nonisolated不排队等待actor，但Rust内部锁仍可等待；复制Data后defer归还原缓冲，只代表共享槽而非全部请求。
    nonisolated func transferProgress() -> ImportTransferProgress? {
        guard let pointer = rust.pointer else { return nil }
        // 调用后必须归还缓冲区，避免 Rust 侧内存泄漏。
        let response = veil_client_progress(pointer)
        defer { veil_buffer_free(response) }
        guard response.kind == 0, let data = response.data else { return nil }
        let bytes = Data(bytes: data, count: response.length)
        // 兼容null响应；当前Rust实际返回Snapshot，空闲由其operation/phase表达。
        if bytes == Data("null".utf8) { return nil }
        do { return try JSONDecoder().decode(ImportTransferProgress.self, from: bytes) } catch {
            AppLog.capture(error)
            return nil
        }
    }
    /// 在独立任务编码JSON、调用Rust并复制响应，再归还桥接内存。
    /// 入参：request（NativeRequest）为按操作名携带字段的值，请求String包括密码可有独立副本。
    /// 返回值：(UInt32, Data)：kind及已复制载荷；kind1直接抛服务错误。
    /// 异常：编码、无效缓冲、服务错误抛出并经AppLog.recorded；句柄配置失败抛message。
    /// 注意事项：Task.detached捕获句柄保证调用期间不释放；await让出actor，允许多个请求同时执行。取消外层Swift Task不会自动取消Rust调用。仅当前编码Data resetBytes，不保证清除String/写时复制/编码器全部副本。
    private func perform(_ request: NativeRequest) async throws -> (UInt32, Data) {
        let rust = self.rust
        do {
            // 调用放到 detached 后台任务，避免编码与 C 调用阻塞 actor / 主线程。
            return try await Task.detached {
                guard let handle = rust.pointer else { throw ClientError.message("无法初始化服务") }
                // 请求编码为 JSON；defer 清零当前 Data 缓冲区，减少该副本驻留时间。
                // request 内的字符串及编码器可能还有各自副本，此处不能保证清零全部密码内存。
                var input = try JSONEncoder().encode(request)
                defer { input.resetBytes(in: 0..<input.count) }
                let result = input.withUnsafeBytes {
                    veil_client_call(handle, $0.bindMemory(to: UInt8.self).baseAddress, input.count)
                }
                // 无论成功失败，缓冲区都必须归还 Rust 释放。
                defer { veil_buffer_free(result) }
                guard let bytes = result.data else { throw ClientError.invalidResponse }
                let data = Data(bytes: bytes, count: result.length)
                // kind=1 表示服务端业务错误，正文是 UTF-8 错误文案。
                if result.kind == 1 { throw ClientError.service(String(decoding: data, as: UTF8.self)) }
                return (result.kind, data)
            }.value
        } catch {
            throw AppLog.recorded(error)
        }
    }
    /// 执行需要JSON响应的请求，并解码为界面业务模型。
    /// 入参：request为NativeRequest；as（T.Type）为目标Decodable类型。
    /// 返回值：T为JSONDecoder解码的对象，模型字段由对应服务响应定义。
    /// 异常：编码/服务错误、非kind0或JSON解码失败抛出，交AppLog.recorded。
    /// 注意事项：await期间可重入；不约束请求必须只读，成功解码前业务可能已经产生变更。
    func json<T: Decodable>(_ request: NativeRequest, as: T.Type = T.self) async throws -> T {
        do {
            let (kind, data) = try await perform(request)
            guard kind == 0 else { throw ClientError.invalidResponse }
            return try JSONDecoder().decode(T.self, from: data)

        } catch {
            throw AppLog.recorded(error)
        }
    }
    /// 执行仅需确认成功的请求并丢弃响应正文。
    /// 入参：request（NativeRequest）为操作描述。
    /// 返回值：Void，允许的kind验证通过即返回，不要求JSON正文为null。
    /// 异常：编码、服务或非法kind抛错并记录。
    /// 注意事项：不是所有无返回业务都可调用：接受kind0，以及export_file操作的kind4，后者稳定性提示被丢弃。业务已完成后发现协议不符也可能抛错。
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
    /// 先请求清主会话，再删除调用方应用私有根及缓存内容。
    /// 入参：无，读取FileManager应用Support/Caches/临时目录。
    /// 返回值：Void，全部删除完成后返回。
    /// 异常：lock服务失败或任一目录枚举/删除错误抛出并记录；之前已删部分不恢复。
    /// 注意事项：await锁定期间actor可重入，不能证明其他请求已结束或所有密码副本销毁；调用方需保证清理期间无并发业务。仅用于清理本机数据入口。
    func clearLocalData() async throws {
        do {
            try await execute(NativeRequest(operation: "lock"))
            // 依次清理 Application Support、Caches 与临时目录里的 Veil 数据。
            let files = FileManager.default
            let support = files.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            let caches = files.urls(for: .cachesDirectory, in: .userDomainMask)[0]
            try clearVeilLocalData(support: support, caches: caches, temporary: files.temporaryDirectory)

        } catch {
            throw AppLog.recorded(error)
        }
    }
    /// 读取完整的预览明文字节，忽略不稳定模式提示。
    /// 入参：name（String）为容器内部文件路径。
    /// 返回值：Data为解密字节，受服务支持类型与16MiB上限约束。
    /// 异常：服务拒绝/认证/编码等错误或响应类型不符时抛错。
    /// 注意事项：不截断大文件；kind3仅表示不持稳定读租约，不证明实际发生变化。Data副本不会随Rust缓冲释放自动零化。
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

/// 删除指定Support下Veil根，并清空给定Caches/临时目录的所有直接子项。
/// 入参：support/caches/temporary（URL）须由调用方保证是本应用私有目录。
/// 返回值：Void，保留缓存/临时目录本身，递归移除其每条子项。
/// 异常：任何枚举/删除失败抛Foundation错误，先前删除不回滚。
/// 注意事项：不校验沙盒边界、不只筛Veil命名缓存；传错URL会删除相应目录内容。不是安全擦除，不触及列表外的导出文件。
func clearVeilLocalData(support: URL, caches: URL, temporary: URL) throws {
    let files = FileManager.default
    // Veil 数据根目录整体删除：其中包含 app.db 与 spaces/ 下的所有空间与容器。
    let vaultData = support.appendingPathComponent("Veil", isDirectory: true)
    if files.fileExists(atPath: vaultData.path) { try files.removeItem(at: vaultData) }
    // Caches 与临时目录按条目逐个清空（保留目录本身）。
    for directory in [caches, temporary] where files.fileExists(atPath: directory.path) {
        for item in try files.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil) {
            try files.removeItem(at: item)
        }
    }
}
