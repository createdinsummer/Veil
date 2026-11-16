/// ============================================================================
/// Veil iOS 客户端 · 状态管理中枢（Store）
///
/// 文件作用：集中保存界面上所有共享业务状态（空间列表、当前容器、目录浏览位置、
///   导入导出进度、预览内容等），并编排所有需要调用 Rust 服务的业务动作。
///   状态管理 = 把分散的界面数据集中到一处保存并发布变化的机制。
/// 所属模块：iOS 客户端状态层，位于 UI 与 ServiceClient（Rust 桥接调用）之间。
/// 文件内核心类型清单：
///   - Store              状态中枢（@MainActor，所有状态变化发生在 UI 主线程）
///   - PreviewItem        预览条目（文件信息 + 解密后临时文件位置）
///   - ExportItem         导出条目（交给系统分享面板的文件内容）
///   - ExportDocument     系统 FileDocument 协议的导出文档包装
///   - PhotoImportFile    相册导入用的 Transferable 文件复制
/// 依赖能力：SwiftUI（ObservableObject 发布机制）、PhotosUI（相册选择）、
///   CoreTransferable（相册文件复制）、UniformTypeIdentifiers / AVFoundation /
///   ImageIO（缩略图生成）。
/// 整体业务目标：持有“空间 → 容器 → 目录 / 文件列表”的浏览状态，协调
///   主要用户动作（busy检查+generation会话票据，部分后台操作绕过），并处理导入/导出冲突、
///   进度展示与预览临时文件的生命周期。
/// ============================================================================
import SwiftUI
import PhotosUI
import CoreTransferable
import UniformTypeIdentifiers
import AVFoundation
import ImageIO

