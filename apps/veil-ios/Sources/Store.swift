import SwiftUI
import PhotosUI
import CoreTransferable
import UniformTypeIdentifiers
import AVFoundation
import ImageIO

@MainActor final class Store: ObservableObject {
    @Published var spaces: [Space] = []
    @Published var selectedSpace = ""
    @Published var container: ContainerSnapshot?
    @Published var directory = ""
    @Published var search = ""
    @Published var mode: BrowseMode = .type
    @Published var category: FileCategory?
    @Published var layout: FileLayout = .grid
    @Published var conflictOperation = "导入"
    @Published var exportSelection: BrowserEntry?
    @Published var importInspection: ImportInspection?
    private var importReview: CheckedContinuation<[ImportResolution]?, Never>?
    @Published var error: String?
    @Published var notice: String?
    @Published var importStatus: String?
    @Published var busy = false
    @Published var preview: PreviewItem? { didSet { if oldValue?.id != preview?.id { oldValue?.removeTemporaryFiles() } } }
    @Published var export: ExportItem?
    @Published private(set) var thumbnails: [String: UIImage] = [:]
    @Published private(set) var textThumbnails: [String: String] = [:]
    private var thumbnailTasks: Set<String> = []
    private var thumbnailFailures: Set<String> = []
    private let client = ServiceClient()
    private var generation = 0
    var space: Space? { spaces.first { $0.id == selectedSpace } ?? spaces.first }
    var entries: [BrowserEntry] { container.map { browserEntries(container: $0, mode: mode, directory: directory, categories: category.map { [$0] } ?? [], search: search) } ?? [] }
    func run(_ action: (Int) async throws -> Void) async {
        guard !busy else { return }
        busy = true; error = nil
        let ticket = generation
        defer { busy = false }
        do { try await action(ticket) } catch { if ticket == generation { self.error = error.localizedDescription } }
    }
    func load() async { await run { _ in
        self.removeStalePreviews()
        try await self.reload()
    } }
    private func reload() async throws {
        spaces = try await client.json(NativeRequest(operation: "list_spaces"))
        if !spaces.contains(where: { $0.id == selectedSpace }) { selectedSpace = spaces.first(where: { $0.isDefault })?.id ?? spaces.first?.id ?? "" }
    }
    func refreshSpaceAvailability() async {
        guard !busy else { return }
        let wasRegisteredContainer = container.map { opened in
            spaces.contains(where: { $0.containers.contains(where: { $0.id == opened.id }) })
        } ?? false
        do {
            try await reload()
            if wasRegisteredContainer, let container,
               !spaces.contains(where: { $0.containers.contains(where: { $0.id == container.id }) }) {
                await lock()
            }
        } catch { return }
    }
    /// 先清除界面上的敏感内容，再串行锁定会话并删除应用私有数据。
    func clearLocalData() async {
        guard !busy else { return }
        generation += 1
        container = nil; preview = nil; export = nil; exportSelection = nil; directory = ""; search = ""; category = nil
        thumbnails.removeAll(); textThumbnails.removeAll(); thumbnailTasks.removeAll(); thumbnailFailures.removeAll()
        busy = true; error = nil
        defer { busy = false }
        do {
            try await client.clearLocalData()
            spaces = []; selectedSpace = ""
            try await reload()
            notice = "本机数据已清理"
        } catch {
            // 清理可能部分完成，重新读取可用状态并如实报告错误。
            spaces = []; selectedSpace = ""
            try? await reload()
            self.error = "清理未完全完成：\(error.localizedDescription)"
        }
    }
    func createSpace(_ name: String) async { await run { ticket in
        let items: [Space] = try await self.client.json(NativeRequest(operation: "create_space", name: name))
        guard ticket == self.generation else { return }; self.spaces = items; self.selectedSpace = items.first(where: { $0.name == name })?.id ?? self.selectedSpace
    } }
    func deleteContainer(_ item: ContainerSummary, password: String) async -> Bool {
        var deleted = false
        await run { ticket in
            let items: [Space] = try await self.client.json(
                NativeRequest(operation: "delete_container", id: item.id, password: password)
            )
            guard ticket == self.generation else { return }
            if self.container?.id == item.id {
                self.container = nil; self.preview = nil; self.export = nil; self.exportSelection = nil
                self.directory = ""; self.search = ""; self.category = nil
                self.thumbnails.removeAll(); self.textThumbnails.removeAll()
                self.thumbnailTasks.removeAll(); self.thumbnailFailures.removeAll()
            }
            self.spaces = items
            if !items.contains(where: { $0.id == self.selectedSpace }) {
                self.selectedSpace = items.first(where: { $0.isDefault })?.id ?? items.first?.id ?? ""
            }
            self.notice = "容器已删除"
            deleted = true
        }
        return deleted
    }
    func deleteSpace(_ space: Space) async {
        await run { ticket in
            let openContainer = self.container
            let belongsToSpace = space.containers.contains(where: { $0.id == openContainer?.id })
                || openContainer.map { URL(fileURLWithPath: $0.path).deletingLastPathComponent().path == space.path } == true
            let items: [Space] = try await self.client.json(
                NativeRequest(operation: space.isAvailable ? "delete_space" : "delete_space_record", id: space.id)
            )
            guard ticket == self.generation else { return }
            if belongsToSpace {
                self.container = nil; self.preview = nil; self.export = nil; self.exportSelection = nil
                self.directory = ""; self.search = ""; self.category = nil
                self.thumbnails.removeAll(); self.textThumbnails.removeAll()
                self.thumbnailTasks.removeAll(); self.thumbnailFailures.removeAll()
            }
            self.spaces = items
            if !items.contains(where: { $0.id == self.selectedSpace }) {
                self.selectedSpace = items.first(where: { $0.isDefault })?.id ?? items.first?.id ?? ""
            }
            self.notice = space.isAvailable
                ? (space.isDefault ? "默认空间已删除并重新建立" : "空间已删除")
                : "空间记录已删除，磁盘文件仍保留"
        }
    }
    func open(form: ContainerForm, name: String, password: String) async -> Bool {
        let spaceID: String?
        switch form {
        case .create(let space): spaceID = space
        case .importPackage(_, let space): spaceID = space
        case .open: spaceID = nil
        }
        if let spaceID, !spaces.contains(where: { $0.id == spaceID && $0.isAvailable }) {
            error = "空间当前不可用，请重新连接外置磁盘后再试"
            return false
        }
        var success = false
        await run { ticket in
            let request: NativeRequest
            switch form {
            case .create(let space): request = NativeRequest(operation: "create_container", space: space, name: name, password: password)
            case .open(let path, _): request = NativeRequest(operation: "open_container", password: password, path: path)
            case .importPackage(let path, let space): request = NativeRequest(operation: "import_package", space: space, password: password, path: path)
            }
            let result: ContainerSnapshot = try await self.client.json(request)
            guard ticket == self.generation else { try await self.client.execute(NativeRequest(operation: "lock")); return }
            self.container = result; self.directory = ""; success = true
            try await self.reload()
        }
        return success
    }
    func lock() async {
        generation += 1; _ = client.cancelTransfer(); completeImportReview(nil); container = nil; preview = nil; export = nil; exportSelection = nil; directory = ""; search = ""; category = nil
        thumbnails.removeAll(); textThumbnails.removeAll(); thumbnailTasks.removeAll(); thumbnailFailures.removeAll()
        do { try await client.execute(NativeRequest(operation: "lock")) } catch { self.error = error.localizedDescription }
    }
    var importProgressDescription: String? {
        guard let progress = importProgress, progress.total_files > 0 else { return nil }
        return "已完成 \(progress.completed_files) / \(progress.total_files) · \(Int(min(1, Double(progress.completed_files) / Double(progress.total_files))*100))%" + (progress.scan_complete ? " · 遍历完成" : " · 总数持续增加")
    }

