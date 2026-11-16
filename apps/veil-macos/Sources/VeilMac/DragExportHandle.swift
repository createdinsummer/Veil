/// macOS 拖放导出适配层：在接收应用确认目标路径后才请求服务解密文件。
/// 所属模块：文件浏览界面的拖放交互。核心类型为 DragExportHandle（SwiftUI 与 AppKit 桥接）、DragExportView（系统文件承诺回调）和 PromiseCompletion（Sendable 完成回调封装）。
/// 依赖能力：AppKit 拖放、Uniform Type Identifiers、AppStore 导出任务和主线程异步执行。整体目标是避免拖动开始时提前生成明文副本，并在任务过期或文件不可用时通知系统失败。
import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// SwiftUI 对 AppKit 拖动视图的包装；把当前文件和 Store 注入原生视图。
struct DragExportHandle: NSViewRepresentable {
    /// 用户正在拖动的容器文件条目。
    let file: FileEntry
    /// 承担加密读取、解密与目标写入的应用状态对象。
    let store: AppStore

    /// 为文件行创建供 AppKit 接收拖动事件的原生手柄。
    /// - Parameters: context: Context SwiftUI 包装上下文，本实现不使用。
    /// - Returns: DragExportView，新建但尚未绑定文件和 Store 的视图。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 后续 updateNSView 负责注入当前状态；此时不读取或解密文件。
    func makeNSView(context: Context) -> DragExportView { DragExportView() }
    /// 把当前文件行的数据同步给原生拖动手柄。
    /// - Parameters: view: DragExportView 已创建的手柄；context: Context SwiftUI 上下文，本实现不使用。
    /// - Returns: Void；更新文件、Store 弱引用和提示文字。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 更新只影响下一次冻结的拖动数据；已保存的 dragFile 不在此重置。
    func updateNSView(_ view: DragExportView, context: Context) {
        view.file = file
        view.store = store
        view.toolTip = "拖到应用外导出 \(file.leafName)"
    }
    /// 为文件行的拖动入口提供固定布局大小。
    /// - Parameters: proposal: ProposedViewSize 建议尺寸；nsView: DragExportView 当前视图；context: Context 包装上下文，三者均不参与尺寸计算。
    /// - Returns: CGSize?，始终为宽 20、高 28 点。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 只提供布局尺寸，不表示实际导出文件大小。
    func sizeThatFits(_ proposal: ProposedViewSize, nsView: DragExportView, context: Context) -> CGSize? {
        CGSize(width: 20, height: 28)
    }
}

/// 处理鼠标拖动并响应系统文件承诺；系统选定目标后调用委托写入明文。
final class DragExportView: NSView, NSDraggingSource, NSFilePromiseProviderDelegate {
    /// 当前显示位置对应的文件条目，由 SwiftUI 包装层更新。
    var file: FileEntry?
    /// 应用状态弱引用；视图不应延长整个 Store 的生命周期。
    weak var store: AppStore?
    /// 本次拖动开始时冻结的文件条目，避免列表切换后回调错用其他文件。
    private var dragFile: FileEntry?
    /// 拖动开始时的任务代数；用于拒绝已失效容器或过期的拖放请求。
    private var dragGeneration: UInt64 = 0

    /// 绘制提示用户可向应用外拖动导出的手柄图标。
    /// - Parameters: dirtyRect: NSRect 系统请求重绘的区域，本实现绘制固定图标位置。
    /// - Returns: Void；系统符号不存在时不绘制。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 仅绘制界面，不访问加密文件内容。
    override func draw(_ dirtyRect: NSRect) {
        guard let image = NSImage(systemSymbolName: "line.3.horizontal", accessibilityDescription: "拖动导出") else { return }
        image.draw(in: NSRect(x: 4, y: 8, width: 12, height: 12))
    }

    /// 将当前文件登记为系统文件承诺，延迟到接收方给出位置后才导出。
    /// - Parameters: event: NSEvent 触发本次拖动的鼠标事件。
    /// - Returns: Void；文件、Store 缺失或 Store 忙碌时忽略，否则启动拖放会话。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 保存文件和任务代数供后续校验；实际导出前状态仍可能变化，回调交给 Store 检查。
    override func mouseDragged(with event: NSEvent) {
        guard let file, let store, !store.isBusy else { return }
        dragFile = file
        dragGeneration = store.dragExportGeneration
        let type = UTType(filenameExtension: (file.leafName as NSString).pathExtension) ?? .data
        let provider = NSFilePromiseProvider(fileType: type.identifier, delegate: self)
        let item = NSDraggingItem(pasteboardWriter: provider)
        let image = NSWorkspace.shared.icon(for: type)
        item.setDraggingFrame(NSRect(x: 0, y: 0, width: 32, height: 32), contents: image)
        beginDraggingSession(with: [item], event: event, source: self)
    }

