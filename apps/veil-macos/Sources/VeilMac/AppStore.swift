import AppKit
import Foundation
import SwiftUI
import UniformTypeIdentifiers

@MainActor
final class AppStore: ObservableObject {
    @Published private(set) var spaces: [Space] = []
    @Published private(set) var recents: [RecentContainer] = []
    @Published private(set) var container: ContainerSnapshot?
    @Published private(set) var pendingOperations = 0
    @Published var selectedSpaceID = ""
    @Published var directory = ""
    @Published var categories: Set<FileCategory> = []
    @Published var search = ""
    @Published var layout: FileLayout = .grid
    @Published var browseMode: BrowseMode = .type
    @Published var form: ContainerForm?
    @Published var showNewSpace = false
    @Published var pendingDelete: BrowserEntry?
    @Published var preview: PreviewAsset?
    @Published var error: String?
    @Published var notice: String?
    @Published private(set) var thumbnails: [String: Data] = [:]

    private let client = ServiceClient()
    private var generation: UInt64 = 0
    private var loaded = false
    private var thumbnailBatch = UUID()
    private var thumbnailVersions: [String: Int] = [:]

    var isBusy: Bool { pendingOperations > 0 }
    var activeSpace: Space? { spaces.first { $0.id == selectedSpaceID } ?? spaces.first { $0.isDefault } ?? spaces.first }
    var visibleEntries: [BrowserEntry] {
        guard let container else { return [] }
        return browserEntries(container: container, mode: browseMode, directory: directory, categories: categories, search: search)
    }
    var externalRecents: [RecentContainer] {
        recents.filter { recent in !spaces.contains { $0.containers.contains { $0.id == recent.id } } }
    }

    private var thumbnailCandidates: [FileEntry] {
        guard layout == .grid else { return [] }
        return Array(visibleEntries.compactMap { entry -> FileEntry? in
            guard case .file(let file) = entry, file.previewMime?.hasPrefix("image/") == true,
                  file.size <= 2 * 1024 * 1024 else { return nil }
            return file
        }.prefix(12))
    }

    var thumbnailKey: String {
        "\(generation):\(container?.id ?? ""):\(layout.rawValue):" + thumbnailCandidates.map { "\($0.id):\($0.version)" }.joined(separator: ",")
    }

    func loadThumbnails() async {
        let batch = UUID(), ticket = generation
        thumbnailBatch = batch
        let candidates = thumbnailCandidates
        let ids = Set(candidates.map(\.id))
        let versions = Dictionary(uniqueKeysWithValues: candidates.map { ($0.id, $0.version) })
        thumbnails = thumbnails.filter { ids.contains($0.key) && versions[$0.key] == thumbnailVersions[$0.key] }
        thumbnailVersions = versions
        for file in candidates where thumbnails[file.id] == nil {
            guard !Task.isCancelled, ticket == generation, batch == thumbnailBatch else { return }
            do {
                let image = try await client.thumbnail(name: file.name)
                guard !Task.isCancelled, ticket == generation, batch == thumbnailBatch else { return }
                thumbnails[file.id] = image
            } catch { /* 不可解码的图片使用类型图标，不影响文件操作。 */ }
        }
    }

    func load() async {
        guard !loaded else { return }
        await perform { _ in
            try await self.reloadCatalog()
            let layout: String = try await self.client.json(NativeRequest(operation: "get_preference", key: "layout"))
            let mode: String = try await self.client.json(NativeRequest(operation: "get_preference", key: "view_mode"))
            self.layout = FileLayout(rawValue: layout) ?? .grid
            self.browseMode = BrowseMode(rawValue: mode) ?? .type
            self.loaded = true
        }
    }

    private func perform(_ work: (UInt64) async throws -> Void) async {
        guard !isBusy else { return }
        pendingOperations += 1
        error = nil
        let ticket = generation
        defer { pendingOperations -= 1 }
        do { try await work(ticket) }
        catch { if ticket == generation { self.error = error.localizedDescription } }
    }

    private func reloadCatalog() async throws {
        spaces = try await client.json(NativeRequest(operation: "list_spaces"))
        recents = try await client.json(NativeRequest(operation: "list_recents"))
        if !spaces.contains(where: { $0.id == selectedSpaceID }) { selectedSpaceID = activeSpace?.id ?? "" }
    }

    private func resetBrowser() {
        thumbnailBatch = UUID()
        thumbnails = [:]
        thumbnailVersions = [:]
        directory = ""
        categories = []
        search = ""
        closePreview()
        pendingDelete = nil
    }

