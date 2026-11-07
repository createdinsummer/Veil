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
    @Published private(set) var transferProgress: TransferProgress?
    @Published private(set) var transferTitle: String?
    @Published private(set) var lastTransferError: String?
    @Published private(set) var lastTransferFailure: TransferDiagnosticRecord?
    @Published var selectedSpaceID = ""
    @Published var directory = ""
    @Published var categories: Set<FileCategory> = []
    @Published var search = ""
    @Published var layout: FileLayout = .grid
    @Published var browseMode: BrowseMode = .type
    @Published var form: ContainerForm?
    @Published var showNewSpace = false
    @Published var showImportPackage = false
    @Published var pendingDelete: BrowserEntry?
    @Published var pendingContainerDelete: ContainerSummary?
    @Published var pendingSpaceDelete: Space?
    @Published var preview: PreviewAsset?
    @Published var conflictOperation = "导入"
    @Published var importInspection: ImportInspection?
    private var importReview: CheckedContinuation<[ImportResolution]?, Never>?
    @Published var error: String?
    @Published var notice: String?
    @Published private(set) var thumbnails: [String: Data] = [:]
    /// 解锁成功后密码在本机内存中的保留时长，来自 app.db。
    @Published private(set) var unlockCacheSeconds = 3600
    @Published private(set) var importCommitIntervalSeconds = 1
    @Published private(set) var importCommitCount = 8

    private let client = ServiceClient()
    private var generation: UInt64 = 0
    private var loaded = false
    private var thumbnailBatch = UUID()
    private var thumbnailVersions: [String: Int] = [:]
    /// 只存在进程内存中，随退出消失；不写入 app.db 或磁盘。
    private var unlockedPasswords: [String: UnlockedPassword] = [:]

    private struct UnlockedPassword {
        let secret: String
        let expiresAt: Date
    }

    var unlockCacheDescription: String { Self.durationLabel(unlockCacheSeconds) }

    static func durationLabel(_ seconds: Int) -> String {
        switch seconds {
        case ...0: "关闭"
        case ..<3600: "\(max(1, seconds / 60)) 分钟"
        default: "\(seconds / 3600) 小时"
        }
    }

    private func rememberUnlock(_ password: String, id: String) {
        guard unlockCacheSeconds > 0 else { unlockedPasswords.removeValue(forKey: id); return }
        unlockedPasswords[id] = UnlockedPassword(
            secret: password,
            expiresAt: Date().addingTimeInterval(TimeInterval(unlockCacheSeconds))
        )
    }

    private func cachedUnlock(for id: String) -> String? {
        guard let entry = unlockedPasswords[id] else { return nil }
        guard entry.expiresAt > Date() else { unlockedPasswords.removeValue(forKey: id); return nil }
        return entry.secret
    }

    var isBusy: Bool { pendingOperations > 0 }
    var canBrowse: Bool { !isBusy || transferProgress?.operation == "import" }
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
            } catch { AppLog.capture(error); /* 不可解码的图片使用类型图标，不影响文件操作。 */ }
        }
    }

    func load() async {
        guard !loaded else { return }
        lastTransferFailure = TransferDiagnostics.latestRecord()
        await perform { _ in
            try await self.reloadCatalog()
            let layout: String = try await self.client.json(NativeRequest(operation: "get_preference", key: "layout"))
            let mode: String = try await self.client.json(NativeRequest(operation: "get_preference", key: "view_mode"))
            let cache: Double = try await self.client.json(NativeRequest(operation: "get_preference", key: "unlock_cache_seconds"))
            let interval: Int = try await self.client.json(NativeRequest(operation: "get_preference", key: "import_commit_interval_seconds"))
            let count: Int = try await self.client.json(NativeRequest(operation: "get_preference", key: "import_commit_count"))
            self.layout = FileLayout(rawValue: layout) ?? .grid
            self.browseMode = BrowseMode(rawValue: mode) ?? .type
            self.unlockCacheSeconds = Int(cache.rounded())
            self.importCommitIntervalSeconds = interval
            self.importCommitCount = count
            self.loaded = true
        }
    }

    private func perform(allowDuringImport: Bool = false, _ work: (UInt64) async throws -> Void) async {
        guard !isBusy || (allowDuringImport && canBrowse) else { return }
        pendingOperations += 1
        error = nil
        let ticket = generation
        defer { pendingOperations -= 1 }
        do { try await work(ticket) }
        catch { AppLog.capture(error); if ticket == generation { self.error = error.localizedDescription } }
    }

    private func withTransferProgress<T>(_ title: String, operation: String, work: () async throws -> T) async throws -> T {
        transferTitle = title
        transferProgress = nil
        lastTransferError = nil
        lastTransferFailure = nil
        let poll = Task { @MainActor in
            var lastRefreshedCompleted: UInt64 = 0
            var lastContainerRefresh = Date.distantPast
            while !Task.isCancelled {
                if let progress = client.transferProgress(), progress.operation == operation,
                   ["preparing", "processing", "verifying", "snapshot", "packing", "unpacking", "finalizing"].contains(progress.phase) {
                    transferProgress = progress
                    let now = Date()
                    let enoughProgress = progress.completedFiles > lastRefreshedCompleted
                        && progress.completedFiles - lastRefreshedCompleted >= 512
                    let maximumDelayReached = now.timeIntervalSince(lastContainerRefresh) >= 10
                    let minimumDelayElapsed = now.timeIntervalSince(lastContainerRefresh) >= 2
                    if operation == "import", progress.completedFiles > lastRefreshedCompleted,
                       minimumDelayElapsed, (enoughProgress || maximumDelayReached) {
                        lastContainerRefresh = now
                        let ticket = generation
                        if let view: ContainerSnapshot = try? await client.json(NativeRequest(operation: "refresh")), ticket == generation {
                            container = view
                            lastRefreshedCompleted = progress.completedFiles
                        }
                    }
                }
                try? await Task.sleep(for: .milliseconds(250))
            }
        }
        defer { poll.cancel(); transferTitle = nil; transferProgress = nil }
        do { return try await work() }
        catch {
            lastTransferError = error.localizedDescription
            let failureProgress = client.transferProgress() ?? transferProgress
            if failureProgress?.cancelRequested != true,
               !error.localizedDescription.localizedCaseInsensitiveContains("已中止") {
                let record = TransferDiagnostics.makeRecord(error: error, operation: operation, progress: failureProgress)
                lastTransferFailure = record
                TransferDiagnostics.append(record)
            }
            throw AppLog.recorded(error)
        }
    }

    var transferDiagnosticFilename: String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = TimeZone.current
        formatter.dateFormat = "yyyyMMdd-HHmmss"
        return "Veil-诊断日志-\(formatter.string(from: Date())).json"
    }

    func exportErrorLogs(to url: URL) {
        do {
            try AppLog.export().write(to: url, options: .atomic)
            notice = "错误日志已导出，可附在反馈中。"
        } catch {
            AppLog.capture(error)
            self.error = "无法导出错误日志：\(error.localizedDescription)"
        }
    }

    func exportTransferDiagnostic(to url: URL) {
        guard let lastTransferFailure else { return }
        do {
            try TransferDiagnostics.export(lastTransferFailure, to: url)
            notice = "诊断日志已导出，可附在反馈中。"
        } catch { AppLog.capture(error);
            self.error = "无法导出诊断日志：\(error.localizedDescription)"
        }
    }

    func cancelTransfer() {
        guard client.cancelTransfer() else { return }
        transferProgress = client.transferProgress()
    }

    private func reloadCatalog() async throws {
        spaces = try await client.json(NativeRequest(operation: "list_spaces"))
        recents = try await client.json(NativeRequest(operation: "list_recents"))
        if !spaces.contains(where: { $0.id == selectedSpaceID }) { selectedSpaceID = activeSpace?.id ?? "" }
    }

    func refreshSpaceAvailability() async {
        guard !isBusy else { return }
        let wasRegisteredContainer = container.map { opened in
            spaces.contains(where: { $0.containers.contains(where: { $0.id == opened.id }) })
        } ?? false
        do {
            try await reloadCatalog()
            if wasRegisteredContainer, let container,
               !spaces.contains(where: { $0.containers.contains(where: { $0.id == container.id }) }) {
                _ = await lock()
            }
        } catch { AppLog.capture(error); return }
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
    /// 自动锁定（休眠、锁屏、退出）保留内存中的免密密码，主动锁定则清除。
    @discardableResult
    func lock(forgetPassword: Bool = false) async -> Bool {
        generation &+= 1
        _ = client.cancelTransfer()
        completeImportReview(nil)
        container = nil
        form = nil
        resetBrowser()
        notice = nil
        if forgetPassword { unlockedPasswords.removeAll() }
        pendingOperations += 1
        defer { pendingOperations -= 1 }
        do {
            try await client.execute(NativeRequest(operation: "lock"))
            return true
        } catch { AppLog.capture(error);
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
        // 同一容器在免密时长内直接用内存中的密码重新打开，不再弹密码框。
        if let password = cachedUnlock(for: item.id) {
            pendingOperations += 1
            defer { pendingOperations -= 1 }
            do {
                try await client.execute(NativeRequest(operation: "lock"))
                resetBrowser()
                let view: ContainerSnapshot = try await client.json(
                    NativeRequest(operation: "open_container", password: password, path: item.path)
                )
                container = view
                form = nil
                do {
                    try await reloadCatalog()
                    if let space = spaces.first(where: { $0.containers.contains(where: { $0.id == view.id }) }) {
                        selectedSpaceID = space.id
                    }
                } catch { AppLog.capture(error);
                    notice = "容器已打开，空间列表刷新失败：\(error.localizedDescription)"
                }
                return
            } catch { AppLog.capture(error);
                // 密码已失效（例如容器被重建），清除缓存后回到输入密码流程。
                unlockedPasswords.removeValue(forKey: item.id)
            }
        }
        form = .open(path: item.path, name: item.name)
    }

    func newContainer(in spaceID: String? = nil) async {
        guard !isBusy, let space = spaceID ?? activeSpace?.id,
              spaces.contains(where: { $0.id == space && $0.isAvailable }) else { return }
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
        guard !isBusy, activeSpace?.isAvailable == true else { return }
        error = nil
        showImportPackage = true
    }

    func submit(_ form: ContainerForm, name: String, password: String, confirmation: String) async {
        let spaceID: String?
        switch form {
        case .create(let space): spaceID = space
        case .importPackage(_, let space): spaceID = space
        case .open: spaceID = nil
        }
        if let spaceID, !spaces.contains(where: { $0.id == spaceID && $0.isAvailable }) {
            error = AppLog.validation("空间当前不可用，请重新挂载外置磁盘后再试")
            return
        }
        guard !password.isEmpty else { error = AppLog.validation("请输入容器密码"); return }
        if form.isCreate && password != confirmation { error = AppLog.validation("两次输入的密码不一致"); return }
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        if form.isCreate && name.isEmpty { error = AppLog.validation("请输入容器名称"); return }
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
            let view: ContainerSnapshot
            if case .importPackage = form {
                view = try await self.withTransferProgress("正在导入容器", operation: "import_package") {
                    try await self.client.json(request)
                }
            } else {
                view = try await self.client.json(request)
            }
            guard ticket == self.generation else {
                // actor 调度不保证先进先出；即使锁定先于迟到的解锁执行，也须再次清理会话。
                try await self.client.execute(NativeRequest(operation: "lock"))
                return
            }
            self.container = view
            self.form = nil
            self.rememberUnlock(password, id: view.id)
            do {
                try await self.reloadCatalog()
                if let space = self.spaces.first(where: { $0.containers.contains(where: { $0.id == view.id }) }) { self.selectedSpaceID = space.id }
            } catch { AppLog.capture(error); self.notice = "容器已打开，空间列表刷新失败：\(error.localizedDescription)" }
        }
    }

    func createSpace(name: String, directory: URL? = nil) async {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let scoped = directory?.startAccessingSecurityScopedResource() ?? false
        defer { if scoped { directory?.stopAccessingSecurityScopedResource() } }
        await perform { ticket in
            let spaces: [Space] = try await self.client.json(NativeRequest(operation: "create_space", name: name, path: directory?.path))
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

    @discardableResult
    func deleteContainer(_ item: ContainerSummary) async -> Bool {
        var deleted = false
        await perform { ticket in
            if self.container?.id == item.id {
                self.container = nil
                self.form = nil
                self.resetBrowser()
                try await self.client.execute(NativeRequest(operation: "lock"))
                guard ticket == self.generation else { return }
            }
            let spaces: [Space] = try await self.client.json(
                NativeRequest(operation: "delete_container", id: item.id)
            )
            guard ticket == self.generation else { return }
            self.unlockedPasswords.removeValue(forKey: item.id)
            self.spaces = spaces
            self.recents.removeAll { $0.id == item.id }
            self.recents = try await self.client.json(NativeRequest(operation: "list_recents"))
            if !spaces.contains(where: { $0.id == self.selectedSpaceID }) {
                self.selectedSpaceID = spaces.first(where: \.isDefault)?.id ?? spaces.first?.id ?? ""
            }
            self.notice = "容器已删除"
            deleted = true
        }
        return deleted
    }

    func deleteSpace(_ space: Space) async {
        await perform { ticket in
            let openContainer = self.container
            let belongsToSpace = space.containers.contains(where: { $0.id == openContainer?.id })
                || openContainer.map { URL(fileURLWithPath: $0.path).deletingLastPathComponent().path == space.path } == true
            if belongsToSpace {
                self.container = nil
                self.form = nil
                self.resetBrowser()
            }
            let spaces: [Space] = try await self.client.json(
                NativeRequest(operation: space.isAvailable ? "delete_space" : "delete_space_record", id: space.id)
            )
            guard ticket == self.generation else { return }
            for item in space.containers { self.unlockedPasswords.removeValue(forKey: item.id) }
            self.spaces = spaces
            let deletedContainerIDs = Set(space.containers.map(\.id))
            self.recents.removeAll { deletedContainerIDs.contains($0.id) }
            self.recents = try await self.client.json(NativeRequest(operation: "list_recents"))
            if !spaces.contains(where: { $0.id == self.selectedSpaceID }) {
                self.selectedSpaceID = spaces.first(where: \.isDefault)?.id ?? spaces.first?.id ?? ""
            }
            self.notice = space.isAvailable
                ? (space.isDefault ? "默认空间已删除并重新建立" : "空间已删除")
                : "空间记录已删除，磁盘文件仍保留"
        }
    }

    func refresh() async {
        await perform(allowDuringImport: true) { ticket in
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
        } catch { AppLog.capture(error); self.error = error.localizedDescription }
    }

    /// 免密时长只写入 app.db 的设置项；密码本身仍只在内存中。
    func setUnlockCache(seconds: Int) async {
        await perform { _ in
            try await self.client.execute(NativeRequest(operation: "set_unlock_cache", seconds: seconds))
            self.unlockCacheSeconds = seconds
            if seconds == 0 { self.unlockedPasswords.removeAll() }
            self.notice = seconds == 0 ? "已关闭免密重开" : "免密时长已设为 \(self.unlockCacheDescription)"
        }
    }

    func setImportCommitInterval(seconds: Int) async {
        await perform { _ in
            try await self.client.execute(NativeRequest(operation: "set_import_commit_interval", seconds: seconds))
            self.importCommitIntervalSeconds = seconds
            self.notice = "导入提交间隔已设为 \(seconds) 秒"
        }
    }

    func setImportCommitCount(_ count: Int) async {
        await perform { _ in
            try await self.client.execute(NativeRequest(operation: "set_import_commit_count", count: count))
            self.importCommitCount = count
            self.notice = "导入提交数量已设为 \(count) 条"
        }
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

    var importProgressDescription: String? {
        guard let progress = transferProgress, progress.totalFiles > 0 else { return nil }
        return "已完成 \(progress.completedFiles) / \(progress.totalFiles) · \(Int(min(1, Double(progress.completedFiles) / Double(progress.totalFiles))*100))%" + (progress.scanComplete ? " · 遍历完成" : " · 总数持续增加")
    }

    func completeImportReview(_ resolutions: [ImportResolution]?, id: String? = nil) {
        if let id, importInspection?.id != id { return }
        let continuation = importReview
        importReview = nil
        importInspection = nil
        continuation?.resume(returning: resolutions)
    }

    private func reviewImport(_ inspection: ImportInspection) async -> [ImportResolution]? {
        if inspection.conflicts.isEmpty { return [] }
        return await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                importReview = continuation
                importInspection = inspection
            }
        } onCancel: {
            Task { @MainActor in self.completeImportReview(nil, id: inspection.id) }
        }
    }

    private func pollImportConflicts(ticket: UInt64, exporting: Bool = false) async {
        while !Task.isCancelled, ticket == generation {
            do {
                let batch: ImportInspection? = try await client.json(NativeRequest(operation: exporting ? "poll_export_conflicts" : "poll_import_conflicts"))
                if let batch {
                    guard ticket == generation, !Task.isCancelled else { return }
                    self.conflictOperation = exporting ? "导出" : "导入"
                    guard let resolutions = await reviewImport(batch), ticket == generation, !Task.isCancelled else {
                        _ = client.cancelTransfer()
                        return
                    }
                    try await client.execute(NativeRequest(operation: exporting ? "resolve_export_conflicts" : "resolve_import_conflicts", id: batch.id, resolutions: resolutions))
                }
            } catch { AppLog.capture(error);
                if ticket == generation, !Task.isCancelled {
                    self.error = error.localizedDescription
                    _ = client.cancelTransfer()
                }
                return
            }
            try? await Task.sleep(for: .milliseconds(150))
        }
    }

    func importFiles(_ urls: [URL]) async {
        guard !urls.isEmpty, container != nil else { return }
        let destination = browseMode == .path ? directory : ""
        let scoped = urls.filter { $0.startAccessingSecurityScopedResource() }
        defer { scoped.forEach { $0.stopAccessingSecurityScopedResource() } }
        await perform { ticket in
            let conflicts = Task { @MainActor in await self.pollImportConflicts(ticket: ticket) }
            defer { conflicts.cancel(); self.completeImportReview(nil) }
            try await self.withTransferProgress("正在导入文件", operation: "import") {
                do {
                    let view: ContainerSnapshot = try await self.client.json(NativeRequest(operation: "stream_import_files", sources: urls.map(\.path), directory: destination))
                    guard ticket == self.generation else { return }
                    self.container = view
                    if let progress = self.client.transferProgress() {
                        self.notice = "已完成 \(progress.completedFiles) 个文件，跳过 \(progress.skippedFiles) 个文件"
                    }
                } catch {
                    let progress = self.client.transferProgress()
                    if ticket == self.generation,
                       let refreshed: ContainerSnapshot = try? await self.client.json(NativeRequest(operation: "refresh")), ticket == self.generation { self.container = refreshed }
                    if ticket == self.generation, let progress, progress.phase == "cancelled" {
                        self.notice = "导入已中止，已完成 \(progress.completedFiles) 个文件（其中跳过 \(progress.skippedFiles) 个）"
                        return
                    }
                    throw AppLog.recorded(error)
                }
            }
        }
    }

    func activate(_ entry: BrowserEntry) async {
        switch entry {
        case .folder(let path): directory = path
        case .file(let file): await showPreview(file)
        }
    }

    func showPreview(_ file: FileEntry) async {
        guard canBrowse else { return }
        closePreview()
        let asset = PreviewAsset(file: file)
        preview = asset
        guard file.previewMime != nil else { asset.error = AppLog.validation("此类型暂不支持内置预览，可导出后查看。"); return }
        await perform(allowDuringImport: true) { ticket in
            do {
                let result = try await self.client.previewWithStatus(name: file.name)
                guard ticket == self.generation, self.preview === asset else { return }
                asset.unstable = result.unstable
                try asset.install(result.data)
            } catch { AppLog.capture(error);
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
        await exportPaths([file.name], to: url)
    }

    func exportFolder(_ path: String) async {
        guard !isBusy else { return }
        let panel = NSOpenPanel()
        panel.title = "选择导出位置"
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        guard panel.runModal() == .OK, let url = panel.url else { return }
        await exportPaths([path], to: url)
    }

    func exportPaths(_ names: [String], to url: URL) async {
        await perform { ticket in
            let accessed = url.startAccessingSecurityScopedResource()
            defer { if accessed { url.stopAccessingSecurityScopedResource() } }
            let conflicts = Task { @MainActor in await self.pollImportConflicts(ticket: ticket, exporting: true) }
            defer { conflicts.cancel(); self.completeImportReview(nil) }
            do {
                try await self.withTransferProgress("正在导出文件", operation: "export") {
                    try await self.client.execute(NativeRequest(operation: "stream_export_files", names: names, destination: url.path))
                }
            } catch {
                if ticket == self.generation, self.client.transferProgress()?.phase == "cancelled" {
                    self.notice = "导出已中止，已导出的文件已保留"
                    return
                }
                throw AppLog.recorded(error)
            }
            if ticket == self.generation, let progress = self.client.transferProgress() {
                self.notice = "导出完成 \(progress.completedFiles - progress.skippedFiles) 个文件，跳过 \(progress.skippedFiles) 个文件"
            }
        }
    }

    var dragExportGeneration: UInt64 { generation }

    func exportDraggedFile(_ file: FileEntry, to url: URL, generation ticket: UInt64) async throws {
        guard ticket == generation, container != nil, !isBusy else {
            throw ClientError.service("保险箱已关闭，请重新拖动文件。")
        }
        pendingOperations += 1
        defer { pendingOperations -= 1 }
        try await withTransferProgress("正在导出文件", operation: "export") {
            let unstable = try await client.exportWithStatus(name: file.name, destination: url.path)
            if unstable { notice = "文件已导出；内容正在写入，结果可能发生变化" }
        }
    }

    func exportPackage() async {
        guard !isBusy, let container else { return }
        let panel = NSSavePanel()
        panel.title = "导出加密备份"
        panel.allowedContentTypes = [UTType(filenameExtension: "veil") ?? .data]
        let name = container.name.replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: ":", with: "_")
        panel.nameFieldStringValue = name.lowercased().hasSuffix(".veil") ? name : name + ".veil"
        panel.canCreateDirectories = true
        guard panel.runModal() == .OK, let url = panel.url else { return }
        await perform { ticket in
            try await self.withTransferProgress("正在导出加密备份", operation: "export_package") {
                try await self.client.execute(NativeRequest(operation: "export_package", destination: url.path))
                if ticket == self.generation { self.notice = "加密备份已导出" }
            }
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
    @Published var unstable = false
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
            } catch { close(); throw AppLog.recorded(error) }
        } else { data = bytes }
    }

    func close() {
        data = nil
        videoURL = nil
        if let temporaryDirectory { AppLog.attempt({ try FileManager.default.removeItem(at: temporaryDirectory) }) }
        temporaryDirectory = nil
    }

    deinit { if let temporaryDirectory { AppLog.attempt({ try FileManager.default.removeItem(at: temporaryDirectory) }) } }
}