/// 应用全局状态中枢：所有界面共享的业务状态集中在这里发布。
/// @MainActor 保证所有状态读写都在 UI 主线程执行，避免多线程同时改状态引发界面错乱。
@MainActor final class Store: ObservableObject {
    /// 本机登记的空间列表（来自服务的 list_spaces 响应），首页与空间菜单都从这里取数。
    @Published var spaces: [Space] = []
    /// 当前选中空间的 id；首页展示与多数服务调用都以它为作用域。
    @Published var selectedSpace = ""
    /// 已解锁容器的完整快照（目录树 + 文件列表 + 元数据）；为 nil 表示当前没有解锁的容器。
    @Published var container: ContainerSnapshot?
    /// 当前浏览的容器内目录路径（"" 表示根目录）。
    @Published var directory = ""
    /// 文件搜索关键字，实时过滤浏览列表。
    @Published var search = ""
    /// 浏览模式：按类型分类或按目录路径。
    @Published var mode: BrowseMode = .type
    /// 按类型浏览时选中的分类（nil 表示“全部”）。
    @Published var category: FileCategory?
    /// 文件列表布局：网格或列表。
    @Published var layout: FileLayout = .grid
    /// 冲突裁决页上展示的操作名称（“导入”或“导出”），由冲突轮询过程写入。
    @Published var conflictOperation = "导入"
    /// 长按“导出到文件夹”选中的条目；非 nil 时界面弹出目录选择器。
    @Published var exportSelection: BrowserEntry?
    /// 当前待用户裁决的冲突批次；非 nil 时界面弹出冲突处理页。
    @Published var importInspection: ImportInspection?
    /// 挂起的冲突裁决回调：冲突页把用户选择经它传回正在执行的导入 / 导出任务。
    private var importReview: CheckedContinuation<[ImportResolution]?, Never>?
    /// 最近一次操作失败的错误文案；非 nil 时界面弹出“操作未完成”。
    @Published var error: String?
    /// 最近一次操作成功的提示文案；非 nil 时界面弹出“操作完成”。
    @Published var notice: String?
    /// 导入 / 导出期间显示的状态文案（例如“正在导入文件”）。
    @Published var importStatus: String?
    /// 是否有耗时操作正在进行：true 时界面显示进度遮罩并禁用所有入口。
    @Published var busy = false
    /// 当前预览条目；切换到另一个预览时自动清理上一个预览遗留的临时文件。
    @Published var preview: PreviewItem? { didSet { if oldValue?.id != preview?.id { oldValue?.removeTemporaryFiles() } } }
    /// 当前待导出的条目；非 nil 时界面弹出系统文件导出面板。
    @Published var export: ExportItem?
    /// 图片缩略图缓存：key 为“文件 id:版本”，value 为解码后的缩略图；只允许本类写入。
    @Published private(set) var thumbnails: [String: UIImage] = [:]
    /// 文本缩略图缓存：key 同上，value 为文本开头片段；只允许本类写入。
    @Published private(set) var textThumbnails: [String: String] = [:]
    /// 正在加载缩略图的文件 key 集合，避免同一文件并发重复加载。
    private var thumbnailTasks: Set<String> = []
    /// 缩略图加载失败的文件 key 集合，失败后不再重试，避免反复打服务。
    private var thumbnailFailures: Set<String> = []
    /// Rust 桥接服务的封装客户端（actor），所有服务调用都经它发出。
    private let client = ServiceClient()
    /// 锁定/清理时递增的会话票据，仅显式比较generation的结果会停止写回。
    private var generation = 0
    /// 当前生效的空间：优先选中的空间，其次列表第一个；列表为空时为 nil。
    var space: Space? { spaces.first { $0.id == selectedSpace } ?? spaces.first }
    /// 当前浏览模式下应展示的条目（文件夹 + 文件），由快照与浏览条件实时计算。
    var entries: [BrowserEntry] { container.map { browserEntries(container: $0, mode: mode, directory: directory, categories: category.map { [$0] } ?? [], search: search) } ?? [] }
    /// 为大多数用户动作设置busy状态并统一处理错误。
    /// 入参：action（(Int) async throws -> Void）接收开始时generation票据。
    /// 返回值：Void；已busy则直接忽略动作，否则等待动作结束。
    /// 异常：捕获并记录action错误，仅票据仍相同时设置error，不向外抛。
    /// 注意事项：MainActor可在await处重入；run不自动阻止闭包所有旧结果写回，闭包须自己查票据。defer无条件清busy，不取消底层Rust任务。
    func run(_ action: (Int) async throws -> Void) async {
        guard !busy else { return }
        busy = true; error = nil
        let ticket = generation
        defer { busy = false }
        do { try await action(ticket) } catch { AppLog.capture(error); if ticket == generation { self.error = error.localizedDescription } }
    }
    /// 清理历史预览临时项并读取空间列表。
    /// 入参：无。
    /// 返回值：Void，忙时run直接跳过。
    /// 异常：内部错误由run记录并显示。
    /// 注意事项：清理/重新读取不检查票据，reload可在await期间被锁定后仍更新空间列表。
    func load() async { await run { _ in
        self.removeStalePreviews()
        try await self.reload()
    } }
    /// 读取最新本机空间列表，并修正失效的当前选择。
    /// 入参：无。
    /// 返回值：Void，spaces被新列表替换，选中ID不在列表时取默认/首项/空。
    /// 异常：服务或解码失败向上抛。
    /// 注意事项：不按generation过滤；主线程await期间仍可执行其他操作，结果可能覆盖较新的空间视图。
    private func reload() async throws {
        spaces = try await client.json(NativeRequest(operation: "list_spaces"))
        if !spaces.contains(where: { $0.id == selectedSpace }) { selectedSpace = spaces.first(where: { $0.isDefault })?.id ?? spaces.first?.id ?? "" }
    }
    /// 在非busy时刷新登记可用性，必要时锁定不再可见的已登记容器。
    /// 入参：无。
    /// 返回值：Void，刷新失败记录后结束。
    /// 异常：不向外抛错。
    /// 注意事项：只在开头检查busy，不占busy；空间离线时容器摘要也可为空，不能把条目消失一律断定为物理删除。
    func refreshSpaceAvailability() async {
        guard !busy else { return }
        // 记录刷新前“当前解锁容器是否已登记在某个空间里”，用于判断刷新后是否失联。
        let wasRegisteredContainer = container.map { opened in
            spaces.contains(where: { $0.containers.contains(where: { $0.id == opened.id }) })
        } ?? false
        do {
            try await reload()
            // 展示列表不再含原容器时锁定；空间离线也可能使摘要为空，不能据此认定目录已删除。
            if wasRegisteredContainer, let container,
               !spaces.contains(where: { $0.containers.contains(where: { $0.id == container.id }) }) {
                await lock()
            }
        } catch { AppLog.capture(error); return }
    }
    /// 先清界面敏感状态，再请求锁定和删除应用本地数据。
    /// 入参：无。
    /// 返回值：Void，忙时忽略；成功重读空间并提示，失败尽力重读并报部分清理。
    /// 异常：捕获并记录服务/删除/重读错误。
    /// 注意事项：generation使有票据检查的结果过期，不强制取消所有缩略图/后台任务；Data/UIImage/String移除引用不等于安全零化。清理失败不恢复已删内容。
    func clearLocalData() async {
        guard !busy else { return }
        // 递增票据，使后续显式比较generation的旧任务不再写回；未检查的轮询/刷新仍需各自处理。
        generation += 1
        // 立即清空界面上的一切敏感状态。
        container = nil; preview = nil; export = nil; exportSelection = nil; directory = ""; search = ""; category = nil
        thumbnails.removeAll(); textThumbnails.removeAll(); thumbnailTasks.removeAll(); thumbnailFailures.removeAll()
        busy = true; error = nil
        defer { busy = false }
        do {
            try await client.clearLocalData()
            spaces = []; selectedSpace = ""
            try await reload()
            notice = "本机数据已清理"
        } catch { AppLog.capture(error);
            // 清理可能部分完成，重新读取可用状态并如实报告错误。
            spaces = []; selectedSpace = ""
            try? await reload()
            self.error = "清理未完全完成：\(error.localizedDescription)"
        }
    }
    /// 请求新建本机空间并尝试选中新建名称对应项。
    /// 入参：name（String）为名称，最终合法性由服务判断。
    /// 返回值：Void，run忙时不执行，票据有效才更新列表/选中ID。
    /// 异常：服务错误由run处理。
    /// 注意事项：按名称find选中，不以独立新ID回包；若票据过期，空间仍可能已经创建。
    func createSpace(_ name: String) async { await run { ticket in
        let items: [Space] = try await self.client.json(NativeRequest(operation: "create_space", name: name))
        // 会话未失效才写回；优先选中刚创建的空间。
        guard ticket == self.generation else { return }; self.spaces = items; self.selectedSpace = items.first(where: { $0.name == name })?.id ?? self.selectedSpace
    } }
    /// 删除指定登记容器，并在成功响应后清理对应主界面。
    /// 入参：item（ContainerSummary）含要删除的稳定ID。
    /// 返回值：Bool，响应有效且票据未变并写回时true；忙/错误/过期均false。
    /// 异常：内部错误由run处理不抛出。
    /// 注意事项：false不证明磁盘/登记未变，服务可部分完成；只清匹配当前容器的界面缓存。
    func deleteContainer(_ item: ContainerSummary) async -> Bool {
        var deleted = false
        await run { ticket in
            let items: [Space] = try await self.client.json(
                NativeRequest(operation: "delete_container", id: item.id)
            )
            guard ticket == self.generation else { return }
            // 删除的正是当前解锁容器：清空浏览、预览、缩略图等一切残留。
            if self.container?.id == item.id {
                self.container = nil; self.preview = nil; self.export = nil; self.exportSelection = nil
                self.directory = ""; self.search = ""; self.category = nil
                self.thumbnails.removeAll(); self.textThumbnails.removeAll()
                self.thumbnailTasks.removeAll(); self.thumbnailFailures.removeAll()
            }
            self.spaces = items
            // 若当前选中空间被删掉，回退到默认空间或第一个空间。
            if !items.contains(where: { $0.id == self.selectedSpace }) {
                self.selectedSpace = items.first(where: { $0.isDefault })?.id ?? items.first?.id ?? ""
            }
            self.notice = "容器已删除"
            deleted = true
        }
        return deleted
    }
    /// 根据展示可用状态选择物理删除空间或仅移除记录。
    /// 入参：space（Space）为已选空间快照，id定位、isAvailable选策略。
    /// 返回值：Void，成功响应且票据有效才更新界面和提示。
    /// 异常：错误由run捕获。
    /// 注意事项：isAvailable可能已变化，服务重新检查；删除成功前界面旧会话仍保留，若服务锁定后失败，需后续刷新纠正界面。
    func deleteSpace(_ space: Space) async {
        await run { ticket in
            let openContainer = self.container
            // 判断当前解锁容器是否属于该空间：按登记关系或磁盘路径隶属两种方式核对。
            let belongsToSpace = space.containers.contains(where: { $0.id == openContainer?.id })
                || openContainer.map { URL(fileURLWithPath: $0.path).deletingLastPathComponent().path == space.path } == true
            // 可用空间删磁盘数据；不可用空间只删 app.db 里的登记记录。
            let items: [Space] = try await self.client.json(
                NativeRequest(operation: space.isAvailable ? "delete_space" : "delete_space_record", id: space.id)
            )
            guard ticket == self.generation else { return }
            // 解锁中的容器随空间一起没了：清空全部浏览状态。
            if belongsToSpace {
                self.container = nil; self.preview = nil; self.export = nil; self.exportSelection = nil
                self.directory = ""; self.search = ""; self.category = nil
                self.thumbnails.removeAll(); self.textThumbnails.removeAll()
                self.thumbnailTasks.removeAll(); self.thumbnailFailures.removeAll()
            }
            self.spaces = items
            // 当前选中空间被删则回退默认 / 第一个空间。
            if !items.contains(where: { $0.id == self.selectedSpace }) {
                self.selectedSpace = items.first(where: { $0.isDefault })?.id ?? items.first?.id ?? ""
            }
            // 三种场景给出不同的成功提示。
            self.notice = space.isAvailable
                ? (space.isDefault ? "默认空间已删除并重新建立" : "空间已删除")
                : "空间记录已删除，磁盘文件仍保留"
        }
    }
    /// 按表单场景创建/解锁/还原，并把服务视图设为当前容器。
    /// 入参：form（ContainerForm）为创建/打开/还原；name为创建名称；password为当前操作密码。
    /// 返回值：Bool，已将结果写container并设置success为true才true；后续reload失败仍可返回true。
    /// 异常：预检不可用空间直接设验证错误；其他错误由run处理。
    /// 注意事项：票据过期时补发lock，但失败会被run捕获，不能保证总能回锁；打开成功只重置directory，不自动清全部旧筛选/缩略图。
    func open(form: ContainerForm, name: String, password: String) async -> Bool {
        // 找出本次操作作用的空间 id。
        let spaceID: String?
        switch form {
        case .create(let space): spaceID = space
        case .importPackage(_, let space): spaceID = space
        case .open: spaceID = nil
        }
        // 创建 / 导入必须落到可用空间；解锁已有容器不受空间可用性限制。
        if let spaceID, !spaces.contains(where: { $0.id == spaceID && $0.isAvailable }) {
            error = AppLog.validation("空间当前不可用，请重新连接外置磁盘后再试")
            return false
        }
        var success = false
        await run { ticket in
            let request: NativeRequest
            // 按场景拼装对应的服务请求。
            switch form {
            case .create(let space): request = NativeRequest(operation: "create_container", space: space, name: name, password: password)
            case .open(let path, _): request = NativeRequest(operation: "open_container", password: password, path: path)
            case .importPackage(let path, let space): request = NativeRequest(operation: "import_package", space: space, password: password, path: path)
            }
            let result: ContainerSnapshot = try await self.client.json(request)
            // 服务端已解锁容器但界面会话已失效（例如中途锁定）：回锁服务端，不留未锁定会话。
            guard ticket == self.generation else { try await self.client.execute(NativeRequest(operation: "lock")); return }
            self.container = result; self.directory = ""; success = true
            try await self.reload()
        }
        return success
    }
    /// 立即清界面主会话、取消共享传输并向Rust请求锁定。
    /// 入参：无。
    /// 返回值：Void，不受busy入口限制。
    /// 异常：Rust锁定失败记录并设error，不恢复原界面。
    /// 注意事项：generation递增仅约束显式检查者，底层取消为协作式；在途密码副本和临时文件可能晚于此方法清理，不能声称全部旧任务强制失效。
    func lock() async {
        // 递增代次并清界面，已持旧ticket且主动比较的结果会被忽略，不强制停止所有后台工作。
        generation += 1; _ = client.cancelTransfer(); completeImportReview(nil); container = nil; preview = nil; export = nil; exportSelection = nil; directory = ""; search = ""; category = nil
        thumbnails.removeAll(); textThumbnails.removeAll(); thumbnailTasks.removeAll(); thumbnailFailures.removeAll()
        // 等待Rust清主会话；不撤回在途密码副本。失败记录并显示，界面保持清空。
        do { try await client.execute(NativeRequest(operation: "lock")) } catch { AppLog.capture(error); self.error = error.localizedDescription }
    }
    /// 冲突页顶部的进度说明文字：已完成数 / 总数与百分比，扫描未完成时注明总数仍在增长。
    var importProgressDescription: String? {
        guard let progress = importProgress, progress.total_files > 0 else { return nil }
        return "已完成 \(progress.completed_files) / \(progress.total_files) · \(Int(min(1, Double(progress.completed_files) / Double(progress.total_files))*100))%" + (progress.scan_complete ? " · 遍历完成" : " · 总数持续增加")
    }

