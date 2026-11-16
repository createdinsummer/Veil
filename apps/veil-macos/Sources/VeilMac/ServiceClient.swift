/// ============================================================================
/// Veil macOS 客户端 · Rust 桥接服务客户端（ServiceClient）
///
/// 文件作用：封装 macOS 客户端对 Rust 桥接库（veil-macos-bridge 的 C ABI，即
///   跨语言函数调用接口）的全部调用。Swift 只把业务请求编码成 JSON、调用 C 函数、
///   取回缓冲区并解码，具体加密与容器逻辑都在 Rust 的 veil-service 里。
/// 所属模块：客户端服务层，位于 AppStore（状态中枢）与 Rust 原生库之间。
/// 文件内核心类型清单：
///   - NativeRequest       发给 Rust 服务的统一请求体（按操作名区分用途）
///   - PreferenceValue     用户设置值（字符串或映射）
///   - ImportConflict      一条导入/导出重名冲突
///   - ImportInspection    一次冲突裁决批次
///   - ImportAction/Resolution/ReviewOutcome 冲突处理动作与裁决结果
///   - ClientError         客户端自定义错误
///   - TransferProgress    服务端发布的传输进度快照
///   - RustHandle          Rust 服务实例指针的 Swift 封装
///   - ServiceClient       服务调用门面（actor隔离状态，异步等待期间可重入）
/// 依赖能力：Foundation、ImageIO（缩略图缩放）、UniformTypeIdentifiers、
///   CVeilBridge 模块里声明的 C ABI 函数。
/// 整体业务目标：为上层提供「发 JSON 请求 → 收 JSON/文件字节」的稳定通道，
///   正确归还Rust缓冲并尽量缩短编码副本寿命；字符串、图像和Data的所有副本不能据此保证清零。
/// ============================================================================
import CVeilBridge
import Foundation
import ImageIO
import UniformTypeIdentifiers

/// 仅描述 macOS 客户端的服务调用，不承载业务规则。
/// 编码后的 JSON 与 bridge 里 Request 枚举一一对应（operation 决定具体操作）。
struct NativeRequest: Encodable, Sendable {
    /// 操作名（snake_case）：list_spaces、open_container、stream_import_files 等。
    var operation: String
    /// 通用实体 id：容器/空间/冲突批次等，按具体操作取用。
    var id: String? = nil
    /// 冲突裁决结果列表：仅 resolve_import_conflicts / resolve_export_conflicts 使用。
    var resolutions: [ImportResolution]? = nil
    /// 空间 id：创建容器、导入加密备份等操作的作用域。
    var space: String? = nil
    /// 名称字段：创建空间/容器时的显示名，或容器内文件路径。
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
    /// 导出目标位置：单文件导出/整容器打包时使用。
    var destination: String? = nil
    /// 设置项键名：get/set_preference、get_system_setting 使用。
    var key: String? = nil
    /// 设置项值：仅 set_preference 使用。
    var value: PreferenceValue? = nil
    /// 秒数：set_unlock_cache、set_import_commit_interval 使用。
    var seconds: Int? = nil
    /// 条数：set_import_commit_count 使用。
    var count: Int? = nil
}

/// 用户偏好请求值：字符串或对象；经JSON编码和服务规则校验后才可写应用库。
enum PreferenceValue: Encodable, Sendable, Equatable {
    case string(String)
    case map([String: String])

    /// 将用户偏好编码为桥接支持的JSON原始值。
    /// 入参：encoder（Encoder）为调用方编码器；self为string或map变体。
    /// 返回值：Void，string编码字符串，map编码字符串键/值对象。
    /// 异常：底层单值容器编码失败向外抛出。
    /// 注意事项：只编码，不直接保存；服务按偏好键再校验类型/范围。map每项为一个偏好子键及文本值。
    func encode(to encoder: Encoder) throws {
        // 用单值容器把两种情况都编码成 JSON 原始值（字符串或对象）。
        var container = encoder.singleValueContainer()
        switch self {
        case .string(let value): try container.encode(value)
        case .map(let value): try container.encode(value)
        }
    }
}

/// 一条导入/导出冲突：目标位置已存在同名条目，需要用户决定覆盖还是跳过。
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
    /// 是否允许「覆盖」：文件夹类型冲突或有判定错误时只能跳过。
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
/// 客户端展示裁决页，回传每条的覆盖/跳过决定后传输继续。
struct ImportInspection: Decodable, Identifiable, Sendable {
    /// 批次 id，回传裁决结果时用于定位本次传输。
    let id: String
    /// 整个传输当前发现的文件总数，扫描中可增长；不等于本批冲突条数。
    let totalFiles: Int
    /// 本批次需要用户裁决的冲突列表。
    let conflicts: [ImportConflict]
}

