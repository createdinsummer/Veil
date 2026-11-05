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
            if let error = asset.error {
                ContentUnavailableView("无法预览", systemImage: "doc.questionmark", description: Text(error))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let url = asset.videoURL {
                VideoPreview(url: url)
            } else if let data = asset.data {
                if asset.file.previewMime == "text/plain" {
                    ScrollView {
                        Text(String(decoding: data, as: UTF8.self)).font(.system(.body, design: .monospaced))
                            .textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading).padding(20)
                    }.background(.primary.opacity(0.025), in: RoundedRectangle(cornerRadius: 10))
                } else if asset.file.previewMime == "application/pdf" {
                    PDFPreview(data: data)
                } else if let image = NSImage(data: data) {
                    Image(nsImage: image).resizable().scaledToFit().frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    ContentUnavailableView("无法显示此文件", systemImage: "doc", description: Text("可导出后使用系统应用查看。"))
                }
            } else {
                VStack(spacing: 13) { ProgressView(); Text("正在解密预览…").foregroundStyle(.secondary) }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            HStack {
                Button("删除…", systemImage: "trash", role: .destructive) { confirmDelete = true }.disabled(store.isBusy)
                Spacer()
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
}

private struct PDFPreview: NSViewRepresentable {
    let data: Data
    func makeNSView(context: Context) -> PDFView {
        let view = PDFView()
        view.autoScales = true
        view.document = PDFDocument(data: data)
        return view
    }
    func updateNSView(_ view: PDFView, context: Context) { }
}

private struct VideoPreview: View {
    let url: URL
    @State private var player = AVPlayer()
    var body: some View {
        VideoPlayer(player: player)
            .onAppear { player.replaceCurrentItem(with: AVPlayerItem(url: url)) }
            .onDisappear { player.pause(); player.replaceCurrentItem(with: nil) }
    }
}