    /// 结束当前冲突等待并恢复挂起任务。
    /// 入参：resolutions（[ImportResolution]?）每项目标/动作，nil为中止；id（String?）有值时须匹配当前批次。
    /// 返回值：Void；id不匹配直接忽略，匹配时先清continuation/inspection再resume一次。
    /// 异常：不抛错。
    /// 注意事项：无id时可结束任何当前批次；只恢复Swift等待，不在此向Rust提交决议或取消。
    func completeImportReview(_ resolutions: [ImportResolution]?, id: String? = nil) {
        // 携带 id 时只收对应批次；不匹配说明冲突页已被新批次替换，旧裁决作废。
        if let id, importInspection?.id != id { return }
        let continuation = importReview
        importReview = nil
        importInspection = nil
        continuation?.resume(returning: resolutions)
    }

    /// 根据是否正等用户决定选择结束审核或直接请求Rust取消。
    /// 入参：无。
    /// 返回值：Void，直接取消被首次受理时显示正在取消。
    /// 异常：不抛错。
    /// 注意事项：有continuation时先恢复nil，由轮询任务再发取消；方法返回不代表后台线程结束。
    func cancelImport() {
        if importReview != nil { completeImportReview(nil) }
        else if client.cancelTransfer() { importStatus = "正在取消" }
    }