extension ImportInspection {
    /// 把整批冲突按统一用户动作转换为逐项目决议。
    /// 入参：action（ImportAction）为overwrite或skip。
    /// 返回值：[ImportResolution]，与conflicts输入顺序一一对应；不可覆盖项强制skip。
    /// 异常：不抛错。
    /// 注意事项：每轮一项冲突，只根据本次canOverwrite提示决策，服务提交仍需检查batch token/目标状态；不改变原冲突列表。
    func resolutions(for action: ImportAction) -> [ImportResolution] {
        conflicts.map { conflict in
            ImportResolution(
                target: conflict.target,
                action: action == .overwrite && conflict.canOverwrite ? .overwrite : .skip)
        }
    }
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

/// 一次同名文件复核的结果；`stickyAction` 非空表示后续批次不再询问，直接沿用该动作。
/// 入参含义：resolutions 是本批每条冲突的裁决；stickyAction 是「全部跳过/覆盖，不再询问」。
/// 补充：resolutions 为 nil 表示用户中止了本次传输。
struct ImportReviewOutcome: Sendable {
    /// 本批每条目标的决定；nil为用户中止，空数组仅表示没有项目而非中止。
    var resolutions: [ImportResolution]?
    /// 后续批次统一动作；nil继续逐批询问，不可覆盖项仍由resolutions转换成skip。
    var stickyAction: ImportAction?
}

/// 客户端自定义错误：本地环境错误与服务端业务错误统一转成可展示文案。
enum ClientError: LocalizedError {
    /// 服务端返回的业务错误（kind=1 缓冲区的 UTF-8 错误正文）。
    case service(String)
    /// 本地环境/校验错误（直接展示文案）。
    case local(String)
    /// 内容类型不支持（例如无法生成缩略图）。
    case unsupportedContent(String)
    /// 服务返回了无法解析的响应。
    case invalidResponse

    /// 把错误转成用户可见的本地化文案。
    var errorDescription: String? {
        switch self {
        case .service(let message), .local(let message), .unsupportedContent(let message): message
        case .invalidResponse: "服务响应无效，请重试。"
        }
    }
}

/// 传输进度快照：服务端周期性发布的导入/导出/打包进度，界面用它渲染进度条与文案。
struct TransferProgress: Decodable, Sendable {
    /// 当前传输方向："import"（导入）、"export"（导出）或打包/导入容器等。
    let operation: String
    /// 传输阶段："preparing/processing/verifying/snapshot/packing/unpacking/finalizing/cancelled" 等。
    let phase: String
    /// 已处理的字节数（打包类任务按字节计进度）。
    let processedBytes: UInt64
    /// 总字节数；扫描未完成或未知时为 nil。
    let totalBytes: UInt64?
    /// 已处理完成数，包含实际提交/发布成功和跳过。
    let completedFiles: UInt64
    /// 已加密、等待提交入库的文件数（仅导入）。
    let pendingFiles: UInt64
    /// 已跳过文件数（重名冲突按用户选择跳过）。
    let skippedFiles: UInt64
    /// 文件总数（扫描未完成时还会持续增长）。
    let totalFiles: UInt64
    /// 文件遍历是否已经完成。
    let scanComplete: Bool
    /// 正在等待用户裁决的文件数。
    let waitingFiles: UInt64
    /// 已经过核对阶段的文件数，与完成/跳过计数不是同一含义。
    let checkedFiles: UInt64
    /// 最近发布到共享进度的当前文件路径；并行时不代表所有工作线程各自的文件。
    let currentFile: String?
    /// 用户是否已请求取消。
    let cancelRequested: Bool
    /// 并行加密/解密的工作线程数。
    let parallelWorkers: UInt64