    /// 先隐藏敏感内容；迟到的解锁和预览响应不能恢复界面。
    @discardableResult
    func lock() async -> Bool {
        generation &+= 1
        container = nil
        form = nil
        resetBrowser()
        notice = nil
        pendingOperations += 1
        defer { pendingOperations -= 1 }
        do {
            try await client.execute(NativeRequest(operation: "lock"))
            return true
        } catch {
            self.error = error.localizedDescription
            return false
        }
    }

    func selectSpace(_ space: Space) async {
        guard !isBusy else { return }
        if container != nil { guard await lock() else { return } }
        selectedSpaceID = space.id
        form = nil
        resetBrowser()
        error = nil
    }

    func chooseContainer(_ item: ContainerSummary) async {
        guard !isBusy else { return }
        if container?.id == item.id { return }
        if container != nil { guard await lock() else { return } }
        if let space = spaces.first(where: { $0.containers.contains(where: { $0.id == item.id }) }) { selectedSpaceID = space.id }
        error = nil
        form = .open(path: item.path, name: item.name)
    }

    func newContainer(in spaceID: String? = nil) async {
        guard !isBusy, let space = spaceID ?? activeSpace?.id, spaces.contains(where: { $0.id == space }) else { return }
        if container != nil { guard await lock() else { return } }
        selectedSpaceID = space
        resetBrowser()
        error = nil
        form = .create(space: space)
    }

    func cancelForm() {
        guard !isBusy else { return }
        form = nil
        error = nil
    }

    func chooseContainerDirectory() async {
        guard !isBusy else { return }
        let panel = NSOpenPanel()
        panel.title = "选择包含 veil.db 的容器目录"
        panel.prompt = "打开容器"
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        guard panel.runModal() == .OK, let url = panel.url else { return }
        await chooseContainer(ContainerSummary(id: url.path, name: url.lastPathComponent, path: url.path))
    }

    func choosePackage() async {
        guard !isBusy, let space = activeSpace else { return }
        let panel = NSOpenPanel()
        panel.title = "导入 .veil 加密容器"
        panel.allowedContentTypes = [UTType(filenameExtension: "veil") ?? .data]
        panel.allowsOtherFileTypes = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        if container != nil { guard await lock() else { return } }
        resetBrowser()
        error = nil
        form = .importPackage(path: url.path, space: space.id)
    }

    func submit(_ form: ContainerForm, name: String, password: String, confirmation: String) async {
        guard !password.isEmpty else { error = "请输入容器密码"; return }
        if form.isCreate && password != confirmation { error = "两次输入的密码不一致"; return }
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        if form.isCreate && name.isEmpty { error = "请输入容器名称"; return }
        await perform { ticket in
            // Rust 的会话仍负责真实的锁定与密码校验。
            try await self.client.execute(NativeRequest(operation: "lock"))
            guard ticket == self.generation else { return }
            self.container = nil
            self.resetBrowser()
            let request: NativeRequest
            switch form {
            case .create(let space): request = NativeRequest(operation: "create_container", space: space, name: name, password: password)
            case .open(let path, _): request = NativeRequest(operation: "open_container", password: password, path: path)
            case .importPackage(let path, let space): request = NativeRequest(operation: "import_package", space: space, password: password, path: path)
            }
            let view: ContainerSnapshot = try await self.client.json(request)
            guard ticket == self.generation else {
                // actor 调度不保证先进先出；即使锁定先于迟到的解锁执行，也须再次清理会话。
                try await self.client.execute(NativeRequest(operation: "lock"))
                return
            }
            self.container = view
            self.form = nil
            do {
                try await self.reloadCatalog()
                if let space = self.spaces.first(where: { $0.containers.contains(where: { $0.id == view.id }) }) { self.selectedSpaceID = space.id }
            } catch { self.notice = "容器已打开，空间列表刷新失败：\(error.localizedDescription)" }
        }
    }

    func createSpace(name: String) async {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        await perform { ticket in
            let spaces: [Space] = try await self.client.json(NativeRequest(operation: "create_space", name: name))
            guard ticket == self.generation else { return }
            self.spaces = spaces
            self.showNewSpace = false
            // 创建空间不隐式切换已打开的容器。
            if self.container == nil {
                self.form = nil
                self.selectedSpaceID = spaces.first(where: { $0.name == name })?.id ?? self.selectedSpaceID
            }
        }
    }

    func refresh() async {
        await perform { ticket in
            if self.container != nil {
                let view: ContainerSnapshot = try await self.client.json(NativeRequest(operation: "refresh"))
                guard ticket == self.generation else { return }
                self.container = view
            }
            try await self.reloadCatalog()
            if ticket == self.generation { self.notice = "已刷新" }
        }
    }

    func savePreferences() async {
        let layout = layout.rawValue, mode = browseMode.rawValue
        do {
            try await client.execute(NativeRequest(operation: "set_preference", key: "layout", value: layout))
            try await client.execute(NativeRequest(operation: "set_preference", key: "view_mode", value: mode))
        } catch { self.error = error.localizedDescription }
    }