    /// 服务端传输进度的最新快照（导入 / 导出通用），由轮询任务周期性刷新。
    @Published private(set) var importProgress: ImportTransferProgress?

    /// 轮询传输冲突，挂起等待UI决议后提交给Rust继续处理。
    /// 入参：ticket（Int）为会话代次；exporting（Bool）决定导出/导入操作名，默认导入。
    /// 返回值：Void，任务取消、代次变动或错误结束循环。
    /// 异常：服务错误捕获记录，有效代次时设error并请求取消。
    /// 注意事项：sleep为150ms检查间隔而非响应上限；continuation需UI或取消处理resume，Task取消本身不直接终止Rust调用；一批在等时其他无冲突文件仍可继续。
    private func pollImportConflicts(ticket: Int, exporting: Bool = false) async {
        // 只要任务未取消、会话未失效，就每 150ms 轮询一次。
        while !Task.isCancelled, ticket == generation {
            do {
                // 按方向选择对应的轮询操作。
                let batch: ImportInspection? = try await client.json(NativeRequest(operation: exporting ? "poll_export_conflicts" : "poll_import_conflicts"))
                if let batch {
                    // 服务端返回冲突批次：先把批次挂到界面上，再用 continuation 挂起等待用户选择。
                    guard ticket == generation, !Task.isCancelled else { return }
                    self.conflictOperation = exporting ? "导出" : "导入"
                    let resolutions: [ImportResolution]? = await withTaskCancellationHandler {
                        await withCheckedContinuation { continuation in
                            importReview = continuation
                            importInspection = batch
                        }
                    } onCancel: { Task { @MainActor in self.completeImportReview(nil, id: batch.id) } }
                    // 用户中止（nil）或会话已失效：取消传输并退出循环。
                    guard let resolutions, ticket == generation, !Task.isCancelled else { _ = client.cancelTransfer(); return }
                    // 把裁决结果提交给服务端继续传输。
                    try await client.execute(NativeRequest(operation: exporting ? "resolve_export_conflicts" : "resolve_import_conflicts", id: batch.id, resolutions: resolutions))
                }
            } catch { AppLog.capture(error);
                // 轮询失败即中止本次传输，并把错误反馈给用户。
                if ticket == generation, !Task.isCancelled { self.error = error.localizedDescription; _ = client.cancelTransfer() }
                return
            }
            try? await Task.sleep(for: .milliseconds(150))
        }
    }

    /// 同时轮询冲突/进度并等待一次流式导入完成。
    /// 入参：urls（[URL]）每项本地来源；ticket为本次会话代次。
    /// 返回值：Void，有效票据下更新container/notice，取消阶段可转为提示并正常返回。
    /// 异常：普通失败尽力刷新已提交视图后经AppLog.recorded抛出。
    /// 注意事项：目录参数在发请求时取当前mode/directory；轮询任务的进度赋值不查ticket，defer统一取消/清理。完成数含跳过，部分成功保留，Swift任务取消不自动停止detached Rust。
    private func importSources(_ urls: [URL], ticket: Int) async throws {
        importStatus = "正在导入文件"
        // 子任务 1：冲突轮询（期间遇到重名冲突会弹出裁决页）。
        let conflicts = Task { @MainActor in await self.pollImportConflicts(ticket: ticket) }
        // 子任务 2：进度轮询，每 150ms 取一次服务端进度快照。
        let progress = Task { @MainActor in
            while !Task.isCancelled {
                let snapshot = self.client.transferProgress()
                // 只展示导入方向的进度，导出方向的快照丢弃。
                self.importProgress = snapshot?.operation == "import" ? snapshot : nil
                try? await Task.sleep(for: .milliseconds(150))
            }
        }
        // 无论成败，结束时停掉两个轮询并清理冲突挂起状态与进度文案。
        defer { conflicts.cancel(); progress.cancel(); completeImportReview(nil); importStatus = nil; importProgress = nil }
        do {
            // 按类型浏览时不带目录参数，按路径浏览时导入到当前目录。
            let result: ContainerSnapshot = try await client.json(NativeRequest(operation: "stream_import_files", sources: urls.map(\.path), directory: mode == .path ? directory : ""))
            if ticket == generation {
                container = result
                // 成功后用最终进度生成“完成 / 跳过”提示。
                if let progress = client.transferProgress() { notice = "已完成 \(progress.completed_files) 个文件，跳过 \(progress.skipped_files) 个文件" }
            }
        } catch {
            // 失败时先尝试刷新容器快照，保留已导入的文件列表。
            if ticket == generation,
               let refreshed: ContainerSnapshot = try? await client.json(NativeRequest(operation: "refresh")), ticket == generation { container = refreshed }
            // 用户主动取消：给出温和的中止提示而不是错误弹窗。
            if ticket == generation, let progress = client.transferProgress(), progress.phase == "cancelled" {
                notice = "导入已中止，已完成 \(progress.completed_files) 个文件（其中跳过 \(progress.skipped_files) 个）"
                return
            }
            throw AppLog.recorded(error)
        }
    }