    func completeImportReview(_ resolutions: [ImportResolution]?, id: String? = nil) {
        if let id, importInspection?.id != id { return }
        let continuation = importReview
        importReview = nil
        importInspection = nil
        continuation?.resume(returning: resolutions)
    }

    func cancelImport() {
        if importReview != nil { completeImportReview(nil) }
        else if client.cancelTransfer() { importStatus = "正在取消" }
    }

    @Published private(set) var importProgress: ImportTransferProgress?

    private func pollImportConflicts(ticket: Int, exporting: Bool = false) async {
        while !Task.isCancelled, ticket == generation {
            do {
                let batch: ImportInspection? = try await client.json(NativeRequest(operation: exporting ? "poll_export_conflicts" : "poll_import_conflicts"))
                if let batch {
                    guard ticket == generation, !Task.isCancelled else { return }
                    self.conflictOperation = exporting ? "导出" : "导入"
                    let resolutions: [ImportResolution]? = await withTaskCancellationHandler {
                        await withCheckedContinuation { continuation in
                            importReview = continuation
                            importInspection = batch
                        }
                    } onCancel: { Task { @MainActor in self.completeImportReview(nil, id: batch.id) } }
                    guard let resolutions, ticket == generation, !Task.isCancelled else { _ = client.cancelTransfer(); return }
                    try await client.execute(NativeRequest(operation: exporting ? "resolve_export_conflicts" : "resolve_import_conflicts", id: batch.id, resolutions: resolutions))
                }
            } catch {
                if ticket == generation, !Task.isCancelled { self.error = error.localizedDescription; _ = client.cancelTransfer() }
                return
            }
            try? await Task.sleep(for: .milliseconds(150))
        }
    }

