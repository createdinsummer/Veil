import AppKit
import AVKit
import PDFKit
import SwiftUI

struct PreviewSheet: View {
    @EnvironmentObject private var store: AppStore
    @ObservedObject var asset: PreviewAsset
    @State private var confirmDelete = false

    var body: some View {
        VStack(spacing: 18) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 6) {
                    Text(asset.file.leafName).font(.title3.weight(.semibold)).lineLimit(1)
                    Text("\(asset.file.parentName) · \(asset.file.sizeLabel) · v\(asset.file.version)")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                Button { store.closePreview() } label: { Image(systemName: "xmark.circle.fill").foregroundStyle(.tertiary).font(.title2) }
                    .buttonStyle(VeilPlainButtonStyle()).keyboardShortcut(.cancelAction).accessibilityLabel("关闭预览")
            }
            if asset.unstable {
                Label("读取期间内容可能发生变化。", systemImage: "exclamationmark.triangle")
                    .font(.caption).foregroundStyle(.orange)
            }
            if let error = asset.error {
                ContentUnavailableView("无法预览", systemImage: "doc.questionmark", description: Text(error))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let url = asset.contentURL {
                if asset.file.previewMime == "text/plain" {
                    ScrollView {
                        Text(Self.decodedText(at: url)).font(.system(.body, design: .monospaced))
                            .textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading).padding(20)
                    }.background(.primary.opacity(0.025), in: RoundedRectangle(cornerRadius: 10))
                } else if asset.file.previewMime == "application/pdf" {
                    PDFPreview(url: url)
                } else if asset.file.previewMime?.hasPrefix("video/") == true {
                    VideoPreview(url: url)
                } else if let image = NSImage(contentsOf: url) {
                    Image(nsImage: image).resizable().scaledToFit().frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    ContentUnavailableView("无法显示此文件", systemImage: "doc", description: Text("可导出后使用系统应用查看。"))
                }
            } else {
                VStack(spacing: 13) { ProgressView(); Text("正在解密文件…").foregroundStyle(.secondary) }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            HStack {
                Button("删除…", systemImage: "trash", role: .destructive) { confirmDelete = true }.disabled(store.isBusy)
                Spacer()
                Button("用系统应用打开", systemImage: "arrow.up.forward.app") { Task { await store.openPreviewExternally() } }
                    .buttonStyle(VeilButtonStyle()).disabled(store.isBusy || asset.contentURL == nil)
                    .help("先解密到临时文件，再用系统应用打开")
                Button { Task { await store.chooseApplicationForPreview() } } label: {
                    Image(systemName: "ellipsis.circle")
                }
                .buttonStyle(VeilIconButtonStyle()).disabled(store.isBusy || asset.contentURL == nil)
                .help("选择用于打开的应用…").accessibilityLabel("选择用于打开的应用")
                Button("导出…", systemImage: "square.and.arrow.up") { Task { await store.exportFile(asset.file) } }
                    .buttonStyle(VeilButtonStyle()).disabled(store.isBusy)
                Button("完成") { store.closePreview() }.buttonStyle(VeilButtonStyle(variant: .primary)).keyboardShortcut(.defaultAction)
            }
        }.padding(24).frame(width: 750, height: 570).foregroundStyle(VeilTheme.text).background(VeilTheme.card).tint(VeilTheme.accent)
            .onDisappear { asset.close() }
            .alert("删除「\(asset.file.leafName)」？", isPresented: $confirmDelete) {
                Button("删除", role: .destructive) {
                    Task { await store.confirmDelete(.file(asset.file)) }
                }
                Button("取消", role: .cancel) { }
            } message: { Text("此文件将从容器中删除。此操作无法撤销。") }
    }

    /// 文本预览从临时文件读回内存展示；解码失败时给出可读提示。
    private static func decodedText(at url: URL) -> String {
        guard let data = try? Data(contentsOf: url) else { return "无法读取文件内容" }
        return String(decoding: data, as: UTF8.self)
    }
}

private struct PDFPreview: NSViewRepresentable {
    let url: URL
    func makeNSView(context: Context) -> PDFView {
        let view = PDFView()
        view.autoScales = true
        view.document = PDFDocument(url: url)
        return view
    }
    func updateNSView(_ view: PDFView, context: Context) { }
}

private struct VideoPreview: NSViewRepresentable {
    // 使用 AppKit 的 AVPlayerView 而不是 SwiftUI 的 VideoPlayer：
    // VideoPlayer 经由 _AVKit_SwiftUI 在 macOS 27 上创建视图时会触发
    // getSuperclassMetadata 崩溃（SIGABRT），AVPlayerView 不经过该框架。
    let url: URL

    func makeNSView(context: Context) -> AVPlayerView {
        let view = AVPlayerView()
        view.controlsStyle = .inline
        view.videoGravity = .resizeAspect
        view.allowsPictureInPicturePlayback = false
        view.player = AVPlayer(url: url)
        return view
    }

    func updateNSView(_ view: AVPlayerView, context: Context) { }

    static func dismantleNSView(_ view: AVPlayerView, coordinator: ()) {
        view.player?.pause()
        view.player = nil
    }
}