    /// 为文件选择器来源配对安全作用域访问申请与释放，然后导入全部URL。
    /// 入参：urls（[URL]）为本地/安全作用域来源。
    /// 返回值：Void，由run管理busy。
    /// 异常：服务/文件错误由run捕获。
    /// 注意事项：accessed仅记录startAccessing返回true的项以stop；实际传入的是urls全部，不会过滤申请失败项，返回false也不总表示无权限。
    func importFiles(_ urls: [URL]) async { await run { ticket in
        // 只收集需配对stop的成功申请项；下面仍导入完整urls，返回false不在这里过滤。
        let accessed = urls.filter { $0.startAccessingSecurityScopedResource() }
        defer { accessed.forEach { $0.stopAccessingSecurityScopedResource() } }
        try await self.importSources(urls, ticket: ticket)
    } }
    /// 先把相册来源复制为本地中转文件，再流式导入容器。
    /// 入参：items（[PhotosPickerItem]）每项一个照片/视频选择。
    /// 返回值：Void，有效票据下逐个暂存并导入。
    /// 异常：loadTransferable缺内容或复制/导入失败由run处理。
    /// 注意事项：staged每项持独立目录文件，defer尽力删除；失败前已复制的目录会清理，但系统或进程中止不保证清理完成。
    func importPhotos(_ items: [PhotosPickerItem]) async { await run { ticket in
        var staged: [PhotoImportFile] = []
        // 结束后统一删除本次复制的相册临时文件。
        defer { staged.forEach { item in AppLog.attempt({ try FileManager.default.removeItem(at: item.url.deletingLastPathComponent()) }) } }
        // 逐个把相册条目落盘到临时目录，失败即中止整次导入。
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
    /// 生成本次文件内容代数的缩略图缓存键。
    /// 入参：file（FileEntry）含稳定ID及版本链长度。
    /// 返回值：String为id:version。
    /// 异常：不抛错。
    /// 注意事项：链长度不是内容摘要；键不含容器ID/路径，缓存能否正确复用还依赖切换时清理策略。
    func thumbnailKey(_ file: FileEntry) -> String { "\(file.id):\(file.version)" }
    /// 将照片、视频或小文本解密到临时目录并生成内存缩略内容。
    /// 入参：file（FileEntry）为文件元数据。
    /// 返回值：Void，缓存UIImage或前300个文本字符；不支持、未解锁、已缓存/在途/失败则跳过。
    /// 异常：错误捕获记录，有效票据下记thumbnailFailures。
    /// 注意事项：绕过busy可与其他任务并发；每类缓存上限120，移除Dictionary.keys.first并非最早插入或LRU。同步图片/小文本处理在MainActor；临时文件尽力删除，内存图像/文本不零化。
    func loadThumbnail(_ file: FileEntry) async {
        // 依据扩展名与已知文本扩展名判断是否按文本处理。
        let ext = URL(fileURLWithPath: file.leafName).pathExtension.lowercased()
        let textType = UTType(filenameExtension: ext)?.conforms(to: .plainText) == true || ["txt", "md", "csv", "log", "json", "xml", "yaml", "yml"].contains(ext)
        // 文本缩略只服务小文件（≤512KB），避免大文本拖慢界面。
        let isText = file.category != .photo && file.category != .video && textType && file.size <= 512 * 1024
        // 只处理照片、视频或小文本文件；容器已锁定则不再加载。
        guard (file.category == .photo || file.category == .video || isText), container != nil else { return }
        let key = thumbnailKey(file)
        // 已有结果、正在加载或已失败，都直接跳过。
        guard thumbnails[key] == nil, textThumbnails[key] == nil, !thumbnailTasks.contains(key), !thumbnailFailures.contains(key) else { return }
        let ticket = generation
        thumbnailTasks.insert(key)
        defer { if ticket == generation { thumbnailTasks.remove(key) } }
        // 每次加载使用独立临时目录（0700 权限 + complete 数据保护），避免互相干扰。
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("veil-preview-" + UUID().uuidString, isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700, .protectionKey: FileProtectionType.complete])
            defer { AppLog.attempt({ try FileManager.default.removeItem(at: folder) }) }
            // 让服务把解密后的文件副本写到这个临时目录。
            let url = folder.appendingPathComponent(file.leafName)
            try await client.execute(NativeRequest(operation: "export_file", name: file.name, destination: url.path))
            guard ticket == generation, !Task.isCancelled else { return }
            if isText {
                // 文本：读出来解码后取前 300 字作为缩略内容。
                let data = try Data(contentsOf: url)
                guard ticket == generation, !Task.isCancelled else { return }
                if let text = Self.decodeText(data) {
                    // 缓存上限120，移除字典当前首键；Dictionary不保证这里是最旧条目。
                    if textThumbnails.count >= 120, let oldest = textThumbnails.keys.first { textThumbnails.removeValue(forKey: oldest) }
                    textThumbnails[key] = String(text.prefix(300))
                } else { thumbnailFailures.insert(key) }
                return
            }
            let image: UIImage?
            if file.category == .photo {
                // 照片：用 ImageIO 生成最大 360 像素的缩略图，并自动应用方向变换。
                let options: [CFString: Any] = [kCGImageSourceCreateThumbnailFromImageAlways: true, kCGImageSourceCreateThumbnailWithTransform: true, kCGImageSourceThumbnailMaxPixelSize: 360]
                if let source = CGImageSourceCreateWithURL(url as CFURL, nil), let cgImage = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) { image = UIImage(cgImage: cgImage) }
                else { image = nil }
            } else {
                // 视频：用 AVAssetImageGenerator 抽取首帧并缩到 360 以内。
                let generator = AVAssetImageGenerator(asset: AVURLAsset(url: url))
                generator.appliesPreferredTrackTransform = true
                generator.maximumSize = CGSize(width: 360, height: 360)
                if let result = await AppLog.attemptAsync({ try await generator.image(at: .zero) }) { image = UIImage(cgImage: result.image) }
                else { image = nil }
            }
            guard ticket == generation, !Task.isCancelled else { return }
            if let image {
                // 图片缓存同样限120，移除字典首键，不是有序LRU淘汰。
                if thumbnails.count >= 120, let oldest = thumbnails.keys.first { thumbnails.removeValue(forKey: oldest) }
                thumbnails[key] = image
            } else { thumbnailFailures.insert(key) }
        } catch { AppLog.capture(error);
            // 失败仅在会话未失效时记录，避免旧会话失败污染新状态。
            if ticket == generation, !Task.isCancelled { thumbnailFailures.insert(key) }
        }
    }
    /// 本地验证新目录单级名称，并在当前路径下创建。
    /// 入参：rawName（String）为输入，trim后合法才继续。
    /// 返回值：Void，只在路径浏览且已打开时通过run执行。
    /// 异常：非法名称设validation错误；服务失败run处理。
    /// 注意事项：提前捕获parent，不在await后随UI位置变化重算；服务成功但票据过期时目录可已创建而界面不更新。
    func createFolder(named rawName: String) async {
        guard let name = folderNameForCreation(rawName) else {
            error = AppLog.validation("文件夹名称无效，请使用不含路径分隔符的名称")
            return
        }
        guard mode == .path, container != nil else { return }
        // 记下父目录，创建时作为 directory 参数。
        let parent = directory
        await run { ticket in
            let result: ContainerSnapshot = try await self.client.json(
                NativeRequest(operation: "create_directory", name: name, directory: parent)
            )
            guard ticket == self.generation else { return }
            self.container = result
            self.notice = "文件夹已创建"
        }
    }

    /// 删除一个浏览文件或整个目录子树并接收新快照。
    /// 入参：item（BrowserEntry）提供内部path。
    /// 返回值：Void，票据相同才更新container。
    /// 异常：服务失败由run记录。
    /// 注意事项：已提交删除不因票据变化或后续错误回滚；物理回收异步尽力，UI不保证已无磁盘残留。
    func remove(_ item: BrowserEntry) async { await run { ticket in
        let result: ContainerSnapshot = try await self.client.json(NativeRequest(operation: "remove_path", name: item.path))
        if ticket == self.generation { self.container = result }
    } }
    /// 发起流式导出并配套冲突/进度轮询。
    /// 入参：names（[String]）每项一个内部选择；destination（URL）为目标；ticket为代次。
    /// 返回值：Void，服务成功后返回。
    /// 异常：服务或协议错误向上抛，defer清理轮询/审核/进度。
    /// 注意事项：进度任务只按operation过滤不查ticket；外层负责安全作用域和完成提示；取消轮询不自动撤回已发布文件。
    private func streamExport(_ names: [String], to destination: URL, ticket: Int) async throws {
        importStatus = "正在导出文件"
        // 导出同样可能遇到目标目录重名冲突，复用同一套冲突轮询（exporting 模式）。
        let conflicts = Task { @MainActor in await self.pollImportConflicts(ticket: ticket, exporting: true) }
        // 进度轮询：只展示导出方向的进度。
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

    /// 将所选文件/目录导出到用户给定目录，并显示成功/跳过数量。
    /// 入参：entry（BrowserEntry）为选择；destination（URL）为目录，文件另拼leafName，目录保留其根名称。
    /// 返回值：Void，run忙时忽略。
    /// 异常：用户取消按阶段转提示；其他失败由run处理。
    /// 注意事项：仅startAccessing成功时stop，失败仍尝试导出；错误可能已有文件发布。
    func exportToDirectory(_ entry: BrowserEntry, destination: URL) async { await run { ticket in
        // 申请目标目录的安全作用域访问权限，结束后释放。
        let accessed = destination.startAccessingSecurityScopedResource()
        defer { if accessed { destination.stopAccessingSecurityScopedResource() } }
        // 文件直接落到目标目录下的同名文件；文件夹则整个目录内容写入目标目录。
        let target: URL
        if case .file(let file) = entry { target = destination.appendingPathComponent(file.leafName) }
        else { target = destination }
        do { try await self.streamExport([entry.path], to: target, ticket: ticket) }
        catch {
            // 用户取消导出：已导出的文件保留，给出温和提示。
            if ticket == self.generation, self.client.transferProgress()?.phase == "cancelled" {
                self.notice = "导出已中止，已导出的文件已保留"
                return
            }
            throw AppLog.recorded(error)
        }
        // 成功后统计实际完成与跳过的文件数。
        if ticket == self.generation, let progress = self.client.transferProgress() {
            self.notice = "导出完成 \(progress.completed_files - progress.skipped_files) 个文件，跳过 \(progress.skipped_files) 个文件"
        }
    } }

    /// 把日志JSON载荷包装为系统文件导出条目。
    /// 入参：无。
    /// 返回值：Void，成功设置export，失败设置error。
    /// 异常：捕获AppLog.export错误并记录。
    /// 注意事项：不通过busy、未锁定到任务generation；Data全量在内存，导出目标稍后由系统面板决定。
    func prepareErrorLogExport() {
        do {
            export = ExportItem(name: "Veil-错误日志.json", document: ExportDocument(data: try AppLog.export()))
        } catch {
            AppLog.capture(error)
            self.error = error.localizedDescription
        }
    }

    /// 准备单文件明文或整容器加密包供系统文件面板保存。
    /// 入参：file（FileEntry?）有值导出单文件，nil打包当前容器。
    /// 返回值：Void，ticket有效时设置ExportItem，载荷全量读到Data。
    /// 异常：目录/服务/读取失败run处理。
    /// 注意事项：defer仅尽力删除暂存目录；大文件/包全量读入内存，未提供内存上限；容器名只替换正斜杠，服务负责输出。
    func prepareExport(_ file: FileEntry? = nil) async { await run { ticket in
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("veil-export-" + UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700, .protectionKey: FileProtectionType.complete])
        defer { AppLog.attempt({ try FileManager.default.removeItem(at: folder) }) }
        // 单文件保留原文件名；备份包统一命名为“容器名.veil”。
        let filename = file?.leafName ?? ((self.container?.name ?? "Veil").replacingOccurrences(of: "/", with: "_") + ".veil")
        let destination = folder.appendingPathComponent(filename)
        // 单文件走流式导出；整容器走服务端打包（export_package）。
        if let file { try await self.streamExport([file.name], to: destination, ticket: ticket) }
        else { try await self.client.execute(NativeRequest(operation: "export_package", destination: destination.path)) }
        // 读出成品交给系统导出面板。
        let data = try Data(contentsOf: destination)
        if ticket == self.generation { self.export = ExportItem(name: filename, document: ExportDocument(data: data)) }
    } }
    /// 解密一份本地副本，再按图片/视频URL或文本展示。
    /// 入参：file（FileEntry）为待预览文件。
    /// 返回值：Void，ticket有效才设置PreviewItem。
    /// 异常：服务/目录/读取/文本编码失败由run处理。
    /// 注意事项：图片/视频临时目录交PreviewItem管理，文本读整文件无大小上限，后台仅做Data读取，文本解码仍在MainActor；清理尽力且不安全擦除。
    func show(_ file: FileEntry) async { await run { ticket in
        // 结合分类与扩展名判断图片 / 视频，其余按文本尝试。
        let ext = URL(fileURLWithPath: file.leafName).pathExtension
        let type = UTType(filenameExtension: ext)
        let isImage = file.category == .photo || type?.conforms(to: .image) == true
        let isVideo = file.category == .video || type?.conforms(to: .movie) == true
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("veil-preview-" + UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700, .protectionKey: FileProtectionType.complete])
        var retained = false
        // 预览条目没接管文件夹时（失败或文本场景）自动清理。
        defer { if !retained { AppLog.attempt({ try FileManager.default.removeItem(at: folder) }) } }
        // 服务端把解密后的文件副本写到临时目录。
        let url = folder.appendingPathComponent(file.leafName)
        try await self.client.execute(NativeRequest(operation: "export_file", name: file.name, destination: url.path))
        guard ticket == self.generation else { return }
        if isImage || isVideo {
            // 图片 / 视频交给文件 URL 预览，临时目录由 PreviewItem 接管，关闭预览时删除。
            self.preview = PreviewItem(file: file, content: isVideo ? .video(url) : .image(url), temporaryFolder: folder)
            retained = true
        } else {
            // 文本预览不限制大小；读取放在后台，避免大文本阻塞主线程。
            let data = try await Task.detached(priority: .userInitiated) { try Data(contentsOf: url) }.value
            guard ticket == self.generation else { return }
            guard let text = Self.decodeText(data) else { throw ClientError.unsupportedContent("此文件不是支持的文本格式，请导出后使用其他应用打开。") }
            self.preview = PreviewItem(file: file, content: .text(text), temporaryFolder: nil)
        }
    } }
    /// 按UTF-8或带BOM的UTF-16尝试把明文字节解释为文本。
    /// 入参：data（Data）为完整文件内容。
    /// 返回值：String?，解码成功且无禁止C0控制字符时返回文本，否则nil。
    /// 异常：不抛错。
    /// 注意事项：仅排除码点<32且不为Tab/LF/CR，未拒绝所有Unicode控制字符，不是完备二进制识别。
    private static func decodeText(_ data: Data) -> String? {
        let text: String?
        // 以字节序标记（BOM）区分 UTF-16 与 UTF-8。
        if data.starts(with: [0xff, 0xfe]) || data.starts(with: [0xfe, 0xff]) {
            text = String(data: data, encoding: .utf16)
        } else {
            text = String(data: data, encoding: .utf8)
        }
        // 二进制内容通常包含除制表符 / 换行 / 回车以外的控制字符，直接判为非文本。
        guard let text, !text.unicodeScalars.contains(where: { $0.value < 32 && ![9, 10, 13].contains($0.value) }) else { return nil }
        return text
    }
    /// 启动时清理临时目录下匹配预览前缀的旧项目。
    /// 入参：无。
    /// 返回值：Void，枚举失败返回，删除失败仅记录。
    /// 异常：不向外抛错，经AppLog.attempt处理。
    /// 注意事项：按名称前缀匹配，不验证是否目录/年龄/仍被其他任务使用；调用方须选择合适时机。
    private func removeStalePreviews() {
        let manager = FileManager.default
        guard let items = AppLog.attempt({ try manager.contentsOfDirectory(at: manager.temporaryDirectory, includingPropertiesForKeys: nil) }) else { return }
        // 只清理自家前缀的目录，不碰其他应用 / 系统的临时文件。
        for item in items where item.lastPathComponent.hasPrefix("veil-preview-") { AppLog.attempt({ try manager.removeItem(at: item) }) }
    }
}
/// 预览条目：把“文件信息 + 预览内容位置”打包给预览页。
/// 图片 / 视频持有解密后临时文件的 URL，文本直接持有内容字符串。
struct PreviewItem: Identifiable {
    /// 预览内容的三种形态：图片文件、视频文件或文本字符串。
    enum Content { case image(URL), video(URL), text(String) }
    /// 每次预览生成独立 id，用于检测预览切换。
    let id = UUID()
    /// 被预览的容器内文件条目（展示文件名等元数据）。
    let file: FileEntry
    /// 预览内容本体。
    let content: Content
    /// 图片 / 视频解密临时目录；文本预览为 nil。
    let temporaryFolder: URL?
    /// 尽力移除预览拥有的临时目录。
    /// 入参：无，使用PreviewItem.temporaryFolder，可nil。
    /// 返回值：Void。
    /// 异常：错误交AppLog.attempt，不抛。
    /// 注意事项：不保证成功或安全擦除；调用前须确保预览播放器等不再依赖此URL。
    func removeTemporaryFiles() { if let temporaryFolder { AppLog.attempt({ try FileManager.default.removeItem(at: temporaryFolder) }) } }
}