    private func importSources(_ urls: [URL], ticket: Int) async throws {
        importStatus = "正在导入文件"
        let conflicts = Task { @MainActor in await self.pollImportConflicts(ticket: ticket) }
        let progress = Task { @MainActor in
            while !Task.isCancelled {
                let snapshot = self.client.transferProgress()
                self.importProgress = snapshot?.operation == "import" ? snapshot : nil
                try? await Task.sleep(for: .milliseconds(150))
            }
        }
        defer { conflicts.cancel(); progress.cancel(); completeImportReview(nil); importStatus = nil; importProgress = nil }
        do {
            let result: ContainerSnapshot = try await client.json(NativeRequest(operation: "stream_import_files", sources: urls.map(\.path), directory: mode == .path ? directory : ""))
            if ticket == generation {
                container = result
                if let progress = client.transferProgress() { notice = "已完成 \(progress.completed_files) 个文件，跳过 \(progress.skipped_files) 个文件" }
            }
        } catch {
            if ticket == generation,
               let refreshed: ContainerSnapshot = try? await client.json(NativeRequest(operation: "refresh")), ticket == generation { container = refreshed }
            if ticket == generation, let progress = client.transferProgress(), progress.phase == "cancelled" {
                notice = "导入已中止，已完成 \(progress.completed_files) 个文件（其中跳过 \(progress.skipped_files) 个）"
                return
            }
            throw error
        }
    }

