/// ============================================================================
/// Veil macOS 客户端 · 全局状态中枢（AppStore）
///
/// 文件作用：集中保存并编排客户端全部界面状态——空间/容器目录、浏览参数
///   （分类、搜索、布局）、导入导出任务、冲突裁决、预览与外开、设置项与免密
///   缓存，同时承担「锁定/解锁、清理临时明文、错误日志导出」等安全生命周期。
/// 所属模块：客户端状态层（SwiftUI @MainActor ObservableObject，状态管理=集中
///   保存界面数据的机制；@MainActor=保证所有改动都在 UI 主线程执行）。
/// 文件内核心类型清单：
///   - DecryptStatus   解密浮层状态
///   - AppStore        全局状态中枢（主类型）
///   - UnlockedPassword 免密密码的内存缓存条目（私有）
///   - PreviewAsset    一次预览会话的资产（临时文件与清理责任）
/// 依赖能力：SwiftUI/AppKit（面板、文件对话框）、ServiceClient（Rust 服务调用）、
///   TemporaryFiles / ExternalOpenTracker（临时明文与外部应用登记）、
///   TransferDiagnostics（失败诊断）、OpenWithRules（默认应用登记）。
/// 整体业务目标：编排服务与界面，免密密码仅存内存但过期为按需检查，不保证定时擦除。
///   锁定清主会话并尽力清临时文件，自动锁定默认保留免密缓存，不能承诺销毁全部副本；
///   传输通过冲突裁决、进度轮询和协作取消提供反馈。
/// ============================================================================
import AppKit
import Foundation
import SwiftUI
import UniformTypeIdentifiers

/// 解密浮层状态：先把明文写进临时文件，再交给应用内查看器或系统应用。
struct DecryptStatus: Equatable, Sendable {
    /// 正在解密的文件名（仅展示）。
    let fileName: String
    /// 解密完成后的去处，例如“应用内查看器”“「OmniPlayer」”“系统默认应用”。
    let destination: String
}

/// 全局状态中枢：所有界面数据与业务操作的唯一入口。
@MainActor
final class AppStore: ObservableObject {
    /// 全部空间（含其中的容器登记）；服务端 list_spaces 的结果。
    @Published private(set) var spaces: [Space] = []
    /// recent_div本机最近缓存；externalRecents另外筛选当前展示列表未包含的容器。
    @Published private(set) var recents: [RecentContainer] = []
    /// 当前解锁的容器视图；nil 表示锁定/未打开状态。
    @Published private(set) var container: ContainerSnapshot?
    /// 进行中的操作计数；>0 时 isBusy 为真，大部分入口被禁用。
    @Published private(set) var pendingOperations = 0
    /// 当前传输进度快照（导入/导出/打包）。
    @Published private(set) var transferProgress: TransferProgress?
    /// 当前传输标题（非空时任务中心显示进行中卡片）。
    @Published private(set) var transferTitle: String?
    /// 最近一次传输失败的错误文案。
    @Published private(set) var lastTransferError: String?
    /// 最近一次传输失败的完整诊断记录（可导出）。
    @Published private(set) var lastTransferFailure: TransferDiagnosticRecord?
    /// 当前选中的空间 id；为空时回落到默认空间。
    @Published var selectedSpaceID = ""
    /// 按路径浏览时的当前目录（"" 表示根目录）。
    @Published var directory = ""
    /// 按类型浏览时选中的分类集合（空表示全部）。
    @Published var categories: Set<FileCategory> = []
    /// 搜索关键词（大小写不敏感包含匹配）。
    @Published var search = ""
    /// 默认隐藏以「.」开头的文件与目录；切换开关仅对当前浏览会话生效。
    @Published var showHiddenFiles = false
    /// 文件布局：网格或列表。
    @Published var layout: FileLayout = .grid
    /// 浏览方式：按类型或按路径。
    @Published var browseMode: BrowseMode = .type
    /// 当前表单状态（创建/解锁/导入备份）；nil 表示没有表单。
    @Published var form: ContainerForm?
    /// 是否显示「新建空间」弹窗。
    @Published var showNewSpace = false
    /// 是否显示「导入加密容器」弹窗。
    @Published var showImportPackage = false
    /// 待删除的浏览条目（文件或文件夹），删除确认弹窗据此显示。
    @Published var pendingDelete: BrowserEntry?
    /// 待删除的容器登记，容器删除确认弹窗据此显示。
    @Published var pendingContainerDelete: ContainerSummary?
    /// 待删除的空间，空间删除确认弹窗据此显示。
    @Published var pendingSpaceDelete: Space?
    /// 当前预览会话资产；非空时展示预览窗口。
    @Published var preview: PreviewAsset?
    /// 冲突裁决页显示的操作名（「导入」或「导出」）。
    @Published var conflictOperation = "导入"
    /// 当前待裁决的冲突批次；非空时弹出裁决页。
    @Published var importInspection: ImportInspection?
    /// 等待裁决页回传的续体（continuation）：用于挂起导入/导出流程直到用户选择。
    private var importReview: CheckedContinuation<ImportReviewOutcome?, Never>?
    /// 主窗口错误横幅文案。
    @Published var error: String?
    /// 主窗口成功提示横幅文案（数秒后自动消失）。
    @Published var notice: String?
    /// 已生成的缩略图缓存：文件 id → 缩略图 PNG 字节（仅在进程内存中）。
    @Published private(set) var thumbnails: [String: Data] = [:]
    /// 解锁成功后密码在本机内存中的保留时长，来自 app.db。
    @Published private(set) var unlockCacheSeconds = 3600
    /// 导入提交间隔：批量导入时合并写入加密库的等待秒数，来自 app.db。
    @Published private(set) var importCommitIntervalSeconds = 1
    /// 导入提交数量：累计多少条文件记录后合并写入，来自 app.db。
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

    /// Rust 服务调用门面（actor）。
    private let client = ServiceClient()
    /// 会话代号：每次锁定 +1，用于丢弃迟到的异步结果（防止解锁竞态污染界面）。
    private var generation: UInt64 = 0
    /// 完整加载成功标志；成功后跳过，但加载期间重入仍可能重复前置清理。
    private var loaded = false
    /// 缩略图加载批次标记，新调用覆盖；显式比较此UUID的旧结果不再写回。
    private var thumbnailBatch = UUID()
    /// 文件 id → 已生成缩略图时的文件版本号（版本变了才重新生成）。
    private var thumbnailVersions: [String: Int] = [:]
    /// 只存在进程内存中，随退出消失；不写入 app.db 或磁盘。
    /// 键为容器 id，值为密码明文与过期时间（免密重开的依据）。
    private var unlockedPasswords: [String: UnlockedPassword] = [:]

    /// 免密缓存条目：密码明文 + 过期时间，全部只在内存中。
    private struct UnlockedPassword {
        /// 本机内存密码文本，移除映射不保证底层所有String副本零化。
        let secret: String
        /// 按Date计算的免密重开截止时刻，仅查询时检查，不是定时删除任务。
        let expiresAt: Date
    }

    /// 免密时长的展示文案（如「1 小时」「30 分钟」「关闭」）。
    var unlockCacheDescription: String { Self.durationLabel(unlockCacheSeconds) }

    /// 把免密偏好秒数换成便于设置页阅读的时长。
    /// 入参：seconds（Int）为秒数，允许非正值表示关闭。
    /// 返回值：String：非正关闭，1到3599按至少1分钟且向下取整，之后按整数小时。
    /// 异常：不抛错。
    /// 注意事项：只是显示，忽略余下秒/分钟，不校验服务允许值。
    static func durationLabel(_ seconds: Int) -> String {
        switch seconds {
        case ...0: "关闭"
        case ..<3600: "\(max(1, seconds / 60)) 分钟"
        default: "\(seconds / 3600) 小时"
        }
    }

    /// 为容器缓存成功解锁时的密码和未来免密截止时间。
    /// 入参：password（String）为密码明文；id（String）为稳定容器ID。
    /// 返回值：Void，时长>0保存/覆盖，关闭则移除该ID。
    /// 异常：不抛错。
    /// 注意事项：Swift String不受零化包装；删除映射不保证擦除底层全部副本。过期不是定时移除，取用时才校验。
    private func rememberUnlock(_ password: String, id: String) {
        guard unlockCacheSeconds > 0 else { unlockedPasswords.removeValue(forKey: id); return }
        unlockedPasswords[id] = UnlockedPassword(
            secret: password,
            expiresAt: Date().addingTimeInterval(TimeInterval(unlockCacheSeconds))
        )
    }