/// 系统导出条目：id为每次包装生成的UUID，name为建议保存名，document持全量Data载荷。
/// 用于分享/存储面板；不直接写用户目标，释放引用不保证擦除所有明文内存副本。
struct ExportItem: Identifiable { let id = UUID(); let name: String; let document: ExportDocument }
/// 系统 FileDocument 导出文档：把内存中的导出内容包装成可写出的文件。
struct ExportDocument: FileDocument {
    /// 声明为通用二进制数据，任何导出场景都可用。
    static var readableContentTypes: [UTType] { [.data] }
    /// 要导出的内容字节（单文件或 .veil 备份包）。
    let data: Data
    /// 保存供系统文件导出的完整内存字节。
    /// 入参：data（Data）为明文单文件或加密包。
    /// 返回值：ExportDocument。
    /// 异常：不抛错。
    /// 注意事项：Data可写时复制，不在此零化或限制大小。
    init(data: Data) { self.data = data }
    /// 从系统读取配置初始化导出文档模型。
    /// 入参：configuration（ReadConfiguration）含FileWrapper。
    /// 返回值：regularFileContents有值则保存，否则保存空Data。
    /// 异常：声明throws但当前主体无主动抛错。
    /// 注意事项：不支持的目录/内容不会报错而是空文档。
    init(configuration: ReadConfiguration) throws { data = configuration.file.regularFileContents ?? Data() }
    /// 为系统文件导出器提供要写出的普通文件载荷。
    /// 入参：configuration（WriteConfiguration）为系统写配置，当前未使用。
    /// 返回值：FileWrapper，regularFileWithContents为data。
    /// 异常：声明throws，当前主体无主动抛错。
    /// 注意事项：这里只构造载荷，不实际完成用户目标文件写入；系统之后仍可失败。
    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper { FileWrapper(regularFileWithContents: data) }
}