    /// 向接收应用声明拖放仅提供导出副本。
    /// - Parameters: session: NSDraggingSession 当前拖放会话；context: NSDraggingContext 拖动场景，两者不改变本实现策略。
    /// - Returns: NSDragOperation，始终为 .copy。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 源容器文件不会因为拖动声明而被移动或删除。
    func draggingSession(_ session: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext) -> NSDragOperation { .copy }
    /// 为接收应用提供本次拖动文件的建议名称。
    /// - Parameters: filePromiseProvider: NSFilePromiseProvider 当前承诺；fileType: String 接收方请求的类型标识，本实现均不参与名称计算。
    /// - Returns: String，冻结文件的 leafName；没有冻结文件时为“文件”。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 仅返回名称，不读取内容，也不保证目标路径没有同名文件。
    func filePromiseProvider(_ filePromiseProvider: NSFilePromiseProvider, fileNameForType fileType: String) -> String {
        dragFile?.leafName ?? "文件"
    }
    /// 指定系统文件承诺回调的调度队列。
    /// - Parameters: filePromiseProvider: NSFilePromiseProvider 当前文件承诺提供者，本实现不读取其内容。
    /// - Returns: OperationQueue.main，主队列。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 后续写入回调创建主 Actor 异步任务；队列选择不代表解密任务同步完成。
    func operationQueue(for filePromiseProvider: NSFilePromiseProvider) -> OperationQueue { .main }
    /// 接收应用确定保存位置后，请求服务导出该文件的明文副本。
    /// - Parameters: filePromiseProvider: NSFilePromiseProvider 当前承诺，本实现不使用；url: URL 接收方指定的输出文件位置；completionHandler: @escaping (Error?) -> Void 系统完成回调。
    /// - Returns: Void；立即创建异步任务，导出结果随后通过回调通知。
    /// - Throws: 不向系统直接抛错；文件或 Store 缺失时通过回调报告不可用，导出错误收集后更新界面并回传。
    /// - 注意事项: 主 Actor 任务开始时读取 dragFile 和代数，并非创建闭包时捕获；同一视图再次拖动可能覆盖这些字段。包装对象不自行保证回调只执行一次。
    func filePromiseProvider(_ filePromiseProvider: NSFilePromiseProvider, writePromiseTo url: URL,
                             completionHandler: @escaping (Error?) -> Void) {
        let completion = PromiseCompletion(completionHandler)
        Task { @MainActor in
            guard let file = self.dragFile, let store = self.store else {
                completion.call(ClientError.service("文件不可用，请重新拖动。"))
                return
            }
            let ticket = self.dragGeneration
            do {
                try await store.exportDraggedFile(file, to: url, generation: ticket)
                completion.call(nil)
            } catch { AppLog.capture(error);
                store.error = error.localizedDescription
                completion.call(error)
            }
        }
    }
}

/// 保存 AppKit 完成回调，供异步导出结束后通知系统。
/// 仅持有一个闭包，不记录完成状态；Sendable 声明不会自动让闭包具备线程安全能力。
private final class PromiseCompletion: @unchecked Sendable {
    /// 系统等待的文件写入完成回调；nil 表示成功，非 nil 表示失败。
    private let handler: (Error?) -> Void
    /// 保存系统提供的文件写入完成通知。
    /// - Parameters: handler: @escaping (Error?) -> Void 回调，nil 表示成功，非 nil 表示失败。
    /// - Returns: PromiseCompletion，持有闭包直至包装对象释放。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: @unchecked Sendable 由代码自行保证使用方式；包装器没有锁或只调用一次的状态保护。
    init(_ handler: @escaping (Error?) -> Void) { self.handler = handler }
    /// 把文件承诺导出的结果通知接收应用。
    /// - Parameters: error: Error?；nil 为导出成功，非 nil 为失败原因。
    /// - Returns: Void；直接执行保存的完成闭包。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 本方法不去重，也不切换线程；调用方负责仅通知一次并在合适的执行环境调用。
    func call(_ error: Error?) { handler(error) }
}