    func chooseImport(directory: Bool) async {
        guard !isBusy, container != nil else { return }
        let panel = NSOpenPanel()
        panel.title = directory ? "添加文件夹" : "添加文件"
        panel.canChooseDirectories = directory
        panel.canChooseFiles = !directory
        panel.allowsMultipleSelection = true
        guard panel.runModal() == .OK else { return }
        await importFiles(panel.urls)
    }

    func importFiles(_ urls: [URL]) async {
        guard !urls.isEmpty, container != nil else { return }
        let destination = browseMode == .path ? directory : ""
        let scoped = urls.filter { $0.startAccessingSecurityScopedResource() }
        defer { scoped.forEach { $0.stopAccessingSecurityScopedResource() } }
        await perform { ticket in
            let view: ContainerSnapshot = try await self.client.json(NativeRequest(operation: "import_files", sources: urls.map(\.path), directory: destination))
            guard ticket == self.generation else { return }
            self.container = view
            self.notice = "文件已添加"
        }
    }

    func activate(_ entry: BrowserEntry) async {
        switch entry {
        case .folder(let path): directory = path
        case .file(let file): await showPreview(file)
        }
    }

    func showPreview(_ file: FileEntry) async {
        guard !isBusy else { return }
        closePreview()
        let asset = PreviewAsset(file: file)
        preview = asset
        guard file.previewMime != nil else { asset.error = "此类型暂不支持内置预览，可导出后查看。"; return }
        await perform { ticket in
            do {
                let data = try await self.client.preview(name: file.name)
                guard ticket == self.generation, self.preview === asset else { return }
                try asset.install(data)
            } catch {
                if ticket == self.generation && self.preview === asset { asset.error = error.localizedDescription }
            }
        }
    }

    func closePreview() { preview?.close(); preview = nil }

    func exportFile(_ file: FileEntry) async {
        guard !isBusy else { return }
        let panel = NSSavePanel()
        panel.title = "导出文件"
        panel.nameFieldStringValue = file.leafName
        panel.canCreateDirectories = true
        guard panel.runModal() == .OK, let url = panel.url else { return }
        await perform { ticket in
            try await self.client.execute(NativeRequest(operation: "export_file", name: file.name, destination: url.path))
            if ticket == self.generation { self.notice = "文件已导出" }
        }
    }

    func exportPackage() async {
        guard !isBusy, let container else { return }
        let panel = NSSavePanel()
        panel.title = "导出加密备份"
        panel.nameFieldStringValue = container.name.replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: ":", with: "_") + ".veil"
        panel.allowedContentTypes = [UTType(filenameExtension: "veil") ?? .data]
        panel.canCreateDirectories = true
        guard panel.runModal() == .OK, let url = panel.url else { return }
        await perform { ticket in
            try await self.client.execute(NativeRequest(operation: "export_package", destination: url.path))
            if ticket == self.generation { self.notice = "加密备份已导出" }
        }
    }

    func confirmDelete(_ confirmedTarget: BrowserEntry? = nil) async {
        guard let target = confirmedTarget ?? pendingDelete else { return }
        pendingDelete = nil
        await perform { ticket in
            let view: ContainerSnapshot = try await self.client.json(NativeRequest(operation: "remove_path", name: target.path))
            guard ticket == self.generation else { return }
            self.closePreview()
            self.container = view
            if self.directory == target.path || self.directory.hasPrefix(target.path + "/") { self.directory = "" }
            self.notice = "已删除"
        }
    }
}

@MainActor
final class PreviewAsset: ObservableObject, Identifiable {
    let file: FileEntry
    nonisolated var id: String { file.id }
    @Published var data: Data?
    @Published var videoURL: URL?
    @Published var error: String?
    private var temporaryDirectory: URL?

    init(file: FileEntry) { self.file = file }

    func install(_ bytes: Data) throws {
        if file.previewMime?.hasPrefix("video/") == true {
            let folder = FileManager.default.temporaryDirectory.appendingPathComponent("veil-native-preview-" + UUID().uuidString, isDirectory: true)
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
            temporaryDirectory = folder
            let url = folder.appendingPathComponent(file.leafName)
            do {
                try bytes.write(to: url, options: .atomic)
                try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
                videoURL = url
            } catch { close(); throw error }
        } else { data = bytes }
    }

    func close() {
        data = nil
        videoURL = nil
        if let temporaryDirectory { try? FileManager.default.removeItem(at: temporaryDirectory) }
        temporaryDirectory = nil
    }

    deinit { if let temporaryDirectory { try? FileManager.default.removeItem(at: temporaryDirectory) } }
}
