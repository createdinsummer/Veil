import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// 使用系统文件承诺，在接收方确定目录后才解密文件。
struct DragExportHandle: NSViewRepresentable {
    let file: FileEntry
    let store: AppStore

    func makeNSView(context: Context) -> DragExportView { DragExportView() }
    func updateNSView(_ view: DragExportView, context: Context) {
        view.file = file
        view.store = store
        view.toolTip = "拖到应用外导出 \(file.leafName)"
    }
    func sizeThatFits(_ proposal: ProposedViewSize, nsView: DragExportView, context: Context) -> CGSize? {
        CGSize(width: 20, height: 28)
    }
}

final class DragExportView: NSView, NSDraggingSource, NSFilePromiseProviderDelegate {
    var file: FileEntry?
    weak var store: AppStore?
    private var dragFile: FileEntry?
    private var dragGeneration: UInt64 = 0

    override func draw(_ dirtyRect: NSRect) {
        guard let image = NSImage(systemSymbolName: "line.3.horizontal", accessibilityDescription: "拖动导出") else { return }
        image.draw(in: NSRect(x: 4, y: 8, width: 12, height: 12))
    }

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

    func draggingSession(_ session: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext) -> NSDragOperation { .copy }
    func filePromiseProvider(_ filePromiseProvider: NSFilePromiseProvider, fileNameForType fileType: String) -> String {
        dragFile?.leafName ?? "文件"
    }
    func operationQueue(for filePromiseProvider: NSFilePromiseProvider) -> OperationQueue { .main }
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
            } catch {
                store.error = error.localizedDescription
                completion.call(error)
            }
        }
    }
}

private final class PromiseCompletion: @unchecked Sendable {
    private let handler: (Error?) -> Void
    init(_ handler: @escaping (Error?) -> Void) { self.handler = handler }
    func call(_ error: Error?) { handler(error) }
}