/// 相册导入中转文件：把相册条目复制到受保护的临时目录，供导入过程使用。
private struct PhotoImportFile: Transferable {
    /// 复制后的本地临时文件位置。
    let url: URL
    /// 支持图片与视频两种导入类型，统一走“复制到临时目录”的流程。
    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(importedContentType: .image) { try copy($0) }
        FileRepresentation(importedContentType: .movie) { try copy($0) }
    }
    /// 把系统临时相册文件复制进应用新建受保护目录。
    /// 入参：received（ReceivedTransferredFile）为系统提供的来源。
    /// 返回值：PhotoImportFile含新URL。
    /// 异常：目录创建/复制错误抛出，复制失败先尝试清目录并经AppLog.recorded。
    /// 注意事项：目录0700并请求complete数据保护；复制期间系统来源需有效，失败清理不保证成功，未实现崩溃后的photo-import前缀扫描。
    private static func copy(_ received: ReceivedTransferredFile) throws -> PhotoImportFile {
        let manager = FileManager.default
        let folder = manager.temporaryDirectory.appendingPathComponent("veil-photo-import-" + UUID().uuidString, isDirectory: true)
        try manager.createDirectory(at: folder, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700, .protectionKey: FileProtectionType.complete])
        do {
            // 目标文件名沿用相册里的原文件名。
            let destination = folder.appendingPathComponent(received.file.lastPathComponent)
            try manager.copyItem(at: received.file, to: destination)
            return PhotoImportFile(url: destination)
        } catch {
            // 复制失败：清理刚建的目录，再把错误向上抛。
            AppLog.attempt({ try manager.removeItem(at: folder) })
            throw AppLog.recorded(error)
        }
    }
}
