/// 容器文件预览窗口及系统原生 PDF / 视频视图桥接。
/// 所属模块：macOS 文件浏览；展示临时解密文件、读取状态、失败原因和导出 / 删除 / 外部打开操作。
/// 核心数据：PreviewAsset 提供文件条目、临时内容 URL、错误和稳定性标记；AppStore 执行外部操作。
/// 依赖 AppKit、AVKit、PDFKit 和 SwiftUI。整体目标是根据 MIME 类型选择合适预览控件，并在窗口关闭时清理明文临时资源。
import AppKit
import AVKit
import PDFKit
import SwiftUI

/// 展示单个文件的解密结果及导出、删除、外部打开入口。
/// 观察 PreviewAsset 的内容 URL、错误和稳定性状态，只保存删除确认状态；实际资源生命周期由 Asset 和 Store 管理。
struct PreviewSheet: View {
    /// 负责导出、外部打开、删除和关闭预览的应用状态。
    @EnvironmentObject private var store: AppStore
    /// 当前预览任务状态与文件元数据；变化时自动刷新视图。
    @ObservedObject var asset: PreviewAsset
    /// 控制删除二次确认对话框。
    @State private var confirmDelete = false

    /// 按临时内容、加载状态、MIME 类型及错误情况构造预览窗口。
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
            // 读取模式不能保证稳定快照时显示提示；标记为 true 不代表已经检测到实际修改。
            if asset.unstable {
                Label("读取期间内容可能发生变化。", systemImage: "exclamationmark.triangle")
                    .font(.caption).foregroundStyle(.orange)
            }
            if let error = asset.error {
                ContentUnavailableView("无法预览", systemImage: "doc.questionmark", description: Text(error))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            // 只有临时解密内容已准备好时才创建预览控件。
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

    /// 为文本预览读取临时明文文件并转换成可显示文本。
    /// - Parameters: url: URL 已解密的临时文件位置。
    /// - Returns: String；读取失败返回“无法读取文件内容”，读取成功后按 UTF-8 解码，非法字节以替代字符显示。
    /// - Throws: 不向调用方抛错，文件读取错误被忽略。
    /// - 注意事项: 同步将整个文件读入内存，无大小截断；视图重新求值可能再次读取，不能保证文件读取期间不被修改。
    private static func decodedText(at url: URL) -> String {
        guard let data = try? Data(contentsOf: url) else { return "无法读取文件内容" }
        return String(decoding: data, as: UTF8.self)
    }
}

/// 将已解密 PDF URL 接入 PDFKit 的原生缩放预览控件。
private struct PDFPreview: NSViewRepresentable {
    /// 解密后的临时 PDF 文件 URL；仅创建原生视图时用于载入文档。
    let url: URL
    /// 为已解密 PDF 建立自动缩放的原生文档视图。
    /// - Parameters: context: Context SwiftUI 桥接上下文，本实现不使用。
    /// - Returns: PDFView；启用自动缩放并尝试载入当前 url，文档不可读或无效时 document 可为 nil。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 主线程创建视图；PDFDocument 同步读取，失败没有额外错误提示。
    func makeNSView(context: Context) -> PDFView {
        let view = PDFView()
        view.autoScales = true
        view.document = PDFDocument(url: url)
        return view
    }
    /// 满足 PDF 原生视图的更新接口，保留已载入文档。
    /// - Parameters: view: PDFView 现有视图；context: Context 桥接上下文，本实现均不使用。
    /// - Returns: Void，不执行任何更新。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 即使输入 url 改变也不会重新载入；调用方需要通过视图生命周期重新创建才能应用新 URL。
    func updateNSView(_ view: PDFView, context: Context) { }
}

/// 用 AppKit AVPlayerView 显示临时视频文件，规避不稳定的 SwiftUI VideoPlayer 桥接。
private struct VideoPreview: NSViewRepresentable {
    // 使用原生 AVPlayerView 绕过 SwiftUI VideoPlayer 桥接层，减少对该桥接初始化路径的依赖。
    /// 解密后的临时视频 URL；创建时交给 AVPlayer，后续更新不会替换播放器。
    let url: URL

    /// 为临时视频建立带内联控制条的原生播放器。
    /// - Parameters: context: Context SwiftUI 桥接上下文，本实现不使用。
    /// - Returns: AVPlayerView；按比例显示、禁用画中画，并绑定当前 url 的 AVPlayer。
    /// - Throws: 不直接抛错，后续媒体加载或播放失败由播放器状态体现。
    /// - 注意事项: 主线程创建，不主动开始播放，也没有提前验证媒体是否可播放。
    func makeNSView(context: Context) -> AVPlayerView {
        let view = AVPlayerView()
        view.controlsStyle = .inline
        view.videoGravity = .resizeAspect
        view.allowsPictureInPicturePlayback = false
        view.player = AVPlayer(url: url)
        return view
    }

    /// 保留当前视频播放器，不在 SwiftUI 更新时重建播放资源。
    /// - Parameters: view: AVPlayerView 现有播放器视图；context: Context 桥接上下文，均不使用。
    /// - Returns: Void，不修改播放器。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 即使 url 变化也不替换媒体；调用方应为不同预览建立新的视图生命周期。
    func updateNSView(_ view: AVPlayerView, context: Context) { }

    /// 在原生视频视图被拆除时停止播放并释放视图持有的播放器。
    /// - Parameters: view: AVPlayerView 待拆除视图；coordinator: () 协议要求的空协调器值，不使用。
    /// - Returns: Void；暂停现有播放器后将引用置为 nil。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 只解除播放器引用，不在此删除临时文件；文件清理由 PreviewAsset.close 负责。
    static func dismantleNSView(_ view: AVPlayerView, coordinator: ()) {
        view.player?.pause()
        view.player = nil
    }
}
