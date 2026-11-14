import AppKit
import Foundation
import SwiftUI
import UniformTypeIdentifiers

/// 解密浮层状态：先把明文写进临时文件，再交给应用内查看器或系统应用。
struct DecryptStatus: Equatable, Sendable {
    let fileName: String
    /// 解密完成后的去处，例如“应用内查看器”“「OmniPlayer」”“系统默认应用”。
    let destination: String
}

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
    /// 默认隐藏以「.」开头的文件与目录；切换开关仅对当前浏览会话生效。
    @Published var showHiddenFiles = false
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
    /// 命令行提示详细程度；与 CLI 共用 app.db 的 hint_level 设置项。
    @Published var hintLevel: HintLevel = .full
    /// 本机设备身份；只读展示，来自 app.db 的 system_setting 表。
    @Published private(set) var deviceID = ""
    /// 设置窗口内的操作反馈；在主窗口 notice 之外独立提示。
    @Published var settingsFeedback: String?
    /// 按扩展名登记的默认打开应用；来自 app.db 的 user_setting 表。
    @Published private(set) var openWithDefaults: [String: String] = [:]
    /// 非空时界面显示“正在解密文件…”浮层。
    @Published private(set) var decryptStatus: DecryptStatus?

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
        return browserEntries(container: container, mode: browseMode, directory: directory, categories: categories, search: search, showHidden: showHiddenFiles)
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
        // 启动即清空上次遗留的明文副本（崩溃、旧版本或外部应用未退出都可能留下）。
        TemporaryFiles.purge(
            root: TemporaryFiles.directory,
            legacyRoot: FileManager.default.temporaryDirectory)
        lastTransferFailure = TransferDiagnostics.latestRecord()
        await perform { _ in
            try await self.reloadCatalog()
            let layout: String = try await self.client.json(NativeRequest(operation: "get_preference", key: "layout"))
            let mode: String = try await self.client.json(NativeRequest(operation: "get_preference", key: "view_mode"))
            let cache: Double = try await self.client.json(NativeRequest(operation: "get_preference", key: "unlock_cache_seconds"))
            let interval: Int = try await self.client.json(NativeRequest(operation: "get_preference", key: "import_commit_interval_seconds"))
            let count: Int = try await self.client.json(NativeRequest(operation: "get_preference", key: "import_commit_count"))
            let hint: String = try await self.client.json(NativeRequest(operation: "get_preference", key: "hint_level"))
            let device: String? = try await self.client.json(NativeRequest(operation: "get_system_setting", key: "device_id"))
            let openWith: [String: String] = (try? await self.client.json(
                NativeRequest(operation: "get_preference", key: "external_open_defaults"))) ?? [:]
            self.layout = FileLayout(rawValue: layout) ?? .grid
            self.browseMode = BrowseMode(rawValue: mode) ?? .type
            self.unlockCacheSeconds = Int(cache.rounded())
            self.importCommitIntervalSeconds = interval
            self.importCommitCount = count
            self.hintLevel = HintLevel(rawValue: hint) ?? .full
            self.deviceID = device ?? ""
            self.openWithDefaults = openWith
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
        showHiddenFiles = false
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
        cleanupExternalOpens()
        // 锁定/休眠/退出应用都走这里：清空整个临时副本目录，不留下任何明文。
        TemporaryFiles.purge(root: TemporaryFiles.directory)
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
            try await client.execute(NativeRequest(operation: "set_preference", key: "layout", value: .string(layout)))
            try await client.execute(NativeRequest(operation: "set_preference", key: "view_mode", value: .string(mode)))
            settingsFeedback = "布局与浏览方式已保存"
        } catch { AppLog.capture(error); self.error = error.localizedDescription }
    }

    /// 免密时长只写入 app.db 的设置项；密码本身仍只在内存中。
    /// 界面先即时生效，保存失败时回退原值；不经过 isBusy 门卫，保证设置必然落盘。
    func setUnlockCache(seconds: Int) async {
        let previous = unlockCacheSeconds
        unlockCacheSeconds = seconds
        do {
            try await client.execute(NativeRequest(operation: "set_unlock_cache", seconds: seconds))
            if seconds == 0 { unlockedPasswords.removeAll() }
            settingsFeedback = seconds == 0 ? "已关闭免密重开" : "免密时长已设为 \(unlockCacheDescription)"
        } catch {
            unlockCacheSeconds = previous
            AppLog.capture(error)
            self.error = error.localizedDescription
        }
    }

    func setImportCommitInterval(seconds: Int) async {
        let previous = importCommitIntervalSeconds
        importCommitIntervalSeconds = seconds
        do {
            try await client.execute(NativeRequest(operation: "set_import_commit_interval", seconds: seconds))
            settingsFeedback = "导入提交间隔已设为 \(seconds) 秒"
        } catch {
            importCommitIntervalSeconds = previous
            AppLog.capture(error)
            self.error = error.localizedDescription
        }
    }

    func setImportCommitCount(_ count: Int) async {
        let previous = importCommitCount
        importCommitCount = count
        do {
            try await client.execute(NativeRequest(operation: "set_import_commit_count", count: count))
            settingsFeedback = "导入提交数量已设为 \(count) 条"
        } catch {
            importCommitCount = previous
            AppLog.capture(error)
            self.error = error.localizedDescription
        }
    }

    /// 提示级别与 CLI 共用设置项；保存到 app.db 的 hint_level。
    func setHintLevel(_ level: HintLevel) async {
        let previous = hintLevel
        hintLevel = level
        do {
            try await client.execute(NativeRequest(operation: "set_preference", key: "hint_level", value: .string(level.rawValue)))
            settingsFeedback = "提示级别已设为「\(level.title)」"
        } catch {
            hintLevel = previous
            AppLog.capture(error)
            self.error = error.localizedDescription
        }
    }

    /// 切换默认空间；成功后刷新空间列表以同步「默认」标记。
    func setDefaultSpace(_ id: String) async {
        do {
            try await client.execute(NativeRequest(operation: "set_default_space", id: id))
            try await reloadCatalog()
            settingsFeedback = "默认空间已更新"
        } catch {
            AppLog.capture(error)
            self.error = error.localizedDescription
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

    func createFolder(named rawName: String) async {
        guard let name = folderNameForCreation(rawName) else {
            error = AppLog.validation("文件夹名称无效，请使用不含路径分隔符的名称")
            return
        }
        guard browseMode == .path, container != nil else { return }
        let parent = directory
        await perform { ticket in
            let view: ContainerSnapshot = try await self.client.json(
                NativeRequest(operation: "create_directory", name: name, directory: parent)
            )
            guard ticket == self.generation else { return }
            self.container = view
            self.notice = "文件夹已创建"
        }
    }

    func activate(_ entry: BrowserEntry) async {
        switch entry {
        case .folder(let path): directory = path
        case .file(let file):
            // 打开语义遵循用户登记：有默认应用就直接用系统应用打开，否则在应用内预览。
            let resolution = defaultApplicationResolution(for: file)
            if resolution.application != nil {
                await openExternally(file)
                return
            }
            if resolution.configured {
                notice = "未找到登记的应用，已在应用内预览"
            }
            await showPreview(file)
        }
    }

    /// 打开文件预览：先显示解密浮层，把明文写入应用临时目录；完成后自动关闭浮层，再打开预览窗口。
    /// 预览窗口按文件 URL 渲染，不受 16 MiB 内存读取限制；网格缩略图仍走内存读取。
    func showPreview(_ file: FileEntry) async {
        guard canBrowse else { return }
        closePreview()
        let asset = PreviewAsset(file: file)
        guard file.previewMime != nil else {
            preview = asset
            asset.error = AppLog.validation("此类型暂不支持内置预览，可导出后查看。")
            return
        }
        // 预览需要把整份明文写入临时目录：先确认空间够用，避免解密到一半才报 ENOSPC。
        if let message = PreviewStorage.insufficientSpaceMessage(
            fileSize: file.size,
            availableBytes: PreviewStorage.availableBytes(in: FileManager.default.temporaryDirectory)
        ) {
            preview = asset
            asset.error = AppLog.validation(message)
            return
        }
        await perform(allowDuringImport: true) { ticket in
            self.decryptStatus = DecryptStatus(fileName: file.leafName, destination: "应用内查看器")
            defer { self.decryptStatus = nil }
            let folder = TemporaryFiles.directory
                .appendingPathComponent("veil-native-preview-" + UUID().uuidString, isDirectory: true)
            do {
                try TemporaryFiles.ensureDirectory()
                try FileManager.default.createDirectory(
                    at: folder, withIntermediateDirectories: false,
                    attributes: [.posixPermissions: 0o700]
                )
                let url = folder.appendingPathComponent(file.leafName)
                let unstable = try await self.client.exportWithStatus(name: file.name, destination: url.path)
                guard ticket == self.generation else {
                    AppLog.attempt({ try FileManager.default.removeItem(at: folder) })
                    return
                }
                asset.unstable = unstable
                try asset.install(url, folder: folder)
                // 先关闭解密浮层，再打开预览窗口。
                self.decryptStatus = nil
                self.preview = asset
            } catch {
                AppLog.attempt({ try FileManager.default.removeItem(at: folder) })
                AppLog.capture(error)
                guard ticket == self.generation else { return }
                self.decryptStatus = nil
                asset.error = PreviewStorage.failureMessage(for: error, fileSize: file.size)
                self.preview = asset
            }
        }
    }

    func closePreview() { preview?.close(); preview = nil }

    // MARK: 用系统应用打开

    /// 设置页展示用：已登记的后缀（升序）。
    var openWithExtensions: [String] { OpenWithRules.sortedExtensions(openWithDefaults) }

    /// 文件当前配置的默认应用；未登记或应用已卸载时返回 nil（回落到系统默认）。
    func defaultOpenApplication(for file: FileEntry) -> ExternalApplication? {
        defaultApplicationResolution(for: file).application
    }

    func isApplicationInstalled(forExtension ext: String) -> Bool {
        guard let bundleID = openWithDefaults[ext] else { return false }
        return ExternalApplication.resolve(bundleID: bundleID) != nil
    }

    /// 用已登记的默认应用打开；未登记或应用缺失时使用系统默认应用。
    func openExternally(_ file: FileEntry) async {
        guard canBrowse else { return }
        let resolution = defaultApplicationResolution(for: file)
        if resolution.configured && resolution.application == nil {
            notice = "未找到登记的应用，已改用系统默认应用打开"
        }
        await exportAndOpen(file, application: resolution.application)
    }

    /// 选择应用打开文件：先确认是否登记为该后缀的默认应用，再解密并交给系统应用。
    func chooseApplication(for file: FileEntry) async {
        guard canBrowse else { return }
        guard
            let application = ExternalApplication.choose(
                title: "选择用于打开「\(file.leafName)」的应用",
                message: "会先把文件解密到临时文件，再交给该系统应用打开。")
        else { return }
        guard await confirmOpenWith(application, for: file) else { return }
        await exportAndOpen(file, application: application)
    }

    /// 预览弹窗：把当前已解密的副本交给系统应用；关闭预览不再删除它，改由锁定/退出清理。
    func openPreviewExternally() async {
        guard let asset = preview else { return }
        let resolution = defaultApplicationResolution(for: asset.file)
        if resolution.configured && resolution.application == nil {
            notice = "未找到登记的应用，已改用系统默认应用打开"
        }
        handOffPreview(asset, application: resolution.application)
    }

    func chooseApplicationForPreview() async {
        guard let asset = preview else { return }
        guard
            let application = ExternalApplication.choose(
                title: "选择用于打开「\(asset.file.leafName)」的应用",
                message: "会先把当前预览副本解密到临时文件，再交给该系统应用打开。")
        else { return }
        guard await confirmOpenWith(application, for: asset.file) else { return }
        handOffPreview(asset, application: application)
    }

    /// 设置页：新增「后缀 → 应用」登记。
    func addDefaultOpenApplication() async {
        guard let ext = promptOpenWithExtension() else { return }
        guard
            let application = ExternalApplication.choose(
                title: "选择 .\(ext) 文件默认使用的应用",
                message: "点击该后缀的文件时会先解密到临时文件，再交给所选应用打开；登记保存在 app.db，可在此修改或移除。")
        else { return }
        await setDefaultOpenApplication(application, forExtension: ext)
    }

    func changeDefaultOpenApplication(forExtension ext: String) async {
        guard
            let application = ExternalApplication.choose(
                title: "选择 .\(ext) 文件默认使用的应用",
                message: "点击该后缀的文件时会先解密到临时文件，再交给所选应用打开；登记保存在 app.db，可在此修改或移除。")
        else { return }
        await setDefaultOpenApplication(application, forExtension: ext)
    }

    func setDefaultOpenApplication(_ application: ExternalApplication, forExtension ext: String)
        async
    {
        guard let ext = OpenWithRules.normalizeUserInput(ext) else { return }
        await updateOpenWithDefaults { $0[ext] = application.bundleID }
        if openWithDefaults[ext] == application.bundleID {
            settingsFeedback = ".\(ext) 已改用「\(application.name)」打开"
        }
    }

    func removeDefaultOpenApplication(forExtension ext: String) async {
        await updateOpenWithDefaults { $0.removeValue(forKey: ext) }
        if openWithDefaults[ext] == nil {
            settingsFeedback = ".\(ext) 已恢复系统默认应用"
        }
    }

    private func updateOpenWithDefaults(_ change: (inout [String: String]) -> Void) async {
        let previous = openWithDefaults
        var rules = previous
        change(&rules)
        openWithDefaults = rules
        do {
            try await client.execute(
                NativeRequest(
                    operation: "set_preference", key: "external_open_defaults",
                    value: .map(rules)))
        } catch {
            openWithDefaults = previous
            AppLog.capture(error)
            self.error = error.localizedDescription
        }
    }

    private func defaultApplicationResolution(for file: FileEntry)
        -> (application: ExternalApplication?, configured: Bool)
    {
        guard let ext = OpenWithRules.normalizeExtension(forFileName: file.name),
            let bundleID = openWithDefaults[ext]
        else { return (nil, false) }
        return (ExternalApplication.resolve(bundleID: bundleID), true)
    }

    private func exportAndOpen(_ file: FileEntry, application: ExternalApplication?) async {
        // 外开要把整份明文写到临时目录，空间不够时先拦住，避免解密到一半失败。
        if let message = PreviewStorage.insufficientSpaceMessage(
            fileSize: file.size,
            availableBytes: PreviewStorage.availableBytes(in: FileManager.default.temporaryDirectory),
            action: "打开")
        {
            self.error = message
            return
        }
        await perform(allowDuringImport: true) { ticket in
            self.decryptStatus = DecryptStatus(
                fileName: file.leafName,
                destination: application.map { "「\($0.name)」" } ?? "系统默认应用")
            defer { self.decryptStatus = nil }
            let folder = TemporaryFiles.directory
                .appendingPathComponent(
                    TemporaryFiles.openPrefix + UUID().uuidString, isDirectory: true)
            do {
                try TemporaryFiles.ensureDirectory()
                try FileManager.default.createDirectory(
                    at: folder, withIntermediateDirectories: false,
                    attributes: [.posixPermissions: 0o700])
                let url = folder.appendingPathComponent(file.leafName)
                _ = try await self.client.exportWithStatus(name: file.name, destination: url.path)
                guard ticket == self.generation else {
                    AppLog.attempt({ try FileManager.default.removeItem(at: folder) })
                    return
                }
                try FileManager.default.setAttributes(
                    [.posixPermissions: 0o600], ofItemAtPath: url.path)
                // 先关闭“正在解密”浮层，再启动系统应用，保证顺序是“解密 → 关闭 → 打开”。
                self.decryptStatus = nil
                ExternalOpenTracker.shared.register(folder)
                self.launch(url, application: application, folder: folder)
            } catch {
                AppLog.attempt({ try FileManager.default.removeItem(at: folder) })
                AppLog.capture(error)
                if ticket == self.generation {
                    self.error = PreviewStorage.failureMessage(
                        for: error, fileSize: file.size, action: "打开")
                }
            }
        }
    }

    private func handOffPreview(_ asset: PreviewAsset, application: ExternalApplication?) {
        guard let url = asset.contentURL else {
            self.error = "文件尚未解密完成，请稍后再试。"
            return
        }
        let folder = asset.detachForExternalOpen()
        if let folder { ExternalOpenTracker.shared.register(folder) }
        launch(url, application: application, folder: folder)
    }

    /// 交给系统应用打开；成功后按实际进程号登记目录，该应用退出时回收明文副本。
    private func launch(_ url: URL, application: ExternalApplication?, folder: URL?) {
        let completion: @Sendable (NSRunningApplication?, (any Error)?) -> Void = {
            running, error in
            Task { @MainActor in
                if let error {
                    AppLog.capture(error)
                    self.error =
                        "无法用「\(application?.name ?? "系统默认应用")」打开文件：\(error.localizedDescription)"
                    ExternalOpenTracker.shared.discard(folder)
                } else {
                    self.notice =
                        application.map { "已用「\($0.name)」打开" } ?? "已用系统默认应用打开"
                    ExternalOpenTracker.shared.associate(
                        folder: folder, process: running?.processIdentifier)
                }
            }
        }
        let configuration = NSWorkspace.OpenConfiguration()
        if let application {
            NSWorkspace.shared.open(
                [url], withApplicationAt: application.url, configuration: configuration,
                completionHandler: completion)
        } else {
            NSWorkspace.shared.open(url, configuration: configuration, completionHandler: completion)
        }
    }

    /// 打开前询问「仅此次 / 总是使用 / 取消」；确认后才开始解密，避免应用先启动、用户后选择。
    /// 文件没有后缀时无法登记默认应用，只提供打开和取消。
    private func confirmOpenWith(_ application: ExternalApplication, for file: FileEntry) async
        -> Bool
    {
        let ext = OpenWithRules.normalizeExtension(forFileName: file.name)
        let hint = "打开时会先把文件解密到临时文件，锁定或退出后自动清理。"
        let alert = NSAlert()
        alert.messageText = "用「\(application.name)」打开「\(file.leafName)」？"
        if let ext {
            alert.informativeText = hint + "「总是使用」会把它登记为 .\(ext) 的默认应用，之后点击该后缀的文件就直接用这个应用打开。"
            alert.addButton(withTitle: "仅此次")
            alert.addButton(withTitle: "总是使用")
        } else {
            alert.informativeText = hint
            alert.addButton(withTitle: "打开")
        }
        alert.addButton(withTitle: "取消")
        switch alert.runModal() {
        case .alertFirstButtonReturn:
            return true
        case .alertSecondButtonReturn where ext != nil:
            guard let ext else { return true }
            await setDefaultOpenApplication(application, forExtension: ext)
            notice = "已把「\(application.name)」设为 .\(ext) 的默认应用"
            return true
        default:
            return false
        }
    }

    private func promptOpenWithExtension() -> String? {
        let alert = NSAlert()
        alert.messageText = "添加打开方式"
        alert.informativeText = "输入文件后缀，例如 mp4、pdf、zip。"
        alert.addButton(withTitle: "继续")
        alert.addButton(withTitle: "取消")
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 220, height: 24))
        field.placeholderString = "mp4"
        alert.accessoryView = field
        alert.window.initialFirstResponder = field
        guard alert.runModal() == .alertFirstButtonReturn else { return nil }
        guard let ext = OpenWithRules.normalizeUserInput(field.stringValue) else {
            self.error = "扩展名无效：只能包含字母和数字，长度 1 至 16。"
            return nil
        }
        return ext
    }

    /// 锁定、退出或切换容器时清理已交给系统应用的明文副本。
    func cleanupExternalOpens() {
        ExternalOpenTracker.shared.cleanupAll()
    }

    /// 设置页：在访达中打开解密副本的临时目录。
    func openTemporaryDirectoryInFinder() {
        do {
            try TemporaryFiles.ensureDirectory()
            NSWorkspace.shared.open(
                TemporaryFiles.directory, configuration: NSWorkspace.OpenConfiguration())
        } catch {
            AppLog.capture(error)
            self.error = "无法打开临时目录：\(error.localizedDescription)"
        }
    }

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
    @Published var contentURL: URL?
    @Published var error: String?
    @Published var unstable = false
    private var temporaryDirectory: URL?

    init(file: FileEntry) { self.file = file }

    /// 解密结果已由服务层写入 `url`；这里只收紧文件权限并登记清理责任。
    func install(_ url: URL, folder: URL) throws {
        do {
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
            temporaryDirectory = folder
            contentURL = url
        } catch { close(); throw AppLog.recorded(error) }
    }

    /// 交给系统应用打开后，关闭预览不应删除文件；清理责任转交给 AppStore 的登记表。
    func detachForExternalOpen() -> URL? {
        guard let folder = temporaryDirectory else { return nil }
        temporaryDirectory = nil
        return folder
    }

    func close() {
        contentURL = nil
        if let temporaryDirectory { AppLog.attempt({ try FileManager.default.removeItem(at: temporaryDirectory) }) }
        temporaryDirectory = nil
    }

    deinit { if let temporaryDirectory { AppLog.attempt({ try FileManager.default.removeItem(at: temporaryDirectory) }) } }
}