    /// 取一个容器尚未过期的内存密码，供免密重开。
    /// 入参：id（String）为容器ID。
    /// 返回值：String?，无条目/已过期nil，过期顺便删除；未过期返回secret。
    /// 异常：不抛错。
    /// 注意事项：使用Date墙上时钟，时间跳变影响期限；不验证密码仍可打开容器，也不延长有效期。
    private func cachedUnlock(for id: String) -> String? {
        guard let entry = unlockedPasswords[id] else { return nil }
        guard entry.expiresAt > Date() else { unlockedPasswords.removeValue(forKey: id); return nil }
        return entry.secret
    }

    /// 观察传输活跃时顺延当前容器已有的免密缓存。
    /// 入参：无，使用当前container、时长和对应缓存。
    /// 返回值：Void，无容器/缓存或关闭则跳过，否则将截止设为现在加时长。
    /// 异常：不抛错。
    /// 注意事项：不是按Rust续租成功回执顺延，而由本地进度轮询触发；不会自动创建缺失条目，也不触发密码验证。
    private func extendUnlockCacheForActiveTransfer() {
        guard unlockCacheSeconds > 0, let id = container?.id,
            let entry = unlockedPasswords[id]
        else { return }
        unlockedPasswords[id] = UnlockedPassword(
            secret: entry.secret,
            expiresAt: Date().addingTimeInterval(TimeInterval(unlockCacheSeconds)))
    }

    /// 是否有操作正在进行。
    var isBusy: Bool { pendingOperations > 0 }
    /// 浏览是否可用：空闲时可用；导入进行中也允许浏览（边导边看）。
    var canBrowse: Bool { !isBusy || transferProgress?.operation == "import" }
    /// 当前生效空间：优先选中项，其次默认空间，最后第一个空间；无空间时为 nil。
    var activeSpace: Space? { spaces.first { $0.id == selectedSpaceID } ?? spaces.first { $0.isDefault } ?? spaces.first }
    /// 当前视图应展示的条目（按浏览方式/目录/分类/搜索/隐藏规则过滤）。
    var visibleEntries: [BrowserEntry] {
        guard let container else { return [] }
        return browserEntries(container: container, mode: browseMode, directory: directory, categories: categories, search: search, showHidden: showHiddenFiles)
    }
    /// 最近打开记录里「不在任何空间登记范围内」的外部容器。
    var externalRecents: [RecentContainer] {
        recents.filter { recent in !spaces.contains { $0.containers.contains { $0.id == recent.id } } }
    }

    /// 缩略图候选：网格布局下、可预览图片且不超过 2 MiB 的前 12 个文件。
    private var thumbnailCandidates: [FileEntry] {
        guard layout == .grid else { return [] }
        return Array(visibleEntries.compactMap { entry -> FileEntry? in
            guard case .file(let file) = entry, file.previewMime?.hasPrefix("image/") == true,
                  file.size <= 2 * 1024 * 1024 else { return nil }
            return file
        }.prefix(12))
    }

    /// 缩略图缓存键：会话/容器/布局/候选文件 id 与版本的组合，任一变化就重新加载。
    var thumbnailKey: String {
        "\(generation):\(container?.id ?? ""):\(layout.rawValue):" + thumbnailCandidates.map { "\($0.id):\($0.version)" }.joined(separator: ",")
    }

    /// 为当前网格候选逐个读取PNG缩略图，并淘汰已不在候选或版本变化的缓存。
    /// 入参：无，读取thumbnailCandidates及generation。
    /// 返回值：Void，图片结果按文件ID存Data，失败保留类型图标。
    /// 异常：服务失败记录后继续，不向外抛。
    /// 注意事项：候选最多12且文件不超过2MiB；候选ID必须唯一，否则Dictionary(uniqueKeysWithValues:)会失败。批次UUID与generation检查防迟到写回，仍不保证Data内存零化。
    func loadThumbnails() async {
        let batch = UUID(), ticket = generation
        thumbnailBatch = batch
        let candidates = thumbnailCandidates
        let ids = Set(candidates.map(\.id))
        // 候选 id → 最新版本；用于判断旧缓存是否仍有效。
        let versions = Dictionary(uniqueKeysWithValues: candidates.map { ($0.id, $0.version) })
        // 先丢弃不在候选内或版本已变化的旧缩略图。
        thumbnails = thumbnails.filter { ids.contains($0.key) && versions[$0.key] == thumbnailVersions[$0.key] }
        thumbnailVersions = versions
        // 逐个补齐缺失的缩略图；每次循环前都确认批次未作废（会话锁定/换目录）。
        for file in candidates where thumbnails[file.id] == nil {
            guard !Task.isCancelled, ticket == generation, batch == thumbnailBatch else { return }
            do {
                let image = try await client.thumbnail(name: file.name)
                guard !Task.isCancelled, ticket == generation, batch == thumbnailBatch else { return }
                thumbnails[file.id] = image
            } catch { AppLog.capture(error); /* 不可解码的图片使用类型图标，不影响文件操作。 */ }
        }
    }

