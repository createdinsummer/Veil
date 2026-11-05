import AppKit
import SwiftUI
import UniformTypeIdentifiers

struct ImportPackageView: View {
    @EnvironmentObject private var store: AppStore
    @State private var selectedURL: URL?
    @State private var showingPassword = false
    @State private var password = ""
    @State private var selectionError: String?
    @State private var isTargeted = false
    @FocusState private var passwordFocused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("导入容器").font(.system(size: 20, weight: .semibold))
            Text(showingPassword ? "输入容器密码，导入到「\(store.activeSpace?.name ?? "本机")」。" : "选择 .veil 文件，导入到「\(store.activeSpace?.name ?? "本机")」。")
                .font(.system(size: 12)).foregroundStyle(VeilTheme.secondary)
            if showingPassword {
                HStack(spacing: 12) {
                    Image(systemName: "checkmark.shield").font(.system(size: 20))
                    VStack(alignment: .leading, spacing: 4) {
                        Text(selectedURL?.lastPathComponent ?? "").font(.system(size: 12, weight: .semibold)).lineLimit(1)
                        Text("导入到 \(store.activeSpace?.name ?? "本机")").font(.system(size: 11)).foregroundStyle(VeilTheme.muted)
                    }
                    Spacer()
                }.padding(14).frame(maxWidth: .infinity, alignment: .leading)
                    .background(VeilTheme.canvas, in: RoundedRectangle(cornerRadius: 9))
                VStack(alignment: .leading, spacing: 8) {
                    Text("容器密码").font(.system(size: 12, weight: .medium))
                    SecureField("输入容器密码", text: $password).textFieldStyle(.roundedBorder)
                        .focused($passwordFocused).disabled(store.isBusy)
                }
                if let error = store.error { Text(error).font(.system(size: 11)).foregroundStyle(VeilTheme.danger) }
            } else {
                Button(action: chooseFile) {
                    HStack(spacing: 13) {
                        Image(systemName: "doc").font(.system(size: 20)).foregroundStyle(VeilTheme.secondary)
                        VStack(alignment: .leading, spacing: 4) {
                            Text(selectedURL?.lastPathComponent ?? "选择或拖入文件")
                                .font(.system(size: 12, weight: .semibold)).lineLimit(1)
                            Text(selectedURL == nil ? "支持 .veil 文件" : "点击更换文件")
                                .font(.system(size: 11)).foregroundStyle(VeilTheme.muted)
                        }
                        Spacer()
                    }.padding(20).frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
                        .background(isTargeted ? VeilTheme.canvas : VeilTheme.card, in: RoundedRectangle(cornerRadius: 12))
                        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(VeilTheme.border, style: StrokeStyle(lineWidth: 1, dash: [5, 4])))
                }.buttonStyle(VeilPlainButtonStyle())
                    .dropDestination(for: URL.self) { urls, _ in
                        guard let url = urls.first, urls.count == 1 else { selectionError = "请选择一个 .veil 文件"; return false }
                        return selectFile(url)
                    } isTargeted: { isTargeted = $0 }
                if let selectionError { Text(selectionError).font(.system(size: 11)).foregroundStyle(VeilTheme.danger) }
            }
            HStack {
                Spacer()
                Button("取消") { store.showImportPackage = false }.buttonStyle(VeilButtonStyle())
                    .keyboardShortcut(.cancelAction).disabled(store.isBusy)
                if showingPassword {
                    Button(store.isBusy ? "正在导入…" : "导入容器") { importPackage() }
                        .buttonStyle(VeilButtonStyle(variant: .primary)).keyboardShortcut(.defaultAction)
                        .disabled(store.isBusy || password.isEmpty)
                } else {
                    Button("继续") { showingPassword = true; passwordFocused = true }
                        .buttonStyle(VeilButtonStyle(variant: .primary)).keyboardShortcut(.defaultAction)
                        .disabled(selectedURL == nil)
                }
            }.padding(.top, 4)
        }.padding(25).frame(width: 430).background(VeilTheme.card)
            .interactiveDismissDisabled(store.isBusy)
            .onDisappear { password = ""; store.error = nil }
    }

    private func chooseFile() {
        let panel = NSOpenPanel()
        panel.title = "选择 .veil 文件"
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.allowedContentTypes = [UTType(filenameExtension: "veil") ?? .data]
        guard panel.runModal() == .OK, let url = panel.url else { return }
        _ = selectFile(url)
    }

    @discardableResult
    private func selectFile(_ url: URL) -> Bool {
        guard url.isFileURL, url.pathExtension.lowercased() == "veil",
              (try? url.resourceValues(forKeys: [.isRegularFileKey]).isRegularFile) == true else {
            selectedURL = nil
            selectionError = "请选择 .veil 文件"
            return false
        }
        selectedURL = url
        selectionError = nil
        return true
    }

    private func importPackage() {
        guard let url = selectedURL, let space = store.activeSpace, !store.isBusy else { return }
        let secret = password
        password = ""
        Task {
            let scoped = url.startAccessingSecurityScopedResource()
            defer { if scoped { url.stopAccessingSecurityScopedResource() } }
            await store.submit(.importPackage(path: url.path, space: space.id), name: "", password: secret, confirmation: "")
            if store.error == nil, store.container != nil { store.showImportPackage = false }
        }
    }
}