    /// JSON 字段名映射：服务端用 snake_case，Swift 属性用驼峰。
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

/// Rust服务指针的所有权容器：共享调用只借用pointer，最后一个Swift引用释放时free。
/// @unchecked Sendable依赖Rust服务内部同步及调用期间的强引用，而不是actor保证请求绝不并发。
private final class RustHandle: @unchecked Sendable {
    /// Rust 端 VeilService 实例指针。
    let pointer: OpaquePointer
    /// 创建本ServiceClient所用的Rust服务实例。
    /// 入参：无。
    /// 返回值：RustHandle持有非可选OpaquePointer。
    /// 异常：veil_client_new为nil会强制解包失败；Rust进程中止不能转Swift错误。
    /// 注意事项：创建初始化日志及后台队列，不解锁容器；由deinit唯一归还句柄。
    init() { pointer = veil_client_new()! }
    // 销毁时把指针还给 Rust，由其释放服务实例与内存中的密码、密钥。
    /// 在最后强引用结束时归还Rust服务句柄。
    /// 入参：无显式参数，使用pointer。
    /// 返回值：无。
    /// 异常：不以Swift Error返回，Rust析构可同步等待后台线程。
    /// 注意事项：调用期间须由detached捕获或当前方法保留强引用；不等于销毁所有其他服务/连接池密码副本。
    deinit { veil_client_free(pointer) }
}

/// 耗时调用在后台执行，允许其他请求在导入期间进入服务。
/// actor隔离Swift入口状态；perform的detached工作可并行，进度/取消以nonisolated直接调用Rust。
actor ServiceClient {
    /// 此ServiceClient持有的Rust句柄，不是全进程单例；nonisolated允许并发进度/取消调用。
    nonisolated private let handle = RustHandle()

    /// 读取共享传输状态用于界面轮询，允许与业务请求并发。
    /// 入参：无，使用持有的Rust句柄。
    /// 返回值：可解码时返回进度结构；无效句柄/类型/指针、兼容null或解码失败为nil。空闲也可返回含idle等阶段的快照。
    /// 异常：不抛错，JSON解码失败交AppLog记录后nil。
    /// 注意事项：nonisolated不排队等待actor，但Rust内部锁仍可等待；复制Data后defer归还原缓冲，只代表共享槽而非全部请求。
    nonisolated func transferProgress() -> TransferProgress? {
        let response = veil_client_progress(handle.pointer)
        // 调用后必须归还缓冲区，避免 Rust 侧内存泄漏。
        defer { veil_buffer_free(response) }
        guard response.kind == 0, let pointer = response.data else { return nil }
        let data = Data(bytes: pointer, count: response.length)
        // 兼容null响应；当前Rust实际返回Snapshot，空闲由其operation/phase表达。
        if data == Data("null".utf8) { return nil }
        do { return try JSONDecoder().decode(TransferProgress.self, from: data) } catch {
            AppLog.capture(error)
            return nil
        }
    }

    /// 向Rust共享进度槽请求协作取消。
    /// 入参：无；使用本客户端持有的句柄。
    /// 返回值：Bool，首次受理true；无可取消任务、已请求取消或桥接失败false。
    /// 异常：不抛出Swift错误。
    /// 注意事项：nonisolated可与长请求并发；不等待任务停止，不撤销已提交文件。调用期间RustHandle必须存活。
    nonisolated func cancelTransfer() -> Bool { veil_client_cancel_transfer(handle.pointer) }

    /// 在独立任务编码JSON、调用Rust并复制响应，再归还桥接内存。
    /// 入参：request（NativeRequest）为按操作名携带字段的值，请求String包括密码可有独立副本。
    /// 返回值：(UInt32, Data)：kind及已复制载荷；kind1直接抛服务错误。
    /// 异常：编码、无效缓冲、服务错误抛出并经AppLog.recorded；有效句柄由RustHandle持有。
    /// 注意事项：Task.detached捕获句柄保证调用期间不释放；await让出actor，允许多个请求同时执行。取消外层Swift Task不会自动取消Rust调用。仅当前编码Data resetBytes，不保证清除String/写时复制/编码器全部副本。
    private func perform(_ request: NativeRequest) async throws -> (UInt32, Data) {
        let handle = self.handle
        do {
            // 调用放到 detached 后台任务，避免编码与 C 调用阻塞 actor/主线程。
            return try await Task.detached {
                // 请求编码为 JSON；defer 清零当前 Data 缓冲区，减少该副本驻留时间。
                // request 内的字符串及编码器可能还有各自副本，此处不能保证清零全部密码内存。
                var input = try JSONEncoder().encode(request)
                defer { input.resetBytes(in: 0..<input.count) }
                let response = input.withUnsafeBytes { raw in
                    veil_client_call(handle.pointer, raw.bindMemory(to: UInt8.self).baseAddress, input.count)
                }
                // 无论成功失败，缓冲区都必须归还 Rust 释放。
                defer { veil_buffer_free(response) }
                guard let pointer = response.data else { throw ClientError.invalidResponse }
                let data = Data(bytes: pointer, count: response.length)
                // kind=1 表示服务端业务错误，正文是 UTF-8 错误文案。
                if response.kind == 1 {
                    throw ClientError.service(String(decoding: data, as: UTF8.self))
                }
                return (response.kind, data)
            }.value
        } catch {
            throw AppLog.recorded(error)
        }
    }

    /// 执行需要JSON响应的请求，并解码为界面业务模型。
    /// 入参：request为NativeRequest；as（T.Type）为目标Decodable类型，T还须Sendable。
    /// 返回值：T为JSONDecoder解码的对象，模型字段由对应服务响应定义。
    /// 异常：编码/服务错误、非kind0或JSON解码失败抛出，交AppLog.recorded。
    /// 注意事项：await期间可重入；不约束请求必须只读，成功解码前业务可能已经产生变更。
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

    /// 执行仅需确认成功的请求并丢弃响应正文。
    /// 入参：request（NativeRequest）为操作描述。
    /// 返回值：Void，允许的kind验证通过即返回，不要求JSON正文为null。
    /// 异常：编码、服务或非法kind抛错并记录。
    /// 注意事项：不是所有无返回业务都可调用：仅接受kind0，kind4也拒绝，应使用exportWithStatus处理不稳定导出。业务已完成后发现协议不符也可能抛错。
    func execute(_ request: NativeRequest) async throws {
        do {
            let (kind, _) = try await perform(request)
            guard kind == 0 else { throw ClientError.invalidResponse }

        } catch {
            throw AppLog.recorded(error)
        }
    }

    /// 读取完整的预览明文字节，忽略不稳定模式提示。
    /// 入参：name（String）为容器内部文件路径。
    /// 返回值：Data为解密字节，受服务支持类型与16MiB上限约束。
    /// 异常：服务拒绝/认证/编码等错误或响应类型不符时抛错。
    /// 注意事项：不截断大文件；kind3仅表示不持稳定读租约，不证明实际发生变化。Data副本不会随Rust缓冲释放自动零化。
    func preview(name: String) async throws -> Data {
        try await previewWithStatus(name: name).data
    }

    /// 读取预览字节并保留服务读取模式提示。
    /// 入参：name（String）为容器内部文件路径。
    /// 返回值：(data:Data, unstable:Bool)，kind3为true，kind2为false。
    /// 异常：服务错误或kind不为2/3抛错并记录。
    /// 注意事项：不稳定表示无持久稳定内容租约，不表示截断；当前普通预览通常返回kind3，仍执行密文认证。
    func previewWithStatus(name: String) async throws -> (data: Data, unstable: Bool) {
        do {
            let (kind, data) = try await perform(NativeRequest(operation: "preview_file", name: name))
            guard kind == 2 || kind == 3 else { throw ClientError.invalidResponse }
            return (data, kind == 3)

        } catch {
            throw AppLog.recorded(error)
        }
    }

    /// 不覆盖导出单个会话文件，并读取稳定性标志。
    /// 入参：name为内部文件路径；destination（String）为精确本地输出位置。
    /// 返回值：Bool，kind4为不稳定提示true，kind0为false。
    /// 异常：服务拒绝、文件/认证/租约错误或非法kind抛出并记录。
    /// 注意事项：发布后同步失败也可能抛错；函数不自行清理目标，调用方负责临时文件生命周期。
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

    /// 在内存解密受限大小图片并编码不超过320像素最长边的PNG缩略图。
    /// 入参：name为内部图片文件路径。
    /// 返回值：Data为首帧/首图索引0的PNG字节。
    /// 异常：服务预览错误、ImageIO解码/缩放失败或PNG编码失败抛ClientError并记录。
    /// 注意事项：先读取整份明文，受16MiB限制；ImageIO同步处理在actor执行域，不保证独立后台线程。未写磁盘但Data/图像内存不自动全部零化，预览稳定性被丢弃。
    func thumbnail(name: String) async throws -> Data {
        do {
            // 先取整份解密字节（服务层 16 MiB 限制内），再在内存中生成小尺寸缩略图。
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
            // 把缩小后的图片编码成 PNG 写入内存缓冲区。
            CGImageDestinationAddImage(destination, image, nil)
            guard CGImageDestinationFinalize(destination) else { throw ClientError.invalidResponse }
            return output as Data

        } catch {
            throw AppLog.recorded(error)
        }
    }
}