    /// 启动时尝试清理旧临时副本、恢复诊断，再读取目录与偏好。
    /// 入参：无。
    /// 返回值：Void，完整读取成功后loaded=true，之后调用直接返回。
    /// 异常：服务/解码失败经perform捕获；打开方式偏好读取失败单独回退空映射。
    /// 注意事项：loaded不是开始时置位，await前可再次进入并重复purge；设置多项分步写回，失败不是原子恢复。清理尽力且可能影响旧进程外开文件。
    func load() async {
        guard !loaded else { return }
        // 启动即清空上次遗留的明文副本（崩溃、旧版本或外部应用未退出都可能留下）。
        TemporaryFiles.purge(
            root: TemporaryFiles.directory,
            legacyRoot: FileManager.default.temporaryDirectory)
        lastTransferFailure = TransferDiagnostics.latestRecord()
        await perform { _ in
            // 先取空间/最近记录目录，再逐项读回设置（与 CLI 共用 app.db 的键）。
            try await self.reloadCatalog()
            let layout: String = try await self.client.json(NativeRequest(operation: "get_preference", key: "layout"))
            let mode: String = try await self.client.json(NativeRequest(operation: "get_preference", key: "view_mode"))
            let cache: Double = try await self.client.json(NativeRequest(operation: "get_preference", key: "unlock_cache_seconds"))
            let interval: Int = try await self.client.json(NativeRequest(operation: "get_preference", key: "import_commit_interval_seconds"))
            let count: Int = try await self.client.json(NativeRequest(operation: "get_preference", key: "import_commit_count"))
            let hint: String = try await self.client.json(NativeRequest(operation: "get_preference", key: "hint_level"))
            let device: String? = try await self.client.json(NativeRequest(operation: "get_system_setting", key: "device_id"))
            // 打开方式登记可能尚未创建过，读取失败按空处理。
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

    /// 为主要用户动作维护忙计数、会话票据和错误展示。
    /// 入参：allowDuringImport（Bool）默认false，true可在canBrowse时放行；work（(UInt64) async throws -> Void）接收开始时generation。
    /// 返回值：Void，不满足入口条件直接忽略；执行后计数减回。
    /// 异常：work错误记录，仅票据仍相同时显示error，不向外抛。
    /// 注意事项：不是互斥锁，MainActor在await期间可重入；work须自行比较ticket防旧结果写回。允许导入并发时多个pendingOperations同时存在。
    private func perform(allowDuringImport: Bool = false, _ work: (UInt64) async throws -> Void) async {
        guard !isBusy || (allowDuringImport && canBrowse) else { return }
        pendingOperations += 1
        error = nil
        let ticket = generation
        defer { pendingOperations -= 1 }
        do { try await work(ticket) }
        catch { AppLog.capture(error); if ticket == generation { self.error = error.localizedDescription } }
    }

    /// 为一次长传输建立标题、轮询状态并在失败时生成诊断。
    /// 入参：title（String）为展示标题；operation（String）为过滤方向；work（async throws -> T）为真正传输。
    /// 返回值：T为work结果。
    /// 异常：失败设置lastTransferError；未观察到cancelRequested且描述不含“已中止”才写诊断，随后经AppLog.recorded抛出。
    /// 注意事项：每250ms轮询非固定响应上限；导入刷新节流至少2秒，并满足新增512项或已隔10秒。进度赋值/标题清理未统一查generation；defer取消轮询不取消work或撤销文件。
    private func withTransferProgress<T>(_ title: String, operation: String, work: () async throws -> T) async throws -> T {
        transferTitle = title
        transferProgress = nil
        lastTransferError = nil
        lastTransferFailure = nil
        // 轮询任务：每 250ms 读一次进度；导入中按节流条件顺带刷新容器视图。
        let poll = Task { @MainActor in
            var lastRefreshedCompleted: UInt64 = 0
            var lastContainerRefresh = Date.distantPast
            while !Task.isCancelled {
                if let progress = client.transferProgress(), progress.operation == operation,
                   ["preparing", "processing", "verifying", "snapshot", "packing", "unpacking", "finalizing"].contains(progress.phase) {
                    transferProgress = progress
                    // 任务仍在推进：顺延免密缓存，避免长任务结束后密码已过期。
                    extendUnlockCacheForActiveTransfer()
                    let now = Date()
                    // 刷新节流：至少新增 512 个文件，或距上次刷新超过 10 秒才刷新目录。
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
            // 用户主动取消（或已请求取消）不算故障，不生成诊断记录。
            if failureProgress?.cancelRequested != true,
               !error.localizedDescription.localizedCaseInsensitiveContains("已中止") {
                let record = TransferDiagnostics.makeRecord(error: error, operation: operation, progress: failureProgress)
                lastTransferFailure = record
                TransferDiagnostics.append(record)
            }
            throw AppLog.recorded(error)
        }
    }

    /// 诊断日志导出的默认文件名（时间戳 + .json）。
    var transferDiagnosticFilename: String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = TimeZone.current
        formatter.dateFormat = "yyyyMMdd-HHmmss"
        return "Veil-诊断日志-\(formatter.string(from: Date())).json"
    }

    /// 将日志JSON写到用户选择位置并更新界面提示。
    /// 入参：url（URL）为目标文件。
    /// 返回值：Void，成功设notice，失败设error。
    /// 异常：写入/读取错误捕获记录。
    /// 注意事项：Data.write(.atomic)可替换已有目标，不是排他创建；不追加安全净化历史正文，也不提供断电持久保证。
    func exportErrorLogs(to url: URL) {
        do {
            try AppLog.export().write(to: url, options: .atomic)
            notice = "错误日志已导出，可附在反馈中。"
        } catch {
            AppLog.capture(error)
            self.error = "无法导出错误日志：\(error.localizedDescription)"
        }
    }

    /// 把最近失败诊断记录保存到选定文件。
    /// 入参：url（URL）为目标。
    /// 返回值：Void，无lastTransferFailure直接返回。
    /// 异常：导出错误捕获记录并显示。
    /// 注意事项：同步MainActor调用，不通过perform，调用方负责选择合法目标。
    func exportTransferDiagnostic(to url: URL) {
        guard let lastTransferFailure else { return }
        do {
            try TransferDiagnostics.export(lastTransferFailure, to: url)
            notice = "诊断日志已导出，可附在反馈中。"
        } catch { AppLog.capture(error);
            self.error = "无法导出诊断日志：\(error.localizedDescription)"
        }
    }

    /// 向Rust请求协作取消，并在受理时刷新一次进度。
    /// 入参：无。
    /// 返回值：Void，未受理则不改界面。
    /// 异常：不抛错。
    /// 注意事项：不是等待取消完成，不回滚已提交文件；失败/no active/已取消均可不受理。
    func cancelTransfer() {
        guard client.cancelTransfer() else { return }
        transferProgress = client.transferProgress()
    }

    /// 分次读取空间与最近记录，并修正无效空间选择。
    /// 入参：无。
    /// 返回值：Void，选中ID不存在则使用activeSpace回退。
    /// 异常：服务/解码错误向上抛。
    /// 注意事项：两次查询非同一快照，spaces写入后recents仍可能失败；没有generation检查，await期间可被会话变化交错。
    private func reloadCatalog() async throws {
        spaces = try await client.json(NativeRequest(operation: "list_spaces"))
        recents = try await client.json(NativeRequest(operation: "list_recents"))
        if !spaces.contains(where: { $0.id == selectedSpaceID }) { selectedSpaceID = activeSpace?.id ?? "" }
    }

    /// 在非忙状态刷新空间视图，必要时锁定原已登记容器。
    /// 入参：无。
    /// 返回值：Void，失败记录后返回。
    /// 异常：不向外抛。
    /// 注意事项：只开头查isBusy，不占忙计数；离线空间也可能省略容器摘要，条目消失不代表磁盘删除。lock返回false在此忽略。
    func refreshSpaceAvailability() async {
        guard !isBusy else { return }
        // 刷新前记录当前容器是否登记在某个空间里。
        let wasRegisteredContainer = container.map { opened in
            spaces.contains(where: { $0.containers.contains(where: { $0.id == opened.id }) })
        } ?? false
        do {
            try await reloadCatalog()
            // 原本登记的空间被移除（例如外置盘被拔走后删记录）：锁定以清掉敏感状态。
            if wasRegisteredContainer, let container,
               !spaces.contains(where: { $0.containers.contains(where: { $0.id == container.id }) }) {
                _ = await lock()
            }
        } catch { AppLog.capture(error); return }
    }

    /// 清理当前浏览派生状态和应用内预览。
    /// 入参：无。
    /// 返回值：Void，换缩略图批次、清图片/版本映射、目录/分类/搜索/隐藏开关和待删项。
    /// 异常：不抛错，预览文件删除失败经其他清理入口记录。
    /// 注意事项：不清container、免密缓存、外开登记或全部表单；缩略图Data移除引用非安全擦除。
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

    @discardableResult
    /// 先隐藏主会话界面并请求取消，再清理临时副本并让Rust锁定。
    /// 入参：forgetPassword（Bool）默认false保留免密缓存；true移除所有容器免密密码。
    /// 返回值：Bool，Rust lock请求成功true，失败false但界面仍已清空。
    /// 异常：错误捕获记录显示，不向外抛；清理失败不保证全部反馈成false。
    /// 注意事项：不受isBusy阻挡；generation递增只约束主动检查的结果，缓存密码/在途请求/系统外开副本不保证全部销毁。清理临时目录可能影响仍使用明文的外部应用。
    func lock(forgetPassword: Bool = false) async -> Bool {
        // 递增代次，只有后续显式比较ticket的旧结果才会停止写回。
        generation &+= 1
        _ = client.cancelTransfer()
        completeImportReview(nil)
        container = nil
        form = nil
        resetBrowser()
        cleanupExternalOpens()
        // 尽力清理本应用登记的临时副本；删除失败、其他进程副本或未结束任务不能据此保证清除。
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

    /// 切换当前空间，若有已打开容器先请求锁定。
    /// 入参：space（Space）为目标快照。
    /// 返回值：Void，忙或lock失败则保持后续选择不变。
    /// 异常：不向外抛。
    /// 注意事项：不另验目标仍存在/在线；lock失败可能已经清界面，不能理解为操作全未发生。
    func selectSpace(_ space: Space) async {
        guard !isBusy else { return }
        if container != nil { guard await lock() else { return } }
        selectedSpaceID = space.id
        form = nil
        resetBrowser()
        error = nil
    }

    /// 选择已知容器，优先用未过期密码尝试重开，否则显示密码表单。
    /// 入参：item（ContainerSummary）为容器ID/缓存名/目录。
    /// 返回值：Void，忙/已是当前ID则跳过。
    /// 异常：缓存重开错误记录并移除该ID缓存，回退表单；列表刷新错误只设notice。
    /// 注意事项：缓存分支await后未统一检查generation，可能迟到写container；任何错误都会移除缓存，不仅密码失效。同ID但换路径也可能因已打开直接忽略。
    func chooseContainer(_ item: ContainerSummary) async {
        guard !isBusy else { return }
        if container?.id == item.id { return }
        if container != nil { guard await lock() else { return } }
        // 把选中空间切到容器所在空间，让后续新建操作落在正确的位置。
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
                // 任意重开失败都清缓存并回到表单，不仅密码失效；也可能是路径/服务错误。
                unlockedPasswords.removeValue(forKey: item.id)
            }
        }
        // 无免密缓存：展示解锁表单让用户输入密码。
        form = .open(path: item.path, name: item.name)
    }

    /// 为可用空间进入创建表单，先锁定当前主会话。
    /// 入参：spaceID（String?）显式目标ID，nil使用activeSpace。
    /// 返回值：Void，忙/无目标/目标展示不可用或lock失败跳过。
    /// 异常：不向外抛。
    /// 注意事项：只准备UI表单，不建数据库；空间可用快照仍须提交时服务复核。
    func newContainer(in spaceID: String? = nil) async {
        guard !isBusy, let space = spaceID ?? activeSpace?.id,
              spaces.contains(where: { $0.id == space && $0.isAvailable }) else { return }
        if container != nil { guard await lock() else { return } }
        selectedSpaceID = space
        resetBrowser()
        error = nil
        form = .create(space: space)
    }

    /// 关闭当前表单和错误提示。
    /// 入参：无。
    /// 返回值：Void，忙时忽略。
    /// 异常：不抛错。
    /// 注意事项：不重新解锁表单前的容器，不回滚此前lock/resetBrowser。
    func cancelForm() {
        guard !isBusy else { return }
        form = nil
        error = nil
    }

    /// 用原生面板选择容器目录，再进入统一选择流程。
    /// 入参：无。
    /// 返回值：Void，面板取消无后续动作。
    /// 异常：不向外抛。
    /// 注意事项：面板只限制目录，不检查veil.db存在；以路径充当临时容器ID，真实身份要解锁后确认。runModal同步阻塞当前交互。
    func chooseContainerDirectory() async {
        guard !isBusy else { return }
        let panel = NSOpenPanel()
        panel.title = "选择包含 veil.db 的容器目录"
        panel.prompt = "打开容器"
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        guard panel.runModal() == .OK, let url = panel.url else { return }
        // 以目录路径当作临时 id，走 chooseContainer 的统一解锁流程。
        await chooseContainer(ContainerSummary(id: url.path, name: url.lastPathComponent, path: url.path))
    }

    /// 为可用当前空间显示导入备份表单。
    /// 入参：无。
    /// 返回值：Void，非忙且activeSpace可用才置showImportPackage。
    /// 异常：不抛错。
    /// 注意事项：这里不选择/读取包，也不锁定当前容器；具体提交另行处理。
    func choosePackage() async {
        guard !isBusy, activeSpace?.isAvailable == true else { return }
        error = nil
        showImportPackage = true
    }

    /// 验证表单后锁旧会话，执行创建/打开/还原并缓存解锁密码。
    /// 入参：form（ContainerForm）为场景；name为创建展示名；password为非空密码；confirmation仅创建校验一致。
    /// 返回值：Void，成功更新container/form/缓存并尝试刷新目录。
    /// 异常：本地验证设validation错误；请求错误由perform捕获，目录刷新错误单独notice。
    /// 注意事项：创建名先trim；代次过期补发Rust lock，但该请求也可能失败。业务成功后刷新失败仍保持已开容器，创建/还原数据不因此回滚。
    func submit(_ form: ContainerForm, name: String, password: String, confirmation: String) async {
        let spaceID: String?
        switch form {
        // 创建与导入备份都作用在某个空间；解锁与空间无关。
        case .create(let space): spaceID = space
        case .importPackage(_, let space): spaceID = space
        case .open: spaceID = nil
        }
        // 目标空间不可用（外置盘被拔出）时直接拦截。
        if let spaceID, !spaces.contains(where: { $0.id == spaceID && $0.isAvailable }) {
            error = AppLog.validation("空间当前不可用，请重新挂载外置磁盘后再试")
            return
        }
        guard !password.isEmpty else { error = AppLog.validation("请输入容器密码"); return }
        // 创建流程要求两次密码一致、名称非空。
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
                // 导入备份可能耗时较长：包一层传输进度展示。
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
            // 解锁成功后记住密码，免密时长内重开无需再输入。
            self.rememberUnlock(password, id: view.id)
            do {
                try await self.reloadCatalog()
                if let space = self.spaces.first(where: { $0.containers.contains(where: { $0.id == view.id }) }) { self.selectedSpaceID = space.id }
            } catch { AppLog.capture(error); self.notice = "容器已打开，空间列表刷新失败：\(error.localizedDescription)" }
        }
    }

    /// 创建新空间并刷新可选空间，不隐式替换已开容器。
    /// 入参：name（String）先trim；directory（URL?）可选自定义目录，nil让服务选择默认位置。
    /// 返回值：Void，perform允许且票据有效才关闭表单/更新spaces；无已开容器时按名称选中新空间。
    /// 异常：服务/权限/解码错误由perform捕获。
    /// 注意事项：申请安全作用域返回false仍尝试创建；只对true配对stop。服务成功而票据过期可不更新UI，创建本身不回滚。
    func createSpace(name: String, directory: URL? = nil) async {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        // 沙盒文件授权：访问用户选择的目录前需要安全作用域凭证。
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
    /// 必要时先清本界面和锁定，再请求删除容器并刷新目录。
    /// 入参：item（ContainerSummary）用稳定ID定位。
    /// 返回值：Bool，全部后续目录读取/状态处理完成才true；忙/错误/过期为false。
    /// 异常：错误由perform记录，不向外抛。
    /// 注意事项：删除已成功后list_recents失败仍返回false；匹配当前容器会先清UI，即使后续lock或删除失败。
    func deleteContainer(_ item: ContainerSummary) async -> Bool {
        var deleted = false
        await perform { ticket in
            // 删除的是当前打开的容器：先锁定并清空界面。
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

    /// 依展示可用状态请求删除空间或移除不可用空间记录。
    /// 入参：space（Space）为目标快照。
    /// 返回值：Void，票据有效才更新空间/最近列表和提示。
    /// 异常：请求/解码/最近读取失败由perform处理。
    /// 注意事项：先按归属清界面，Rust分支负责实际锁会话；免密仅移除快照space.containers列出的ID，离线或嵌套空间未列项不能据此保证全清。
    func deleteSpace(_ space: Space) async {
        await perform { ticket in
            let openContainer = self.container
            // 当前打开的容器属于该空间（按登记或按目录路径判断）时，先锁定。
            let belongsToSpace = space.containers.contains(where: { $0.id == openContainer?.id })
                || openContainer.map { URL(fileURLWithPath: $0.path).deletingLastPathComponent().path == space.path } == true
            if belongsToSpace {
                self.container = nil
                self.form = nil
                self.resetBrowser()
            }
            // 空间目录不可访问（磁盘拔出）时只能删除登记记录，磁盘文件保留。
            let spaces: [Space] = try await self.client.json(
                NativeRequest(operation: space.isAvailable ? "delete_space" : "delete_space_record", id: space.id)
            )
            guard ticket == self.generation else { return }
            // 仅清此展示快照列出的容器密码；离线摘要为空或嵌套空间未列项时不代表全范围缓存已删。
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

    /// 刷新容器视图及本机目录，导入期间允许手动查看。
    /// 入参：无。
    /// 返回值：Void，经perform(allowDuringImport:true)执行。
    /// 异常：错误由perform处理。
    /// 注意事项：容器结果查票据，reloadCatalog本身不按generation过滤；多请求可交错，不是固定跨表快照。
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

    /// 把当前布局与浏览模式分别保存到用户偏好。
    /// 入参：无，开始时读取layout/browseMode原始值。
    /// 返回值：Void，两条保存都完成才设置settingsFeedback。
    /// 异常：错误记录并设error。
    /// 注意事项：不走perform，可并发；两条独立写入，第一条成功后第二条失败不会回滚第一条，也不自动回退UI。
    func savePreferences() async {
        let layout = layout.rawValue, mode = browseMode.rawValue
        do {
            try await client.execute(NativeRequest(operation: "set_preference", key: "layout", value: .string(layout)))
            try await client.execute(NativeRequest(operation: "set_preference", key: "view_mode", value: .string(mode)))
            settingsFeedback = "布局与浏览方式已保存"
        } catch { AppLog.capture(error); self.error = error.localizedDescription }
    }

    /// 保存免密重开时长。
    /// 入参：seconds（Int）为秒数，0关闭，服务验证允许范围。
    /// 返回值：Void，先改unlockCacheSeconds，成功设反馈，失败恢复本次捕获旧值。
    /// 异常：捕获服务/编码/校验错误并记录显示。
    /// 注意事项：绕过busy，不保证保存成功；await期间多次保存可交错，失败回退可能覆盖更新的UI选择。成功且为0时移除缓存映射；缩短非零时长不会立即重算已有条目的expiresAt。
    func setUnlockCache(seconds: Int) async {
        let previous = unlockCacheSeconds
        unlockCacheSeconds = seconds
        do {
            try await client.execute(NativeRequest(operation: "set_unlock_cache", seconds: seconds))
            // 移除免密缓存映射，不保证Swift String的其他副本安全擦除。
            if seconds == 0 { unlockedPasswords.removeAll() }
            settingsFeedback = seconds == 0 ? "已关闭免密重开" : "免密时长已设为 \(unlockCacheDescription)"
        } catch {
            unlockCacheSeconds = previous
            AppLog.capture(error)
            self.error = error.localizedDescription
        }
    }

    /// 保存后续导入的批量提交时间阈值。
    /// 入参：seconds（Int）为秒数，服务校验范围。
    /// 返回值：Void，先改importCommitIntervalSeconds，成功设反馈，失败恢复本次捕获旧值。
    /// 异常：捕获服务/编码/校验错误并记录显示。
    /// 注意事项：绕过busy，不保证保存成功；await期间多次保存可交错，失败回退可能覆盖更新的UI选择。不保证改变已经读取策略的在途导入，阈值不是硬提交截止。
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

    /// 保存后续导入的提交数量阈值。
    /// 入参：count（Int）为条数，服务校验正值及范围。
    /// 返回值：Void，先改importCommitCount，成功设反馈，失败恢复本次捕获旧值。
    /// 异常：捕获服务/编码/校验错误并记录显示。
    /// 注意事项：绕过busy，不保证保存成功；await期间多次保存可交错，失败回退可能覆盖更新的UI选择。在途管线通常已读取策略，不随此属性即时重配置。
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

    /// 保存与CLI共用的提示详细程度。
    /// 入参：level（HintLevel）为full/brief/off。
    /// 返回值：Void，先改hintLevel，成功设反馈，失败恢复本次捕获旧值。
    /// 异常：捕获服务/编码/校验错误并记录显示。
    /// 注意事项：绕过busy，不保证保存成功；await期间多次保存可交错，失败回退可能覆盖更新的UI选择。CLI进程VEIL_HINTS仍可能覆盖此持久值。
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

    /// 保存默认空间ID后重读目录中的默认标记。
    /// 入参：id（String）为现有空间ID。
    /// 返回值：Void，保存及reloadCatalog都成功才设置成功反馈。
    /// 异常：捕获记录并设置error。
    /// 注意事项：保存成功后刷新失败不恢复旧默认；不走perform/generation保护，可能与其他刷新交错。
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

    /// 用文件面板选择来源并进入流式导入。
    /// 入参：directory（Bool）true只选目录、false只选文件，均允许多选。
    /// 返回值：Void，忙/未开容器/面板取消时结束。
    /// 异常：不直接抛错，后续importFiles处理。
    /// 注意事项：runModal同步显示面板；返回URL不证明每个文件最终可读取。
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

    /// 导入进度的文案（冲突裁决页展示）：已完成/总数、百分比与遍历状态。
    var importProgressDescription: String? {
        guard let progress = transferProgress, progress.totalFiles > 0 else { return nil }
        return "已完成 \(progress.completedFiles) / \(progress.totalFiles) · \(Int(min(1, Double(progress.completedFiles) / Double(progress.totalFiles))*100))%" + (progress.scanComplete ? " · 遍历完成" : " · 总数持续增加")
    }

    /// 将本批冲突结果恢复给等待任务并摘除UI登记。
    /// 入参：outcome（ImportReviewOutcome?）nil为中止；id（String?）有值时须匹配当前inspection。
    /// 返回值：Void，先清continuation/inspection后resume一次。
    /// 异常：不抛错。
    /// 注意事项：无id可结束任意当前批次；本方法不直接提交Rust决议或取消传输。
    func completeImportReview(_ outcome: ImportReviewOutcome?, id: String? = nil) {
        // 批次 id 不匹配说明是过期裁决（例如锁定后），直接丢弃。
        if let id, importInspection?.id != id { return }
        let continuation = importReview
        importReview = nil
        importInspection = nil
        continuation?.resume(returning: outcome)
    }

    /// 为非空冲突批次挂起等待审核页结果。
    /// 入参：inspection（ImportInspection）为服务批次token及目标集合。
    /// 返回值：ImportReviewOutcome?，空冲突返回空决定，无选择/取消返回nil。
    /// 异常：不抛错。
    /// 注意事项：取消处理另派MainActor任务恢复continuation，不是同步等待取消完成；调用方须保证共享审核槽不被其他并发任务覆盖。
    private func reviewImport(_ inspection: ImportInspection) async -> ImportReviewOutcome? {
        if inspection.conflicts.isEmpty {
            return ImportReviewOutcome(resolutions: [], stickyAction: nil)
        }
        // 取消时另派MainActor任务恢复nil，需待该任务得到调度，不是同步取消Rust工作。
        return await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                importReview = continuation
                importInspection = inspection
            }
        } onCancel: {
            Task { @MainActor in self.completeImportReview(nil, id: inspection.id) }
        }
    }



    /// 持续读取当前传输冲突，按用户统一动作或逐批审核回传决议。
    /// 入参：ticket（UInt64）为会话票据；exporting（Bool）默认false，true改用导出操作名。
    /// 返回值：Void，取消/代次变动/用户中止或错误退出。
    /// 异常：错误记录，有效票据时设置error并请求Rust取消。
    /// 注意事项：通常每150ms轮询，sticky模式成功提交后立即取下一批；不可覆盖项在统一覆盖中转skip。审核期间其他文件仍可推进，循环结束不证明Rust已结束。
    private func pollImportConflicts(ticket: UInt64, exporting: Bool = false) async {
        // 用户选择「全部跳过 / 全部覆盖」后，后续批次不再弹窗，直接沿用。
        var sticky: ImportAction?
        while !Task.isCancelled, ticket == generation {
            do {
                let batch: ImportInspection? = try await client.json(NativeRequest(operation: exporting ? "poll_export_conflicts" : "poll_import_conflicts"))
                if let batch {
                    guard ticket == generation, !Task.isCancelled else { return }
                    self.conflictOperation = exporting ? "导出" : "导入"
                    let outcome: ImportReviewOutcome
                    if let action = sticky {
                        // 粘性模式：不再弹窗，按统一动作生成全部裁决。
                        outcome = ImportReviewOutcome(
                            resolutions: batch.resolutions(for: action),
                            stickyAction: action)
                    } else {
                        // 弹窗裁决；用户中止（返回 nil）则取消整个传输。
                        guard let reviewed = await reviewImport(batch), ticket == generation,
                            !Task.isCancelled
                        else {
                            _ = client.cancelTransfer()
                            return
                        }
                        outcome = reviewed
                    }
                    if let action = outcome.stickyAction {
                        // 首次选择「不再询问」时给出提示，之后保持同一动作。
                        if sticky == nil {
                            notice =
                                action == .skip
                                ? "后续同名文件将自动跳过，不再询问"
                                : "后续同名文件将自动覆盖，无法覆盖的跳过"
                        }
                        sticky = action
                    }
                    guard let resolutions = outcome.resolutions else {
                        // 用户中止本批：取消传输。
                        _ = client.cancelTransfer()
                        return
                    }
                    try await client.execute(NativeRequest(operation: exporting ? "resolve_export_conflicts" : "resolve_import_conflicts", id: batch.id, resolutions: resolutions))
                    // 已选择统一处理时继续立即取下一批，不必等轮询间隔。
                    if sticky != nil { continue }
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

    /// 向当前目录/根目录流式导入全部来源，并轮询审核和进度。
    /// 入参：urls（[URL]）每项本地文件或目录，非空且需有container。
    /// 返回值：Void，成功写快照，取消可只显示保留成果提示。
    /// 异常：普通失败尽力刷新已提交文件后交perform显示。
    /// 注意事项：安全作用域true集合仅用于配对stop，实际发送全部urls；目录在开始时捕获。defer取消审核不撤销提交，完成数包含跳过。
    func importFiles(_ urls: [URL]) async {
        guard !urls.isEmpty, container != nil else { return }
        // 按路径浏览时导入到当前目录；按类型浏览时导入到容器根目录。
        let destination = browseMode == .path ? directory : ""
        // 沙盒文件授权：对来自文件面板/拖放的路径持有访问凭证直到导入结束。
        let scoped = urls.filter { $0.startAccessingSecurityScopedResource() }
        defer { scoped.forEach { $0.stopAccessingSecurityScopedResource() } }
        await perform { ticket in
            // 并行启动冲突轮询任务；导入结束时取消并清掉未完成的裁决弹窗。
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
                    // 失败后尽力刷新一次目录，展示已入库的部分文件。
                    if ticket == self.generation,
                       let refreshed: ContainerSnapshot = try? await self.client.json(NativeRequest(operation: "refresh")), ticket == self.generation { self.container = refreshed }
                    // 用户主动中止：给出温和提示，不算错误。
                    if ticket == self.generation, let progress, progress.phase == "cancelled" {
                        self.notice = "导入已中止，已完成 \(progress.completedFiles) 个文件（其中跳过 \(progress.skippedFiles) 个）"
                        return
                    }
                    throw AppLog.recorded(error)
                }
            }
        }
    }

    /// 在当前路径下验证并创建一个子目录。
    /// 入参：rawName（String）为原输入，folderNameForCreation校验后使用。
    /// 返回值：Void，路径模式且已开容器时经perform提交。
    /// 异常：本地非法设验证错误，服务错误perform处理。
    /// 注意事项：捕获父目录，不跟随await期间用户导航；票据过期时目录仍可能已建，只不更新UI。
    func createFolder(named rawName: String) async {
        guard let name = folderNameForCreation(rawName) else {
            error = AppLog.validation("文件夹名称无效，请使用不含路径分隔符的名称")
            return
        }
        guard browseMode == .path, container != nil else { return }
        // 新文件夹建在当前目录下。
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

    /// 按条目类型进入目录，或按默认打开规则选择外开/内置预览。
    /// 入参：entry（BrowserEntry）为当前浏览项。
    /// 返回值：Void。
    /// 异常：子流程各自记录错误，不向外抛。
    /// 注意事项：进入目录直接赋directory，不在此复查存在/忙；登记应用缺失回退内置预览，和显式openExternally的系统默认回退不同。
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
            // 登记过但应用已卸载：提示降级为内置预览。
            if resolution.configured {
                notice = "未找到登记的应用，已在应用内预览"
            }
            await showPreview(file)
        }
    }

    /// 把受支持文件解密到随机临时目录，再交预览资产持有URL。
    /// 入参：file（FileEntry）提供内部路径、大小和预览MIME。
    /// 返回值：Void，不可预览或预检不足时建立带错误的PreviewAsset。
    /// 异常：目录/服务/权限错误捕获记录，并按容量错误转换文案。
    /// 注意事项：允许导入时查看；容量只瞬时预检且查询系统临时卷，真正目录由TemporaryFiles决定。无16MiB整文件读取限制，权限/清理仍尽力；票据变动只在检查点阻止写回。
    func showPreview(_ file: FileEntry) async {
        guard canBrowse else { return }
        closePreview()
        let asset = PreviewAsset(file: file)
        // 服务层标记为不可预览的类型：直接给出提示，不解密。
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
            // 每个预览使用独立的随机命名目录，权限仅当前用户可读写。
            let folder = TemporaryFiles.directory
                .appendingPathComponent("veil-native-preview-" + UUID().uuidString, isDirectory: true)
            do {
                try TemporaryFiles.ensureDirectory()
                try FileManager.default.createDirectory(
                    at: folder, withIntermediateDirectories: false,
                    attributes: [.posixPermissions: 0o700]
                )
                let url = folder.appendingPathComponent(file.leafName)
                // 服务层把解密结果写入该 URL（kind=4 时内容仍可能变化）。
                let unstable = try await self.client.exportWithStatus(name: file.name, destination: url.path)
                guard ticket == self.generation else {
                    // 会话已变化（例如锁定）：清掉刚解密的文件。
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

    /// 关闭当前预览并释放其临时目录拥有关系。
    /// 入参：无。
    /// 返回值：Void，调用asset.close后设preview=nil。
    /// 异常：删除失败在资产内部记录。
    /// 注意事项：已detach给外开的目录不在此删除；不保证磁盘无残留。
    func closePreview() { preview?.close(); preview = nil }

    // MARK: 用系统应用打开

    /// 设置页展示用：已登记的后缀（升序）。
    var openWithExtensions: [String] { OpenWithRules.sortedExtensions(openWithDefaults) }

    /// 解析此文件已登记且目前可定位的默认应用。
    /// 入参：file（FileEntry）按name后缀查规则。
    /// 返回值：ExternalApplication?，未登记/格式无效/应用缺失nil。
    /// 异常：不抛错。
    /// 注意事项：nil本身不决定回退系统默认还是内置预览，由上层入口选择。
    func defaultOpenApplication(for file: FileEntry) -> ExternalApplication? {
        defaultApplicationResolution(for: file).application
    }

    /// 判断某登记后缀对应的应用当前能否定位。
    /// 入参：ext（String）为已规范化的规则键。
    /// 返回值：Bool，精确键存在且bundleID可解析为true。
    /// 异常：不抛错。
    /// 注意事项：此入口不规范化ext，不证明应用能成功打开任意目标文件。
    func isApplicationInstalled(forExtension ext: String) -> Bool {
        guard let bundleID = openWithDefaults[ext] else { return false }
        return ExternalApplication.resolve(bundleID: bundleID) != nil
    }

    /// 按登记应用或系统默认应用打开一份新解密副本。
    /// 入参：file为内部文件元数据。
    /// 返回值：Void，canBrowse为false直接忽略。
    /// 异常：实际解密/启动错误在子流程显示。
    /// 注意事项：缺登记或应用卸载会使用系统默认；副本不自动写回容器。
    func openExternally(_ file: FileEntry) async {
        guard canBrowse else { return }
        let resolution = defaultApplicationResolution(for: file)
        // 登记过但应用已卸载：提示后仍用系统默认应用打开。
        if resolution.configured && resolution.application == nil {
            notice = "未找到登记的应用，已改用系统默认应用打开"
        }
        await exportAndOpen(file, application: resolution.application)
    }

    /// 选择本次打开应用，必要时登记默认，再解密外开。
    /// 入参：file为内部文件元数据。
    /// 返回值：Void，面板或确认取消直接返回。
    /// 异常：子流程处理错误。
    /// 注意事项：选择器同步阻塞；“总是使用”保存失败不必阻止本次打开，不能当成登记成功证明。
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

    /// 把当前预览已有明文交给登记或系统默认应用。
    /// 入参：无，使用preview资产。
    /// 返回值：Void，无资产则返回。
    /// 异常：未解密或启动失败由子流程设置error。
    /// 注意事项：不重新解密；交出目录后关闭预览不再删除该目录，清理责任转跟踪器。
    func openPreviewExternally() async {
        guard let asset = preview else { return }
        let resolution = defaultApplicationResolution(for: asset.file)
        if resolution.configured && resolution.application == nil {
            notice = "未找到登记的应用，已改用系统默认应用打开"
        }
        handOffPreview(asset, application: resolution.application)
    }

    /// 为当前已解密预览选择应用并交出明文。
    /// 入参：无。
    /// 返回值：Void，无预览或取消时返回。
    /// 异常：子流程处理错误。
    /// 注意事项：使用当前asset，await保存默认规则期间可能界面已换预览；未统一generation复核。
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

    /// 通过后缀输入和应用面板添加默认打开规则。
    /// 入参：无。
    /// 返回值：Void，任一面板取消即结束。
    /// 异常：非法后缀设error，保存错误由通用更新入口处理。
    /// 注意事项：只登记bundleID，不解密文件、不启动应用。
    func addDefaultOpenApplication() async {
        guard let ext = promptOpenWithExtension() else { return }
        guard
            let application = ExternalApplication.choose(
                title: "选择 .\(ext) 文件默认使用的应用",
                message: "点击该后缀的文件时会先解密到临时文件，再交给所选应用打开；登记保存在 app.db，可在此修改或移除。")
        else { return }
        await setDefaultOpenApplication(application, forExtension: ext)
    }

    /// 为已有后缀选择另一默认应用。
    /// 入参：ext（String）为后缀规则键。
    /// 返回值：Void，取消无变化。
    /// 异常：保存错误由通用入口记录。
    /// 注意事项：后缀最终经setDefaultOpenApplication规范化；不是系统全局默认应用设置，仅本应用偏好。
    func changeDefaultOpenApplication(forExtension ext: String) async {
        guard
            let application = ExternalApplication.choose(
                title: "选择 .\(ext) 文件默认使用的应用",
                message: "点击该后缀的文件时会先解密到临时文件，再交给所选应用打开；登记保存在 app.db，可在此修改或移除。")
        else { return }
        await setDefaultOpenApplication(application, forExtension: ext)
    }

    /// 规范化后缀并保存应用bundleID到本应用偏好。
    /// 入参：application（ExternalApplication）为选择应用；ext（String）为原后缀输入。
    /// 返回值：Void，规范化失败静默返回；保存后内存值匹配时设反馈。
    /// 异常：保存错误在updateOpenWithDefaults捕获。
    /// 注意事项：无Bool表示持久化成功；反馈判断基于当前内存，可能本来就相同或被并发修改。
    func setDefaultOpenApplication(_ application: ExternalApplication, forExtension ext: String)
        async
    {
        guard let ext = OpenWithRules.normalizeUserInput(ext) else { return }
        await updateOpenWithDefaults { $0[ext] = application.bundleID }
        if openWithDefaults[ext] == application.bundleID {
            settingsFeedback = ".\(ext) 已改用「\(application.name)」打开"
        }
    }

    /// 移除本应用的精确后缀规则，让后续入口采用其回退行为。
    /// 入参：ext（String）为规则键。
    /// 返回值：Void，更新后键不存在时设反馈。
    /// 异常：保存错误在通用入口处理。
    /// 注意事项：此函数不规范化ext；没有规则后默认点击通常用内置预览，显式外开才系统默认，不能一律称恢复系统默认。
    func removeDefaultOpenApplication(forExtension ext: String) async {
        await updateOpenWithDefaults { $0.removeValue(forKey: ext) }
        if openWithDefaults[ext] == nil {
            settingsFeedback = ".\(ext) 已恢复系统默认应用"
        }
    }

    /// 先更新规则内存副本，再整体保存external_open_defaults。
    /// 入参：change（(inout [String:String])->Void）修改后缀到bundleID映射。
    /// 返回值：Void，失败回退调用开始时的previous。
    /// 异常：捕获服务错误记录并设error。
    /// 注意事项：不走busy，多个异步保存可交错；整体旧快照回退可能覆盖较新UI，磁盘最后写顺序也不由该闭包保证。
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

    /// 同时回答文件是否登记过默认应用以及当前能否定位它。
    /// 入参：file（FileEntry）从name提取并规范化后缀。
    /// 返回值：(application:ExternalApplication?, configured:Bool)，无规则(nil,false)，有规则但缺应用(nil,true)。
    /// 异常：不抛错。
    /// 注意事项：规则缓存来自客户端内存，不在此重新读app.db；bundleID定位不保证启动成功。
    private func defaultApplicationResolution(for file: FileEntry)
        -> (application: ExternalApplication?, configured: Bool)
    {
        guard let ext = OpenWithRules.normalizeExtension(forFileName: file.name),
            let bundleID = openWithDefaults[ext]
        else { return (nil, false) }
        return (ExternalApplication.resolve(bundleID: bundleID), true)
    }

    /// 解密到独立外开临时目录，收紧权限并交系统应用启动。
    /// 入参：file为内部元数据；application（ExternalApplication?）nil使用系统默认。
    /// 返回值：Void，解密成功后登记目录并异步launch。
    /// 异常：预检不足设error；解密/权限/文件错误捕获后尽力删除目录。
    /// 注意事项：允许导入期间运行；忽略exportWithStatus不稳定提示。函数完成不等于外部应用成功打开，结果在launch回调处理；外部修改不自动写回容器。
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
            // 外开副本使用独立前缀目录，交由 ExternalOpenTracker 跟踪生命周期。
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
                // 明文文件权限收紧为仅当前用户可读写。
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

    /// 把已有预览目录移交外开跟踪后请求系统启动。
    /// 入参：asset（PreviewAsset）为已解密资产；application可选默认应用。
    /// 返回值：Void，缺contentURL设error后返回。
    /// 异常：不抛错。
    /// 注意事项：detach不检查目录仍存在；不复制新文件，同一副本由外部应用使用，清理不再由预览close负责。
    private func handOffPreview(_ asset: PreviewAsset, application: ExternalApplication?) {
        guard let url = asset.contentURL else {
            self.error = "文件尚未解密完成，请稍后再试。"
            return
        }
        let folder = asset.detachForExternalOpen()
        if let folder { ExternalOpenTracker.shared.register(folder) }
        launch(url, application: application, folder: folder)
    }

    /// 请求NSWorkspace打开明文，并在回调中处理目录与进程关联。
    /// 入参：url（URL）为目标明文；application指定应用或nil系统默认；folder（URL?）为需要跟踪回收的目录。
    /// 返回值：Void，启动异步，成功/失败由回调Task更新UI。
    /// 异常：回调错误记录并discard目录。
    /// 注意事项：未捕获generation，迟到回调仍可更新notice/error或重新关联目录；进程ID可能缺失，跟踪不证明外部副本已全部清理。
    private func launch(_ url: URL, application: ExternalApplication?, folder: URL?) {
        let completion: @Sendable (NSRunningApplication?, (any Error)?) -> Void = {
            running, error in
            Task { @MainActor in
                if let error {
                    // 打开失败：立即回收明文副本并展示错误。
                    AppLog.capture(error)
                    self.error =
                        "无法用「\(application?.name ?? "系统默认应用")」打开文件：\(error.localizedDescription)"
                    ExternalOpenTracker.shared.discard(folder)
                } else {
                    self.notice =
                        application.map { "已用「\($0.name)」打开" } ?? "已用系统默认应用打开"
                    // 打开成功：把目录绑定到实际进程，应用退出时回收。
                    ExternalOpenTracker.shared.associate(
                        folder: folder, process: running?.processIdentifier)
                }
            }
        }
        let configuration = NSWorkspace.OpenConfiguration()
        if let application {
            // 指定应用打开。
            NSWorkspace.shared.open(
                [url], withApplicationAt: application.url, configuration: configuration,
                completionHandler: completion)
        } else {
            // 系统默认应用打开。
            NSWorkspace.shared.open(url, configuration: configuration, completionHandler: completion)
        }
    }

    /// 在解密前询问本次打开、记默认或取消。
    /// 入参：application为所选应用；file为内部文件元数据。
    /// 返回值：Bool，同意本次/总是为true，取消false。
    /// 异常：不抛错；保存默认失败由内部记录。
    /// 注意事项：“总是使用”await保存后无论是否持久化成功仍true并显示notice；无有效后缀只允许本次或取消。
    private func confirmOpenWith(_ application: ExternalApplication, for file: FileEntry) async
        -> Bool
    {
        let ext = OpenWithRules.normalizeExtension(forFileName: file.name)
        let hint = "打开时会先把文件解密到临时文件，锁定或退出后自动清理。"
        let alert = NSAlert()
        alert.messageText = "用「\(application.name)」打开「\(file.leafName)」？"
        if let ext {
            // 有后缀：提供「仅此次 / 总是使用（登记默认应用）」。
            alert.informativeText = hint + "「总是使用」会把它登记为 .\(ext) 的默认应用，之后点击该后缀的文件就直接用这个应用打开。"
            alert.addButton(withTitle: "仅此次")
            alert.addButton(withTitle: "总是使用")
        } else {
            // 无后缀：只能「打开」或「取消」。
            alert.informativeText = hint
            alert.addButton(withTitle: "打开")
        }
        alert.addButton(withTitle: "取消")
        switch alert.runModal() {
        case .alertFirstButtonReturn:
            // 仅此次打开。
            return true
        case .alertSecondButtonReturn where ext != nil:
            // 总是使用：登记为该后缀的默认应用后再打开。
            guard let ext else { return true }
            await setDefaultOpenApplication(application, forExtension: ext)
            notice = "已把「\(application.name)」设为 .\(ext) 的默认应用"
            return true
        default:
            // 取消：不解密、不启动应用。
            return false
        }
    }

    /// 用原生弹窗取得并规范化规则后缀。
    /// 入参：无。
    /// 返回值：String?，确认且有效返回规范后缀，取消/无效nil，无效另设error。
    /// 异常：不抛错。
    /// 注意事项：具体1至16字母数字规则由OpenWithRules执行；这里只收输入，不保存。
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

    /// 请求跟踪器清理当前登记的外开临时副本。
    /// 入参：无。
    /// 返回值：Void。
    /// 异常：不通过此接口抛错，跟踪器自行记录。
    /// 注意事项：不关闭外部应用或撤回它已复制的数据；文件删除尽力完成。
    func cleanupExternalOpens() {
        ExternalOpenTracker.shared.cleanupAll()
    }

    /// 确保应用临时副本根存在后请求访达打开。
    /// 入参：无。
    /// 返回值：Void，目录创建失败设error。
    /// 异常：文件错误捕获记录；NSWorkspace异步打开结果未在此检查。
    /// 注意事项：不解密文件、不延长预览或外开副本寿命；根可包含敏感明文。
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

    /// 通过保存面板选择单文件输出，再走共享流式导出。
    /// 入参：file（FileEntry）用于内部选择及建议leafName。
    /// 返回值：Void，忙/面板取消跳过。
    /// 异常：后续错误由exportPaths处理。
    /// 注意事项：面板选择不代表已生成文件；同名目标仍走流水线审核规则。
    func exportFile(_ file: FileEntry) async {
        guard !isBusy else { return }
        let panel = NSSavePanel()
        panel.title = "导出文件"
        panel.nameFieldStringValue = file.leafName
        panel.canCreateDirectories = true
        guard panel.runModal() == .OK, let url = panel.url else { return }
        await exportPaths([file.name], to: url)
    }

    /// 选择外部父目录并导出整个内部文件夹。
    /// 入参：path（String）为内部目录路径。
    /// 返回值：Void，忙/面板取消跳过。
    /// 异常：后续错误由共享入口处理。
    /// 注意事项：按流水线保留顶层目录名称，空根路径语义另由服务验证。
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

    /// 配套冲突/进度轮询执行流式导出，并反馈完成/跳过数。
    /// 入参：names（[String]）每项内部文件/目录；url（URL）为精确文件或多项外部目录目标。
    /// 返回值：Void，已busy被perform忽略，取消可温和提示。
    /// 异常：其他错误由perform捕获。
    /// 注意事项：申请安全作用域返回false仍继续；defer只结束轮询/审核，已发布文件不会回滚。完成数包含跳过，实际导出数需相减。
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
                // 用户主动中止：提示已保留已导出的文件，不算错误。
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

    /// 拖拽导出用的会话代号：拖拽开始时的代号与导出时不一致则拒绝执行。
    var dragExportGeneration: UInt64 { generation }

    /// 在拖放接收方确定目标时解密单个文件。
    /// 入参：file为拖拽元数据；url为最终目标；ticket（UInt64）为开始拖拽时generation。
    /// 返回值：Void，等待单文件导出返回。
    /// 异常：入口会话过期/未开/忙时抛ClientError；其他导出/协议错误继续抛。
    /// 注意事项：只入口比较ticket，await后未重检；目标不覆盖，无交互冲突轮询，也不在此取得安全作用域。
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

    /// 选择保存位置并备份当前会话容器。
    /// 入参：无，使用当前container名称生成默认文件名。
    /// 返回值：Void，忙/无容器/面板取消跳过。
    /// 异常：错误由perform和进度包装处理。
    /// 注意事项：名只替换“/”和“:”，已带.veil不重复追加；备份服务拒绝覆盖，发布后统计/同步失败可能仍有包。
    func exportPackage() async {
        guard !isBusy, let container else { return }
        let panel = NSSavePanel()
        panel.title = "导出加密备份"
        panel.allowedContentTypes = [UTType(filenameExtension: "veil") ?? .data]
        // 文件名清洗：路径分隔符换成下划线，并确保带 .veil 后缀。
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

    /// 删除弹窗确认的内部条目并刷新浏览位置。
    /// 入参：confirmedTarget（BrowserEntry?）优先，nil取pendingDelete。
    /// 返回值：Void，无目标返回；先清pendingDelete，再通过perform提交。
    /// 异常：服务错误由perform处理。
    /// 注意事项：忙时perform可能忽略但确认目标已清；成功后关闭当前预览，删当前目录或祖先则回根，物理密文回收仍是尽力。
    func confirmDelete(_ confirmedTarget: BrowserEntry? = nil) async {
        guard let target = confirmedTarget ?? pendingDelete else { return }
        pendingDelete = nil
        await perform { ticket in
            let view: ContainerSnapshot = try await self.client.json(NativeRequest(operation: "remove_path", name: target.path))
            guard ticket == self.generation else { return }
            self.closePreview()
            self.container = view
            // 当前目录被删（或在其内部）：退回根目录避免停留在失效路径。
            if self.directory == target.path || self.directory.hasPrefix(target.path + "/") { self.directory = "" }
            self.notice = "已删除"
        }
    }
}

/// 一次预览会话的资产：解密后的临时文件、展示状态与清理责任。
@MainActor
final class PreviewAsset: ObservableObject, Identifiable {
    /// 被预览的文件记录。
    let file: FileEntry
    /// 列表唯一标识，复用文件 id；nonisolated 允许在非隔离上下文读取。
    nonisolated var id: String { file.id }
    /// 解密结果的临时文件 URL；nil 表示尚未解密完成。
    @Published var contentURL: URL?
    /// 预览失败时的展示文案。
    @Published var error: String?
    /// 解密内容是否不稳定（读取期间可能变化）。
    @Published var unstable = false
    /// 预览临时目录（关闭/销毁时删除）；交出外开后清空。
    private var temporaryDirectory: URL?

    /// 建立一次文件预览资产，稍后再安装解密文件。
    /// 入参：file（FileEntry）为预览元数据。
    /// 返回值：PreviewAsset，contentURL未设置。
    /// 异常：不抛错。
    /// 注意事项：id直接复用文件ID，不是每次独立UUID；同文件多次预览需调用方管理引用。
    init(file: FileEntry) { self.file = file }

    /// 为已解密文件设置0600权限，并接管目录清理责任。
    /// 入参：url（URL）为明文文件；folder（URL）为拥有目录。
    /// 返回值：Void，成功写temporaryDirectory和contentURL。
    /// 异常：权限设置失败close旧资产状态后抛AppLog.recorded错误。
    /// 注意事项：folder在权限成功后才保存，因此失败时close不会自动删除新传入folder，需外层兜底；不验证url位于folder中。
    func install(_ url: URL, folder: URL) throws {
        do {
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
            temporaryDirectory = folder
            contentURL = url
        } catch { close(); throw AppLog.recorded(error) }
    }

    /// 把暂存目录的清理责任交给外开跟踪，避免关闭预览立即删文件。
    /// 入参：无。
    /// 返回值：URL?，存在目录则摘除后返回，否则nil。
    /// 异常：不抛错。
    /// 注意事项：不清contentURL、不自动注册外部进程；调用方必须接管返回目录并安排后续清理。
    func detachForExternalOpen() -> URL? {
        guard let folder = temporaryDirectory else { return nil }
        temporaryDirectory = nil
        return folder
    }

    /// 结束资产对临时目录的拥有并隐藏文件URL。
    /// 入参：无。
    /// 返回值：Void，删除失败也清除temporaryDirectory引用。
    /// 异常：删除错误经AppLog.attempt记录，不抛。
    /// 注意事项：不清error/unstable字段，不保证目录删成或安全擦除；其他引用仍可持URL。
    func close() {
        contentURL = nil
        if let temporaryDirectory { AppLog.attempt({ try FileManager.default.removeItem(at: temporaryDirectory) }) }
        temporaryDirectory = nil
    }

    /// 在资产最后释放时尽力回收仍拥有的预览目录。
    /// 入参：无显式参数，使用temporaryDirectory。
    /// 返回值：无。
    /// 异常：删除错误记录后忽略。
    /// 注意事项：外开已detach时不会删除；不保证异常终止后运行此析构。
    deinit { if let temporaryDirectory { AppLog.attempt({ try FileManager.default.removeItem(at: temporaryDirectory) }) } }
}