    func importFiles(_ urls: [URL]) async { await run { ticket in
        let accessed = urls.filter { $0.startAccessingSecurityScopedResource() }
        defer { accessed.forEach { $0.stopAccessingSecurityScopedResource() } }
        try await self.importSources(urls, ticket: ticket)
    } }
    /// 相册选择器返回的临时文件在回调结束后可能失效，先复制到受保护的临时目录。
    func importPhotos(_ items: [PhotosPickerItem]) async { await run { ticket in
        var staged: [PhotoImportFile] = []
        defer { staged.forEach { try? FileManager.default.removeItem(at: $0.url.deletingLastPathComponent()) } }
        for item in items {
            guard ticket == self.generation else { return }
            guard let file = try await item.loadTransferable(type: PhotoImportFile.self) else {
                throw ClientError.message("无法读取所选的照片或视频")
            }
            staged.append(file)
        }
        guard ticket == self.generation else { return }
        try await self.importSources(staged.map(\.url), ticket: ticket)
    } }
    func thumbnailKey(_ file: FileEntry) -> String { "\(file.id):\(file.version)" }
    func loadThumbnail(_ file: FileEntry) async {
        let ext = URL(fileURLWithPath: file.leafName).pathExtension.lowercased()
        let textType = UTType(filenameExtension: ext)?.conforms(to: .plainText) == true || ["txt", "md", "csv", "log", "json", "xml", "yaml", "yml"].contains(ext)
        let isText = file.category != .photo && file.category != .video && textType && file.size <= 512 * 1024
        guard (file.category == .photo || file.category == .video || isText), container != nil else { return }
        let key = thumbnailKey(file)
        guard thumbnails[key] == nil, textThumbnails[key] == nil, !thumbnailTasks.contains(key), !thumbnailFailures.contains(key) else { return }
        let ticket = generation
        thumbnailTasks.insert(key)
        defer { if ticket == generation { thumbnailTasks.remove(key) } }
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("veil-preview-" + UUID().uuidString, isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700, .protectionKey: FileProtectionType.complete])
            defer { try? FileManager.default.removeItem(at: folder) }
            let url = folder.appendingPathComponent(file.leafName)
            try await client.execute(NativeRequest(operation: "export_file", name: file.name, destination: url.path))
            guard ticket == generation, !Task.isCancelled else { return }
            if isText {
                let data = try Data(contentsOf: url)
                guard ticket == generation, !Task.isCancelled else { return }
                if let text = Self.decodeText(data) {
                    if textThumbnails.count >= 120, let oldest = textThumbnails.keys.first { textThumbnails.removeValue(forKey: oldest) }
                    textThumbnails[key] = String(text.prefix(300))
                } else { thumbnailFailures.insert(key) }
                return
            }
            let image: UIImage?
            if file.category == .photo {
                let options: [CFString: Any] = [kCGImageSourceCreateThumbnailFromImageAlways: true, kCGImageSourceCreateThumbnailWithTransform: true, kCGImageSourceThumbnailMaxPixelSize: 360]
                if let source = CGImageSourceCreateWithURL(url as CFURL, nil), let cgImage = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) { image = UIImage(cgImage: cgImage) }
                else { image = nil }
            } else {
                let generator = AVAssetImageGenerator(asset: AVURLAsset(url: url))
                generator.appliesPreferredTrackTransform = true
                generator.maximumSize = CGSize(width: 360, height: 360)
                if let result = try? await generator.image(at: .zero) { image = UIImage(cgImage: result.image) }
                else { image = nil }
            }
            guard ticket == generation, !Task.isCancelled else { return }
            if let image {
                if thumbnails.count >= 120, let oldest = thumbnails.keys.first { thumbnails.removeValue(forKey: oldest) }
                thumbnails[key] = image
            } else { thumbnailFailures.insert(key) }
        } catch {
            if ticket == generation, !Task.isCancelled { thumbnailFailures.insert(key) }
        }
    }
    func remove(_ item: BrowserEntry) async { await run { ticket in
        let result: ContainerSnapshot = try await self.client.json(NativeRequest(operation: "remove_path", name: item.path))
        if ticket == self.generation { self.container = result }
    } }
    private func streamExport(_ names: [String], to destination: URL, ticket: Int) async throws {
        importStatus = "正在导出文件"
        let conflicts = Task { @MainActor in await self.pollImportConflicts(ticket: ticket, exporting: true) }
        let progress = Task { @MainActor in
            while !Task.isCancelled {
                let snapshot = self.client.transferProgress()
                self.importProgress = snapshot?.operation == "export" ? snapshot : nil
                try? await Task.sleep(for: .milliseconds(150))
            }
        }
        defer { conflicts.cancel(); progress.cancel(); completeImportReview(nil); importStatus = nil; importProgress = nil }
        try await client.execute(NativeRequest(operation: "stream_export_files", names: names, destination: destination.path))
    }

    func exportToDirectory(_ entry: BrowserEntry, destination: URL) async { await run { ticket in
        let accessed = destination.startAccessingSecurityScopedResource()
        defer { if accessed { destination.stopAccessingSecurityScopedResource() } }
        let target: URL
        if case .file(let file) = entry { target = destination.appendingPathComponent(file.leafName) }
        else { target = destination }
        do { try await self.streamExport([entry.path], to: target, ticket: ticket) }
        catch {
            if ticket == self.generation, self.client.transferProgress()?.phase == "cancelled" {
                self.notice = "导出已中止，已导出的文件已保留"
                return
            }
            throw error
        }
        if ticket == self.generation, let progress = self.client.transferProgress() {
            self.notice = "导出完成 \(progress.completed_files - progress.skipped_files) 个文件，跳过 \(progress.skipped_files) 个文件"
        }
    } }

    func prepareExport(_ file: FileEntry? = nil) async { await run { ticket in
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("veil-export-" + UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700, .protectionKey: FileProtectionType.complete])
        defer { try? FileManager.default.removeItem(at: folder) }
        let filename = file?.leafName ?? ((self.container?.name ?? "Veil").replacingOccurrences(of: "/", with: "_") + ".veil")
        let destination = folder.appendingPathComponent(filename)
        if let file { try await self.streamExport([file.name], to: destination, ticket: ticket) }
        else { try await self.client.execute(NativeRequest(operation: "export_package", destination: destination.path)) }
        let data = try Data(contentsOf: destination)
        if ticket == self.generation { self.export = ExportItem(name: filename, document: ExportDocument(data: data)) }
    } }
    func show(_ file: FileEntry) async { await run { ticket in
        let ext = URL(fileURLWithPath: file.leafName).pathExtension
        let type = UTType(filenameExtension: ext)
        let isImage = file.category == .photo || type?.conforms(to: .image) == true
        let isVideo = file.category == .video || type?.conforms(to: .movie) == true
        if !isImage && !isVideo && file.size > 8 * 1024 * 1024 {
            throw ClientError.message("文本文件超过 8 MB，请导出后查看。")
        }
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("veil-preview-" + UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700, .protectionKey: FileProtectionType.complete])
        var retained = false
        defer { if !retained { try? FileManager.default.removeItem(at: folder) } }
        let url = folder.appendingPathComponent(file.leafName)
        try await self.client.execute(NativeRequest(operation: "export_file", name: file.name, destination: url.path))
        guard ticket == self.generation else { return }
        if isImage || isVideo {
            self.preview = PreviewItem(file: file, content: isVideo ? .video(url) : .image(url), temporaryFolder: folder)
            retained = true
        } else {
            let data = try Data(contentsOf: url)
            guard let text = Self.decodeText(data) else { throw ClientError.message("此文件不是支持的文本格式，请导出后使用其他应用打开。") }
            self.preview = PreviewItem(file: file, content: .text(text), temporaryFolder: nil)
        }
    } }
    private static func decodeText(_ data: Data) -> String? {
        let text: String?
        if data.starts(with: [0xff, 0xfe]) || data.starts(with: [0xfe, 0xff]) {
            text = String(data: data, encoding: .utf16)
        } else {
            text = String(data: data, encoding: .utf8)
        }
        guard let text, !text.unicodeScalars.contains(where: { $0.value < 32 && ![9, 10, 13].contains($0.value) }) else { return nil }
        return text
    }
    private func removeStalePreviews() {
        let manager = FileManager.default
        guard let items = try? manager.contentsOfDirectory(at: manager.temporaryDirectory, includingPropertiesForKeys: nil) else { return }
        for item in items where item.lastPathComponent.hasPrefix("veil-preview-") { try? manager.removeItem(at: item) }
    }
}
struct PreviewItem: Identifiable {
    enum Content { case image(URL), video(URL), text(String) }
    let id = UUID()
    let file: FileEntry
    let content: Content
    let temporaryFolder: URL?
    func removeTemporaryFiles() { if let temporaryFolder { try? FileManager.default.removeItem(at: temporaryFolder) } }
}

struct ExportItem: Identifiable { let id = UUID(); let name: String; let document: ExportDocument }
struct ExportDocument: FileDocument {
    static var readableContentTypes: [UTType] { [.data] }
    let data: Data
    init(data: Data) { self.data = data }
    init(configuration: ReadConfiguration) throws { data = configuration.file.regularFileContents ?? Data() }
    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper { FileWrapper(regularFileWithContents: data) }
}

private struct PhotoImportFile: Transferable {
    let url: URL
    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(importedContentType: .image) { try copy($0) }
        FileRepresentation(importedContentType: .movie) { try copy($0) }
    }
    private static func copy(_ received: ReceivedTransferredFile) throws -> PhotoImportFile {
        let manager = FileManager.default
        let folder = manager.temporaryDirectory.appendingPathComponent("veil-photo-import-" + UUID().uuidString, isDirectory: true)
        try manager.createDirectory(at: folder, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700, .protectionKey: FileProtectionType.complete])
        do {
            let destination = folder.appendingPathComponent(received.file.lastPathComponent)
            try manager.copyItem(at: received.file, to: destination)
            return PhotoImportFile(url: destination)
        } catch {
            try? manager.removeItem(at: folder)
            throw error
        }
    }
}
