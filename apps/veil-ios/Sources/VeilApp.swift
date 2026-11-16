/// ============================================================================
/// Veil iOS 客户端 · 应用入口与界面层（VeilApp）
///
/// 文件作用：组装应用根界面（首页 / 容器浏览 / 各弹窗表单），是 iOS 客户端全部
/// 可视界面的挂载点；同时监听应用前后台切换，在进入后台时锁定容器并刷新日志。
/// 所属模块：iOS 客户端 UI 层（与 Rust 服务之间隔一层 Store 状态中枢）。
/// 文件内核心类型清单：
///   - VeilApp              应用入口，注入全局 Store 并监听场景阶段
///   - RootView             根视图，按“未解锁→首页 / 已解锁→文件浏览”分屏
///   - NewSpaceView         新建空间表单
///   - DeleteContainerView  删除容器确认页
///   - ContainerFormView    创建 / 解锁 / 导入加密备份共用的密码表单
///   - PreviewView          图片 / 视频 / 文本预览容器
///   - ImagePreview         图片预览（双指缩放、双击放大）
///   - VideoPreview         视频预览（AVPlayer）
///   - ReadOnlyTextView     UITextView 包装的只读文本预览
///   - IntroView            启动品牌引导页
///   - EntryThumbnail       文件 / 文件夹缩略图
///   - PrimaryButton 等     三个按钮样式
/// 依赖能力：SwiftUI（界面）、PhotosUI（相册选择）、AVKit（视频播放）、
///   UniformTypeIdentifiers（文件类型识别）、UIKit（图片与文本原生视图）。
/// 整体业务目标：让用户在本机完成空间 / 容器管理、文件导入导出、新建文件夹、
///   图片与文本预览、加密备份（.veil，把容器打成单个文件迁移）导入导出；
///   进入后台时自动锁定容器，防止敏感内容泄露。
/// ============================================================================
import SwiftUI
import PhotosUI
import AVKit
import UniformTypeIdentifiers
import UIKit

/// 应用入口：创建全局状态中枢 Store，把它注入整个界面树，并监听应用生命周期。
/// 进入后台时提交异步锁定和日志刷新任务；回调不会等待完成，也没有在这里实现后台快照遮挡。
@main struct VeilApp: App {
    /// 全局业务状态：空间、容器、文件列表、导入进度等界面数据都集中在这里（状态管理中枢）。
    @StateObject private var store = Store()
    /// 应用场景阶段（active 前台 / background 后台 / inactive 过渡），用于识别“进入后台”事件。
    @Environment(\.scenePhase) private var phase
    var body: some Scene {
        WindowGroup { RootView().environmentObject(store).task { await store.load() } }
            // 后台事件提交异步锁定，结束后请求刷新日志；不能据此保证系统抓取快照前清理已完成。
            .onChange(of: phase) { _, value in if value == .background { Task { await store.lock(); AppLog.flush() } } }
    }
}

/// 界面主题色集合：ink 主前景色；surface 卡片底色；mutedSurface 弱化底色；outline 描边色。
private let ink = Color.primary
/// 卡片底色：跟随系统分组背景，保证深浅色模式下观感一致。
private let surface = Color(uiColor: .secondarySystemGroupedBackground)
/// 弱化底色：用于图标底衬、胶囊标签等需要再淡一层的场合。
private let mutedSurface = Color(uiColor: .tertiarySystemFill)
/// 通用描边：10% 透明度的主前景色，用于卡片和输入框边框。
private let outline = Color.primary.opacity(0.10)

/// 主按钮样式：深色实底、白字、圆角 9；按下时底色变浅。用于创建 / 解锁等关键操作。
struct PrimaryButton: ButtonStyle {
    /// 功能：为业务按钮生成与其层级一致的外观和按压反馈。
    /// - Parameter configuration: ButtonStyle.Configuration，提供按钮标签和当前按下状态。
    /// - Returns: 包含标签、字号、留白及当前样式背景的 SwiftUI 视图。
    /// - Throws: 不抛出错误。
    /// - Note: SwiftUI 按状态重新求值；这里只描述外观，不触发创建、解锁等业务动作。
    func makeBody(configuration: Configuration) -> some View {
        // 按下时把实底从 90% 降到 70% 不透明度，形成按压反馈。
        configuration.label.font(.subheadline.weight(.semibold)).padding(.horizontal, 16).frame(minHeight: 44)
            .foregroundStyle(Color(uiColor: .systemBackground)).background(Color.primary.opacity(configuration.isPressed ? 0.7 : 0.9), in: RoundedRectangle(cornerRadius: 9))
    }
}
/// 次级按钮样式：卡片底色 + 描边，用于取消等次要操作。
struct SecondaryButton: ButtonStyle {
    /// 功能：为业务按钮生成与其层级一致的外观和按压反馈。
    /// - Parameter configuration: ButtonStyle.Configuration，提供按钮标签和当前按下状态。
    /// - Returns: 包含标签、字号、留白及当前样式背景的 SwiftUI 视图。
    /// - Throws: 不抛出错误。
    /// - Note: SwiftUI 按状态重新求值；这里只描述外观，不触发创建、解锁等业务动作。
    func makeBody(configuration: Configuration) -> some View {
        // 按下时卡片底色降到 60% 不透明度，与主按钮的按压反馈保持一致。
        configuration.label.font(.subheadline.weight(.medium)).padding(.horizontal, 14).frame(minHeight: 44)
            .background(surface.opacity(configuration.isPressed ? 0.6 : 1), in: RoundedRectangle(cornerRadius: 9))
            .overlay(RoundedRectangle(cornerRadius: 9).stroke(outline))
    }
}
/// 首页小号操作按钮样式（创建 / 导入）：可切换实底或描边两种外观。
struct HomeActionButton: ButtonStyle {
    /// 是否为实底主样式；false 时退化为卡片底色样式。
    let primary: Bool
    /// 功能：为业务按钮生成与其层级一致的外观和按压反馈。
    /// - Parameter configuration: ButtonStyle.Configuration，提供按钮标签和当前按下状态。
    /// - Returns: 包含标签、字号、留白及当前样式背景的 SwiftUI 视图。
    /// - Throws: 不抛出错误。
    /// - Note: SwiftUI 按状态重新求值；这里只描述外观，不触发创建、解锁等业务动作。
    func makeBody(configuration: Configuration) -> some View {
        // 实底样式的文字用系统背景色，描边样式的文字用主前景色。
        configuration.label.font(.caption.weight(.semibold)).lineLimit(1)
            .padding(.horizontal, 11).frame(minHeight: 38)
            .foregroundStyle(primary ? Color(uiColor: .systemBackground) : Color.primary)
            .background(primary ? Color.primary.opacity(configuration.isPressed ? 0.7 : 0.9) : surface, in: RoundedRectangle(cornerRadius: 9))
            .overlay(RoundedRectangle(cornerRadius: 9).stroke(outline))
    }
}
/// 根视图：根据“容器是否已解锁”在首页与文件浏览页之间切换，并挂载全部弹窗、导入导出入口。
/// 未解锁时展示空间与容器列表（home）；解锁后展示文件浏览器（browser）。
struct RootView: View {
    /// 全局业务状态中枢，由 VeilApp 注入。
    @EnvironmentObject var store: Store
    /// 应用场景阶段；非前台时显示遮挡，进入后台时重置下方显式列出的部分交互状态。
    @Environment(\.scenePhase) private var phase
    /// 当前正在展示的容器表单（创建 / 解锁 / 导入加密备份），非 nil 时弹出对应 sheet。
    @State private var form: ContainerForm?
    /// 启动品牌引导是否还在展示。
    @State private var showIntro = true
    /// 是否弹出“新建空间”表单。
    @State private var newSpace = false
    /// 是否弹出“清理本机数据”确认框。
    @State private var confirmClearData = false
    /// 是否弹出通用“从文件导入”选择器。
    @State private var importing = false
    /// 是否弹出“从相册导入”选择器。
    @State private var importingPhotos = false
    /// 相册选择条目数组，每项是系统提供的照片或视频引用；变化后复制给导入任务，再清空界面选择。
    @State private var selectedPhotos: [PhotosPickerItem] = []
    /// 是否弹出“导入加密备份（.veil）”选择器。
    @State private var importingPackage = false
    /// 待导入备份 URL；选择时尝试开启安全作用域访问，表单关闭时调用 stop，但没有保存开启是否成功的标记。
    @State private var pendingPackage: URL?
    /// 待删除的文件 / 文件夹条目，非 nil 时弹出删除确认框。
    @State private var deleting: BrowserEntry?
    /// 待删除的容器摘要，非 nil 时弹出删除容器页。
    @State private var deletingContainer: ContainerSummary?
    /// 待删除的空间，非 nil 时弹出删除空间确认框。
    @State private var deletingSpace: Space?
    /// 是否弹出“新建文件夹”输入框。
    @State private var showNewFolder = false
    /// 新建文件夹的输入内容。
    @State private var newFolderName = ""
    /// 组合首页或浏览页及其弹窗；本地状态保存用户选择，实际文件和容器操作均委托 Store。
    var body: some View {
        NavigationStack {
            // 有已解锁容器时显示文件浏览器，否则显示空间 / 容器首页。
            Group { if store.container == nil { home } else { browser } }
                .background(Color(uiColor: .systemGroupedBackground))
                // 导航标题：已解锁显示容器名，未解锁显示固定文案。
                .navigationTitle(store.container?.name ?? "我的容器")
                .navigationBarTitleDisplayMode(.inline)
                // 未解锁时隐藏导航栏，让首页更像纯引导页。
                .toolbar(store.container == nil ? .hidden : .visible, for: .navigationBar)
                .toolbar {
                    // 已解锁时提供锁定入口；点击提交异步任务，回调返回不代表锁定已经完成。
                    if store.container != nil { ToolbarItem(placement: .topBarLeading) { Button("锁定", systemImage: "lock") { Task { await store.lock() } } } }
                    ToolbarItem(placement: .topBarTrailing) {
                        // 右上角“更多”菜单：未解锁时是空间与数据维护项，已解锁时是导入导出项。
                        Menu { if store.container == nil {
                            // ——未解锁：空间管理入口——
                            Button("新建空间", systemImage: "folder.badge.plus") { newSpace = true }
                            // 空间不可用时不能导入加密备份。
                            Button("导入加密备份", systemImage: "square.and.arrow.down") { importingPackage = true }
                                .disabled(store.space?.isAvailable != true)
                            Divider()
                            Button("导出错误日志", systemImage: "doc.text") { store.prepareErrorLogExport() }
                            Button("清理数据", systemImage: "trash", role: .destructive) { confirmClearData = true }
                        } else {
                            // ——已解锁：导入 / 导出入口——
                            importOptions
                            Divider()
                            Button("导出加密备份", systemImage: "square.and.arrow.up") { Task { await store.prepareExport() } }
                        } } label: { Image(systemName: "ellipsis.circle") }.disabled(store.busy)
                    }
                }
                .overlay {
                    // busy 时在内容中央叠加进度卡片；不是覆盖全屏的点击拦截层，其他控件是否可点由各自 disabled 决定。
                    if store.busy {
                        VStack(spacing: 12) {
                            ProgressView()
                            // 仅当存在导入状态文案时才展示进度区块。
                            if let status = store.importStatus {
                                Text(status).font(.subheadline)
                                // 已知文件总数时展示确定性进度条；扫描中总数可能继续增长。
                                if let progress = store.importProgress, progress.total_files > 0 {
                                    ProgressView(value: min(1, Double(progress.completed_files) / Double(progress.total_files))).frame(width: 220)
                                    Text("\(Int(min(1, Double(progress.completed_files) / Double(progress.total_files))*100))% · 已完成 \(progress.completed_files) / \(progress.total_files)").font(.caption).monospacedDigit()
                                    Text(progress.scan_complete ? "文件遍历完成" : "正在发现文件，总数持续增加").font(.caption)
                                    // 展示服务 waiting_files；该计数代表等待处理项，不能仅凭这里判断都是同名冲突。
                                    if progress.waiting_files > 0 { Text("待选择：\(progress.waiting_files) 个文件").font(.caption) }
                                }
                                Button("取消") { store.cancelImport() }
                            }
                        }.padding().background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14))
                    }
                }
        }
        .overlay {
            // 品牌引导层：启动后展示约 1 秒再淡出。
            if showIntro {
                IntroView().transition(.opacity).allowsHitTesting(true)
            }
        }
        .task {
            // 引导层停留 1 秒后淡出；等待被取消时直接结束任务。
            do { try await Task.sleep(for: .seconds(1)) } catch { return }
            withAnimation(.easeOut(duration: 0.2)) { showIntro = false }
        }
        .task {
            // 每 15 秒在前台轮询一次空间可用性（例如外置磁盘是否重新连接）。
            while !Task.isCancelled {
                do { try await Task.sleep(for: .seconds(15)) } catch { return }
                if phase == .active { await store.refreshSpaceAvailability() }
            }
        }
        // 向系统标记敏感视图；privacySensitive 不是禁止用户截图或录屏的保证。
        .privacySensitive()
        // 非 active 时按场景状态显示不透明底色以遮挡内容；系统快照时序仍不由此代码直接控制。
        .overlay { if phase != .active { Color(uiColor: .systemBackground).ignoresSafeArea() } }
        // 后台时清空此回调列出的表单、删除、文件选择和目录导出状态；相册选择、新建空间等状态未在此重置。
        .onChange(of: phase) { _, value in if value == .background { form = nil; deleting = nil; deletingContainer = nil; deletingSpace = nil; showNewFolder = false; importing = false; importingPackage = false; store.exportSelection = nil } }
        .tint(ink)
        // 表单关闭时清除待导入 URL 并调用停止安全作用域；不取消已经提交给 Store 的导入任务。
        .sheet(item: $form, onDismiss: { if let url = pendingPackage { url.stopAccessingSecurityScopedResource(); pendingPackage = nil } }) { ContainerFormView(form: $0) { form = nil } }
        // 选择“导出到文件夹”目标目录；选择成功即开始导出，失败则记录并提示。
        .fileImporter(isPresented: Binding(get: { store.exportSelection != nil }, set: { _ in }), allowedContentTypes: [.folder]) { result in
            let entry = store.exportSelection
            store.exportSelection = nil
            if case .success(let url) = result, let entry { Task { await store.exportToDirectory(entry, destination: url) } }
            if case .failure(let error) = result { AppLog.capture(error); store.error = error.localizedDescription }
        }
        // 系统分享 / 存储面板：把准备好的导出内容（单文件或加密备份）写入用户选择的位置。
        .fileExporter(isPresented: Binding(get: { store.export != nil }, set: { if !$0 { store.export = nil } }), document: store.export?.document, contentType: .data, defaultFilename: store.export?.name ?? "Veil") { result in
            if case .failure(let error) = result { AppLog.capture(error); store.error = error.localizedDescription }; store.export = nil
        }
        // 导入 / 导出过程中遇到重名冲突时弹出裁决页；禁止下拉关闭，用户必须显式选择。
        .sheet(item: $store.importInspection) { inspection in
            ImportConflictReview(inspection: inspection, operationLabel: store.conflictOperation, progressText: store.importProgressDescription) { store.completeImportReview($0, id: inspection.id) }
                .interactiveDismissDisabled()
                // 消失时兜底回传 nil；Store 按批次 ID 忽略已经恢复的等待，未决批次则请求中止，已完成文件不自动回滚。
                .onDisappear { store.completeImportReview(nil, id: inspection.id) }
        }
        // 文件预览页：图片 / 视频 / 文本共用同一入口。
        .sheet(item: $store.preview) { PreviewView(item: $0) }
        // 删除容器确认页。
        .sheet(item: $deletingContainer) { item in DeleteContainerView(item: item) }
        // 新建空间表单。
        .sheet(isPresented: $newSpace) { NewSpaceView() }
        // 清理本机数据的二次确认：破坏性操作，文案强调不可撤销。
        .alert("清理所有本机数据？", isPresented: $confirmClearData) {
            Button("取消", role: .cancel) { }
            Button("清理所有数据", role: .destructive) { Task { await store.clearLocalData() } }
        } message: {
            Text("将永久删除此设备上 Veil 的所有空间、容器、文件和设置，无法撤销。请先导出需要保留的加密备份。")
        }
        // 新建文件夹输入框：名称通过 folderNameForCreation 校验后方可提交。
        .alert("新建文件夹", isPresented: $showNewFolder) {
            TextField("文件夹名称", text: $newFolderName)
            Button("创建") {
                let name = newFolderName
                newFolderName = ""
                Task { await store.createFolder(named: name) }
            }
            .disabled(folderNameForCreation(newFolderName) == nil)
            Button("取消", role: .cancel) { newFolderName = "" }
        } message: {
            Text("将在「\(store.directory.isEmpty ? "全部文件" : store.directory)」下创建文件夹。")
        }
        // 通用“操作完成”提示弹窗。
        .alert("操作完成", isPresented: Binding(get: { store.notice != nil }, set: { if !$0 { store.notice = nil } })) {
            Button("好") { store.notice = nil }
        } message: { Text(store.notice ?? "") }
        // 通用“操作未完成”错误弹窗。
        .alert("操作未完成", isPresented: Binding(get: { store.error != nil }, set: { if !$0 { store.error = nil } })) { Button("好") { store.error = nil } } message: { Text(store.error ?? "") }
        // 删除文件 / 文件夹的二次确认。
        .confirmationDialog("删除后无法恢复", isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }), titleVisibility: .visible) {
            Button("删除", role: .destructive) { if let item = deleting { Task { await store.remove(item) } }; deleting = nil }
        }
        // 删除空间：可用空间与不可用空间使用不同的标题与按钮文案。
        .confirmationDialog(deletingSpace?.isAvailable == true ? "删除空间？" : "删除空间记录？", isPresented: Binding(get: { deletingSpace != nil }, set: { if !$0 { deletingSpace = nil } }), titleVisibility: .visible) {
            Button(deletingSpace?.isAvailable == true ? "删除空间" : "删除记录", role: .destructive) { if let space = deletingSpace { Task { await store.deleteSpace(space) } }; deletingSpace = nil }
            Button("取消", role: .cancel) { deletingSpace = nil }
        } message: {
            if let space = deletingSpace, !space.isAvailable {
                // 不可用空间只删登记记录，磁盘文件保留。
                Text("删除后，Veil 中的空间和容器记录无法恢复。磁盘上的空间文件和容器文件不会被删除。")
            } else {
                // 删除默认空间后服务会自动重建一个空默认空间。
                Text(deletingSpace?.isDefault == true ? "将永久删除整个空间文件夹、其中的容器和其他文件。完成后会自动重新建立一个空的默认空间。无法撤销。" : "将永久删除整个空间文件夹、其中的容器和其他文件。无法撤销。")
            }
        }
        // 相册选择器：只允许选图片与视频。
        .photosPicker(isPresented: $importingPhotos, selection: $selectedPhotos, matching: .any(of: [.images, .videos]))
        // 每轮选择结果 items 是本次相册引用快照；先清空界面选择，再异步请求读取并导入，不能保证所有条目都成功。
        .onChange(of: selectedPhotos) { _, items in
            guard !items.isEmpty else { return }
            selectedPhotos = []
            Task { await store.importPhotos(items) }
        }
        // 成功结果 urls 每项是一个来源 URL，原样交给 Store；这里不做类型或存在性验证，失败只记录和提示。
        .fileImporter(isPresented: $importing, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            switch result { case .success(let urls): Task { await store.importFiles(urls) }; case .failure(let error): AppLog.capture(error); store.error = error.localizedDescription }
        }
        // 备份选择器接收通用 data；不在此校验 veil 后缀或包结构，服务将在密码提交后验证。
        .fileImporter(isPresented: $importingPackage, allowedContentTypes: [.data]) { result in
            switch result { case .success(let url):
                guard let space = store.space else { store.error = AppLog.validation("请先创建空间"); return }
                // 忽略开启访问的 Bool；即使失败也进入表单，后续读取可能因权限不足失败。
                _ = url.startAccessingSecurityScopedResource(); pendingPackage = url
                form = .importPackage(path: url.path, space: space.id)
            case .failure(let error): AppLog.capture(error); store.error = error.localizedDescription }
        }
    }
    /// 首页：展示品牌头部、空间切换菜单、容器列表与维护入口。容器未解锁时使用。
    var home: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                HStack(spacing: 12) {
                    Image("Brand").resizable().scaledToFit().frame(width: 32, height: 32).clipShape(RoundedRectangle(cornerRadius: 7))
                    Text("veil").font(.headline)
                    Spacer()
                    Menu {
                        // 切换空间：列出本机登记的全部空间，当前空间带勾选标记。
                        Section("切换空间") {
                            // 每轮一个已登记空间，包括离线项；点击只改变 selectedSpace，不解锁其中容器。
                            ForEach(store.spaces) { space in
                                Button { store.selectedSpace = space.id } label: {
                                    // 不可用空间在名称后追加提示。
                                    let title = space.name + (space.isAvailable ? "" : " · 不可用")
                                    if space.id == store.space?.id { Label(title, systemImage: "checkmark") }
                                    else { Text(title) }
                                }
                            }
                            Button("新建空间", systemImage: "folder.badge.plus") { newSpace = true }
                        }
                        // 管理空间：仅对当前空间提供删除入口。
                        Section("管理空间") {
                            if let space = store.space {
                                Button(space.isAvailable ? "删除当前空间" : "删除空间记录", systemImage: "trash", role: .destructive) { deletingSpace = space }
                            }
                        }
                        // 首页维护入口；这里的备份导入按钮未按空间可用性单独禁用，选择回调还会检查是否存在空间。
                        Section("其他操作") {
                            Button("导入加密备份", systemImage: "square.and.arrow.down") { importingPackage = true }
                            Button("导出错误日志", systemImage: "doc.text") { store.prepareErrorLogExport() }
                            Button("清理数据", systemImage: "trash", role: .destructive) { confirmClearData = true }
                        }
                    } label: { HStack { Text(store.space?.name ?? "选择空间").lineLimit(1); Image(systemName: "chevron.down") }.font(.subheadline.weight(.medium)) }
                        .disabled(store.busy)
                }
                VStack(alignment: .leading, spacing: 14) {
                    HStack(spacing: 8) {
                        Text("\(store.space?.name ?? "空间")的容器").font(.headline).lineLimit(1)
                        Spacer(minLength: 4)
                        // 创建容器：只有在空间可用时才允许。
                        Button { if let space = store.space { form = .create(space: space.id) } } label: {
                            Label("创建", systemImage: "plus")
                        }.buttonStyle(HomeActionButton(primary: true)).disabled(store.busy || store.space?.isAvailable != true)
                        // 导入加密备份：同样要求空间可用。
                        Button { importingPackage = true } label: {
                            Label("导入", systemImage: "square.and.arrow.down")
                        }.buttonStyle(HomeActionButton(primary: false)).disabled(store.busy || store.space?.isAvailable != true)
                    }
                    if let space = store.space {
                        if !space.isAvailable {
                            // 空间不可用提示卡：引导用户重连磁盘或检查目录。
                            HStack(spacing: 12) {
                                Image(systemName: "externaldrive.badge.xmark").font(.title3).frame(width: 42, height: 42).background(mutedSurface, in: RoundedRectangle(cornerRadius: 9))
                                VStack(alignment: .leading, spacing: 5) {
                                    Text("空间当前不可用").font(.subheadline.weight(.semibold))
                                    Text("请重新连接外置磁盘，或检查空间目录：\(space.path)").font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                                }
                            }.frame(maxWidth: .infinity, alignment: .leading).padding(16).background(surface, in: RoundedRectangle(cornerRadius: 12)).overlay(RoundedRectangle(cornerRadius: 12).stroke(outline))
                        } else if space.containers.isEmpty {
                            // 空空间占位卡：虚线边框提示下一步动作。
                            HStack(spacing: 12) {
                                Image(systemName: "folder").font(.title3).frame(width: 42, height: 42).background(mutedSurface, in: RoundedRectangle(cornerRadius: 9))
                                VStack(alignment: .leading, spacing: 5) { Text("这个空间还没有容器").font(.subheadline.weight(.semibold)); Text("创建或导入容器后，它会显示在这里。").font(.caption).foregroundStyle(.secondary) }
                            }.frame(maxWidth: .infinity, alignment: .leading).padding(16).background(surface, in: RoundedRectangle(cornerRadius: 12)).overlay(RoundedRectangle(cornerRadius: 12).stroke(outline, style: StrokeStyle(lineWidth: 1, dash: [4])))
                        }
                        // 每轮当前空间中的一个容器摘要，点击只打开密码表单，右侧菜单只设置待删除目标。
                        // 列表位于可用性判断之外；是否有离线容器摘要由 Store 提供的数据决定。
                        ForEach(space.containers) { item in
                            HStack(spacing: 6) {
                                Button { form = .open(path: item.path, name: item.name) } label: {
                                    HStack(spacing: 12) {
                                        Image(systemName: "checkmark.shield").font(.title3).foregroundStyle(.secondary).frame(width: 42, height: 42).background(mutedSurface, in: RoundedRectangle(cornerRadius: 9))
                                        VStack(alignment: .leading, spacing: 5) { Text(item.name).font(.subheadline.weight(.semibold)); Text("本机 / \(space.name)").font(.caption).foregroundStyle(.secondary) }
                                        Spacer(); Image(systemName: "arrow.right").font(.caption).foregroundStyle(.secondary)
                                    }.padding(.vertical, 13).padding(.leading, 14)
                                }.buttonStyle(.plain).frame(maxWidth: .infinity, alignment: .leading)
                                Menu {
                                    Button("删除容器", systemImage: "trash", role: .destructive) { deletingContainer = item }
                                } label: {
                                    Image(systemName: "ellipsis").font(.body.weight(.medium)).foregroundStyle(.secondary)
                                        .frame(width: 38, height: 44).contentShape(Rectangle())
                                }.accessibilityLabel("管理容器")
                            }.padding(.trailing, 4).background(surface, in: RoundedRectangle(cornerRadius: 12)).overlay(RoundedRectangle(cornerRadius: 12).stroke(outline))
                        }
                    }
                }
                HStack { Text("VEIL / PRIVATE BY DESIGN").tracking(1); Spacer(); Text("你的文件，你的空间") }.font(.system(size: 9)).foregroundStyle(.tertiary).padding(.top, 12)
            }.padding(20)
        }
    }
    /// 文件浏览页：按路径 / 按类型两种浏览模式，网格 / 列表两种布局，支持搜索与长按操作。
    var browser: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                HStack(spacing: 24) {
                    browserTab("按路径", mode: .path)
                    browserTab("按类型", mode: .type)
                    Spacer()
                    // 网格 / 列表布局切换；按钮图标显示“切过去之后”的样式。
                    Button { store.layout = store.layout == .grid ? .list : .grid } label: { Image(systemName: store.layout == .grid ? "list.bullet" : "square.grid.2x2") }.accessibilityLabel(store.layout == .grid ? "列表" : "大图标")
                    Menu { importOptions } label: { Image(systemName: "plus") }.accessibilityLabel("添加内容")
                }
                // 按类型浏览时展示横向分类筛选条。
                if store.mode == .type {
                    ScrollView(.horizontal, showsIndicators: false) { HStack(spacing: 8) {
                        filterButton("全部", category: nil)
                        // 每轮一种文件分类，选择后替换单一 category；“全部”单独使用 nil。
                        ForEach(FileCategory.allCases) { category in filterButton(category.title, category: category) }
                    } }
                }
                VStack(alignment: .leading, spacing: 16) {
                    HStack {
                        // 按路径浏览且不在根目录时，提供“返回上一级”。
                        if store.mode == .path && !store.directory.isEmpty {
                            Button { store.directory = store.directory.split(separator: "/").dropLast().joined(separator: "/") } label: { Label("返回上一级", systemImage: "chevron.left") }
                        } else { Text(store.mode == .type ? "文件分类 / \(store.category?.title ?? "全部")" : "全部文件") }
                        Spacer(); Text("\(store.entries.count) 项")
                    }.font(.caption).foregroundStyle(.secondary)
                    if store.entries.isEmpty {
                        // 空列表占位：区分“无文件”与“搜索无结果”两种场景。
                        VStack(spacing: 10) {
                            Image(systemName: "folder").font(.largeTitle).foregroundStyle(.tertiary)
                            Text(store.search.isEmpty ? "这里还没有文件" : "没有找到文件").font(.subheadline.weight(.semibold))
                            Text(store.search.isEmpty ? "添加文件，安全地保存在容器中。" : "试试其他搜索内容。 ").font(.caption).foregroundStyle(.secondary)
                            if store.search.isEmpty { Menu { importOptions } label: { Label("添加内容", systemImage: "plus") }.buttonStyle(PrimaryButton()).padding(.top, 6) }
                        }.frame(maxWidth: .infinity).padding(.vertical, 65)
                    } else if store.layout == .grid {
                        // 网格布局：自适应列宽，逐个渲染缩略图卡片。
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 98), spacing: 12)], spacing: 20) {
                            // 每轮一条已经由 Store 按目录、类别和搜索筛选的文件或目录，卡片操作绑定该条记录。
                            ForEach(store.entries) { entry in
                                Button { activate(entry) } label: {
                                    VStack(alignment: .leading, spacing: 7) {
                                        EntryThumbnail(entry: entry).frame(height: 100)
                                        Text(entry.name).font(.caption.weight(.semibold)).lineLimit(1)
                                        Text(detail(entry)).font(.caption2).foregroundStyle(.secondary).lineLimit(1)
                                    }.frame(maxWidth: .infinity, alignment: .leading)
                                }.buttonStyle(.plain).contextMenu { entryActions(entry) }
                            }
                        }
                    } else {
                        // 遍历同一筛选后 entries，每轮一条行记录；按最后条目的 ID 判断是否添加分隔线。
                        LazyVStack(spacing: 0) { ForEach(store.entries) { entry in
                            Button { activate(entry) } label: {
                                HStack(spacing: 12) { EntryThumbnail(entry: entry).frame(width: 40, height: 40); VStack(alignment: .leading, spacing: 4) { Text(entry.name).lineLimit(1); Text(detail(entry)).font(.caption).foregroundStyle(.secondary) }; Spacer(); Image(systemName: "chevron.right").font(.caption).foregroundStyle(.tertiary) }.padding(.vertical, 8)
                            }.buttonStyle(.plain).contextMenu { entryActions(entry) }
                            // 最后一行不加分割线。
                            if entry.id != store.entries.last?.id { Divider() }
                        } }
                    }
                }.padding(16).frame(maxWidth: .infinity, alignment: .leading).background(surface, in: RoundedRectangle(cornerRadius: 12)).overlay(RoundedRectangle(cornerRadius: 12).stroke(outline))
                if !store.entries.isEmpty { Text("长按文件可导出或删除").font(.caption).foregroundStyle(.secondary) }
            }.padding(20)
        }.scrollDismissesKeyboard(.interactively).searchable(text: $store.search, prompt: "搜索文件")
    }
    /// “添加内容”菜单：按路径浏览时多一个“新建文件夹”；其余两项为文件与相册导入。
    @ViewBuilder var importOptions: some View {
        // 只有按路径浏览时才提供新建文件夹（文件夹概念在类型浏览下不成立）。
        if store.mode == .path {
            Button("新建文件夹", systemImage: "folder.badge.plus") {
                newFolderName = ""
                showNewFolder = true
            }
            Divider()
        }
        Button("从文件添加", systemImage: "folder") { importing = true }
        Button("从相册添加", systemImage: "photo.on.rectangle") { importingPhotos = true }
    }
    /// 构建路径或类型浏览模式的切换按钮。
    /// - Parameters: title: String 显示标题；mode: BrowseMode 点击后采用的模式（path 或 type）。
    /// - Returns: some View，选中模式以字重和底线突出显示。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 点击即更新 mode，并清空目录及分类；搜索词不在此清除，也不等待任何文件服务调用。
    func browserTab(_ title: String, mode: BrowseMode) -> some View {
        Button { store.mode = mode; store.directory = ""; store.category = nil } label: {
            VStack(spacing: 8) { Text(title).font(.subheadline.weight(store.mode == mode ? .semibold : .regular)); Rectangle().fill(store.mode == mode ? Color.primary : .clear).frame(height: 2) }
        }.buttonStyle(.plain).foregroundStyle(store.mode == mode ? Color.primary : Color.secondary)
    }
    /// 构建单一文件类别的筛选按钮。
    /// - Parameters: title: String 分类标题；category: FileCategory? 目标分类，nil 表示全部类别。
    /// - Returns: some View，以背景和边框区分当前分类的按钮。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 点击直接替换 Store.category；重复点击已选分类不会自动取消，是否参与筛选由浏览模式决定。
    func filterButton(_ title: String, category: FileCategory?) -> some View {
        Button { store.category = category } label: { Text(title).font(.caption).padding(.horizontal, 12).padding(.vertical, 8).background(store.category == category ? mutedSurface : surface, in: RoundedRectangle(cornerRadius: 7)).overlay(RoundedRectangle(cornerRadius: 7).stroke(store.category == category ? Color.secondary : outline)) }.buttonStyle(.plain)
    }
    /// 响应用户点击文件或目录的浏览动作。
    /// - Parameters: entry: BrowserEntry 当前点击项；folder 携带容器内路径，file 携带文件元数据。
    /// - Returns: Void；目录直接修改浏览路径，文件创建异步预览任务。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 不在此检查 busy 或验证目录仍然存在；预览错误由 Store.show 处理，任务完成前界面状态可能变化。
    func activate(_ entry: BrowserEntry) {
        switch entry { case .folder(let path): store.directory = path; case .file(let file): Task { await store.show(file) } }
    }
    /// 生成文件行或卡片下方的辅助信息。
    /// - Parameters: entry: BrowserEntry 待展示的目录或文件。
    /// - Returns: String；目录固定为“文件夹”，文件在类型模式显示父目录名，在路径模式显示格式化大小。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 只使用元数据和当前模式，不读取明文内容或查询磁盘实时大小。
    func detail(_ entry: BrowserEntry) -> String {
        switch entry { case .folder: return "文件夹"; case .file(let file): return store.mode == .type ? file.parentName : file.sizeLabel }
    }
    /// 为一个浏览条目生成长按操作菜单。
    /// - Parameters: entry: BrowserEntry 当前文件或目录，保存到选择状态后由外层流程处理。
    /// - Returns: some View，所有条目可选导出到文件夹和删除，只有文件提供分享。
    /// - Throws: 不向调用方抛出异常，分享准备错误由 Store 处理。
    /// - 注意事项: 导出和删除只设置待处理条目，不立即写入或删除；分享创建异步任务，菜单自身不互斥或取消任务。
    @ViewBuilder func entryActions(_ entry: BrowserEntry) -> some View {
        Button("导出到文件夹…", systemImage: "square.and.arrow.up") { store.exportSelection = entry }
        // 仅文件支持“分享”单文件导出（文件夹整目录导出走“导出到文件夹”）。
        if case .file(let file) = entry { Button("分享文件…", systemImage: "square.and.arrow.up.on.square") { Task { await store.prepareExport(file) } } }
        Button("删除", systemImage: "trash", role: .destructive) { deleting = entry }
    }

}

/// 条目缩略图：照片 / 视频显示图片缩略，文本文件显示文字片段，其余显示分类图标。
/// 缩略图数据缓存在 Store 的 thumbnails / textThumbnails 字典里，按“文件 id:版本”取用。
private struct EntryThumbnail: View {
    /// 全局状态中枢，用于读取缩略图缓存与发起缩略图加载。
    @EnvironmentObject private var store: Store
    /// 要渲染的浏览条目（文件或文件夹）。
    let entry: BrowserEntry
    var body: some View {
        GeometryReader { geometry in
            ZStack {
                mutedSurface
                // 图片 / 视频缩略图就绪时铺满显示；视频叠加播放角标。
                if case .file(let file) = entry, let image = store.thumbnails[store.thumbnailKey(file)] {
                    Image(uiImage: image).resizable().scaledToFill()
                        .frame(width: geometry.size.width, height: geometry.size.height).clipped()
                    if file.category == .video {
                        Image(systemName: "play.fill").font(.caption).foregroundStyle(.white)
                            .padding(7).background(.black.opacity(0.55), in: Circle())
                    }
                } else if case .file(let file) = entry, let text = store.textThumbnails[store.thumbnailKey(file)] {
                    // 文本缩略：等宽小字显示内容开头片段。
                    Text(text).font(.system(size: 9, design: .monospaced)).foregroundStyle(.secondary)
                        .frame(width: geometry.size.width - 16, height: geometry.size.height - 16, alignment: .topLeading)
                        .clipped()
                } else {
                    // 无缩略图时退化为分类图标。
                    Image(systemName: entry.symbol).font(.title3).foregroundStyle(.secondary)
                }
            }.clipShape(RoundedRectangle(cornerRadius: 9))
        }
        // 视图出现或 entry.id 改变时请求缩略图，Store 自行去重和记录尝试；本任务键不含文件版本。
        // 同一 ID 的版本改变不会仅凭此 task(id:) 重新启动任务，后续是否刷新依赖视图重建或 Store 调度。
        .task(id: entry.id) { if case .file(let file) = entry { await store.loadThumbnail(file) } }
    }
}

/// 启动时短暂展示品牌引导，随后自动进入首页。
/// 补充：该视图由 RootView 在启动后覆盖展示约 1 秒，随后淡出，仅承担品牌提示，不含业务数据。
struct IntroView: View {
    var body: some View {
        ZStack {
            Color(uiColor: .systemGroupedBackground).ignoresSafeArea()
            VStack(alignment: .leading, spacing: 18) {
                Image("Brand").resizable().scaledToFit().frame(width: 56, height: 56).clipShape(RoundedRectangle(cornerRadius: 12))
                Label("一切都在你的掌控中", systemImage: "circle.fill")
                    .font(.caption2).foregroundStyle(.secondary).padding(.horizontal, 10).padding(.vertical, 7)
                    .background(surface.opacity(0.75), in: Capsule())
                Text("安全，从这里开始。").font(.system(size: 24, weight: .semibold))
                Text("你的文件加密保存在本机，由你掌管。")
                    .font(.subheadline).foregroundStyle(.secondary).lineSpacing(5)
            }
            .frame(maxWidth: .infinity, alignment: .leading).padding(24)
            .background(mutedSurface.opacity(0.45), in: RoundedRectangle(cornerRadius: 17))
            .overlay(RoundedRectangle(cornerRadius: 17).stroke(outline))
            .padding(24).frame(maxWidth: 500)
        }
        .accessibilityElement(children: .combine)
    }
}

/// 新建空间表单：输入空间名称后创建；iOS 上存储位置固定为本机应用私有目录，不可改。
struct NewSpaceView: View {
    /// 全局业务状态中枢，创建完成后更新空间列表。
    @EnvironmentObject var store: Store
    /// 系统关闭动作，用于“取消 / 关闭”按钮。
    @Environment(\.dismiss) private var dismiss
    /// 空间名称输入。
    @State private var name = ""
    /// 名称输入框的焦点状态，用于键盘交互。
    @FocusState private var nameFocused: Bool
    var body: some View {
        NavigationStack {
            ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                VStack(alignment: .leading, spacing: 8) {
                    Text("新建空间").font(.title2.bold())
                    Text("用不同空间整理你的加密容器。").font(.subheadline).foregroundStyle(.secondary)
                }
                VStack(alignment: .leading, spacing: 8) {
                    Text("空间名称").font(.subheadline.weight(.semibold))
                    TextField("例如：工作资料", text: $name).focused($nameFocused).submitLabel(.done).onSubmit { nameFocused = false }.padding(12).background(surface, in: RoundedRectangle(cornerRadius: 8)).overlay(RoundedRectangle(cornerRadius: 8).stroke(outline))
                }
                VStack(alignment: .leading, spacing: 8) {
                    // iOS 端存储位置固定，仅作展示说明，不提供选择。
                    Text("存储位置").font(.subheadline.weight(.semibold))
                    HStack(spacing: 12) { Image(systemName: "folder").foregroundStyle(.secondary); VStack(alignment: .leading, spacing: 4) { Text("本机私有空间").font(.subheadline); Text("容器保存在此设备的应用目录中。").font(.caption).foregroundStyle(.secondary) }; Spacer() }.padding(14).background(surface, in: RoundedRectangle(cornerRadius: 9)).overlay(RoundedRectangle(cornerRadius: 9).stroke(outline))
                }
                // 底部操作行：去掉首尾空白后的名称作为最终空间名；成功且无错误时自动关闭表单。
                HStack { Spacer(); Button("取消") { dismiss() }.buttonStyle(SecondaryButton()); Button("创建空间") { let value = name.trimmingCharacters(in: .whitespacesAndNewlines); Task { await store.createSpace(value); if store.error == nil { dismiss() } } }.buttonStyle(PrimaryButton()).disabled(store.busy || name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty) }
            }.padding(24).frame(maxWidth: 560).frame(maxWidth: .infinity)
            }.scrollDismissesKeyboard(.interactively).background(Color(uiColor: .systemGroupedBackground)).navigationBarTitleDisplayMode(.inline)
                .toolbar { ToolbarItem(placement: .cancellationAction) { Button("关闭") { dismiss() } } }
                .alert("操作未完成", isPresented: Binding(get: { store.error != nil }, set: { if !$0 { store.error = nil } })) { Button("好") { store.error = nil } } message: { Text(store.error ?? "") }
        }
    }
}

/// 删除容器确认页：明确提示加密容器目录连同文件将被永久删除。
struct DeleteContainerView: View {
    /// 全局业务状态中枢，执行删除并更新界面。
    @EnvironmentObject private var store: Store
    /// 系统关闭动作。
    @Environment(\.dismiss) private var dismiss
    /// 待删除的容器摘要（名称、路径等展示信息）。
    let item: ContainerSummary

    var body: some View {
        NavigationStack {
            Form {
                Section("容器") {
                    Text(item.name)
                    Text("加密容器目录及其中的文件将被永久删除，无法恢复。")
                        .font(.footnote).foregroundStyle(.secondary)
                }
            }
            .navigationTitle("删除容器")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("取消") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("永久删除", role: .destructive) { submit() }
                        .disabled(store.busy)
                }
            }
            .tint(.red)
        }
    }

    /// 提交删除容器请求，并在 Store 报告成功后关闭确认页。
    /// - Parameters: 无；使用当前 item 容器摘要及共享 Store。
    /// - Returns: Void；忙碌时忽略，否则创建异步任务，返回 true 才执行 dismiss。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 忙碌检查不提前占用任务槽；失败由 Store 报告，本页没有自己的错误弹窗，取消关闭不等于已提交任务被撤销。
    private func submit() {
        guard !store.busy else { return }
        Task {
            if await store.deleteContainer(item) { dismiss() }
        }
    }
}

/// 容器密码表单：创建容器、解锁容器、导入加密备份三种场景共用；收集名称与密码后交给 Store。
struct ContainerFormView: View {
    /// 全局业务状态中枢，执行创建 / 解锁 / 导入并展示错误。
    @EnvironmentObject var store: Store
    /// 本次表单对应的业务场景（创建 / 解锁 / 导入加密备份）。
    let form: ContainerForm
    /// 提交成功后由父视图调用，用于收起表单。
    let dismiss: () -> Void
    /// 容器名称输入（仅创建场景使用）。
    @State private var name = ""
    /// 容器密码输入（创建时即“设置密码”，解锁 / 导入时即“解锁密码”）。
    @State private var password = ""
    /// 确认密码输入（仅创建场景使用）。
    @State private var confirm = ""
    /// 本地输入校验提示文案，非 nil 时红字展示在表单内。
    @State private var validation: String?
    /// 三个输入框的枚举标识，供焦点管理与滚动定位使用。
    private enum InputField: Hashable { case name, password, confirm }
    /// 当前聚焦的输入框。
    @FocusState private var focused: InputField?
    var body: some View {
        NavigationStack {
            ScrollViewReader { proxy in
            ScrollView {
                VStack(spacing: 22) {
                    VStack(spacing: 12) {
                        Image(systemName: form.isCreate ? "plus" : "lock.open").font(.title2).foregroundStyle(.secondary).frame(width: 56, height: 56).background(mutedSurface, in: RoundedRectangle(cornerRadius: 15))
                        Text(form.isCreate ? "NEW CONTAINER" : "OPEN CONTAINER").font(.system(size: 10, weight: .bold)).tracking(1.4).foregroundStyle(.secondary)
                        Text(form.isCreate ? "创建新容器" : form.title).font(.title2.bold())
                        Text(form.isCreate ? "设置密码，为重要文件创建一个私密空间。" : "输入密码，打开你的私密空间。 ").font(.subheadline).foregroundStyle(.secondary).multilineTextAlignment(.center)
                    }.padding(.top, 16)
                    VStack(alignment: .leading, spacing: 18) {
                        // 创建场景：先填名称；解锁 / 导入场景：展示容器名称与所属空间。
                        if form.isCreate { field("容器名称") { TextField("例如：个人文件", text: $name).textInputAutocapitalization(.never).focused($focused, equals: .name).submitLabel(.next).onSubmit { focused = .password } }.id(InputField.name) }
                        else { HStack { Image(systemName: "checkmark.shield").foregroundStyle(.secondary); VStack(alignment: .leading, spacing: 5) { Text(form.detail).font(.subheadline.weight(.semibold)); Text("本机 / \(store.space?.name ?? "空间")").font(.caption).foregroundStyle(.secondary) }; Spacer() }.padding(14).background(mutedSurface, in: RoundedRectangle(cornerRadius: 9)) }
                        // 密码输入：创建场景提示“设置密码”，解锁场景提示“输入密码解锁”。
                        field("容器密码") { SecureField(form.isCreate ? "设置容器密码" : "输入密码解锁", text: $password).textContentType(form.isCreate ? .newPassword : .password).focused($focused, equals: .password).submitLabel(form.isCreate ? .next : .done).onSubmit { focused = form.isCreate ? .confirm : nil } }.id(InputField.password)
                        // 确认密码：仅创建场景需要二次输入，防止打错。
                        if form.isCreate { field("确认密码") { SecureField("再次输入密码", text: $confirm).textContentType(.newPassword).focused($focused, equals: .confirm).submitLabel(.done).onSubmit { focused = nil } }.id(InputField.confirm) }
                        // 本地校验提示，直接显示在输入区下方。
                        if let validation { Text(validation).font(.footnote).foregroundStyle(.red) }
                        Button {
                            // 提交前先做本地校验：密码必填；创建还需名称合法且两次密码一致。
                            guard !password.isEmpty else { validation = "请输入密码"; return }
                            guard !form.isCreate || (!name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && password == confirm) else { validation = "请输入名称并确认两次密码一致"; return }
                            // Store 报告成功时移除表单持有的密码文本并关闭；普通 String 置空不保证所有内存副本被擦除。
                            Task { if await store.open(form: form, name: name.trimmingCharacters(in: .whitespacesAndNewlines), password: password) { password = ""; confirm = ""; dismiss() } }
                        } label: { HStack { Spacer(); Text(form.isCreate ? "创建容器" : "解锁容器"); Spacer(); Image(systemName: "arrow.right") } }.buttonStyle(PrimaryButton()).disabled(store.busy)
                    }.padding(20).background(surface, in: RoundedRectangle(cornerRadius: 16)).overlay(RoundedRectangle(cornerRadius: 16).stroke(outline))
                    Label("密码用于本地解锁容器，请妥善保管。", systemImage: "lock").font(.caption).foregroundStyle(.secondary)
                }.padding(20).frame(maxWidth: 560).frame(maxWidth: .infinity)
            }
            .scrollDismissesKeyboard(.interactively)
            // 焦点切换后把对应输入框滚动到可视区域中央（小屏手机上防止被键盘遮挡）。
            .onChange(of: focused) { _, field in
                guard let field else { return }
                Task { @MainActor in
                    try? await Task.sleep(for: .milliseconds(250))
                    guard focused == field else { return }
                    withAnimation { proxy.scrollTo(field, anchor: .center) }
                }
            }
            }.background(Color(uiColor: .systemGroupedBackground)).navigationTitle(form.title).navigationBarTitleDisplayMode(.inline)
                .toolbar { ToolbarItem(placement: .cancellationAction) { Button("取消", action: dismiss).disabled(store.busy) } }
                .alert("操作未完成", isPresented: Binding(get: { store.error != nil }, set: { if !$0 { store.error = nil } })) { Button("好") { store.error = nil } } message: { Text(store.error ?? "") }
        }
    }
    /// 为容器表单输入组合统一标题、背景和边框。
    /// - Parameters: title: String 用户可见字段标题；content: () -> Content 输入控件构建闭包，Content 符合 View。
    /// - Returns: some View，包含标题和调用方输入控件的纵向字段布局。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 仅负责外观，不自行进行密码校验、焦点管理或忙碌禁用。
    func field<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.subheadline.weight(.semibold))
            content().padding(12).background(Color(uiColor: .systemBackground), in: RoundedRectangle(cornerRadius: 8)).overlay(RoundedRectangle(cornerRadius: 8).stroke(outline))
        }
    }
}
/// 文件预览容器：按内容类型分发到图片 / 视频 / 文本三个预览实现。
struct PreviewView: View {
    /// 待预览的条目：包含文件信息、解密后的本地内容位置与类型。
    let item: PreviewItem
    /// 系统关闭动作。
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        NavigationStack {
            Group {
                switch item.content {
                case .image(let url): ImagePreview(url: url)
                case .video(let url): VideoPreview(url: url)
                case .text(let text):
                    ReadOnlyTextView(text: text)
                }
            }
            .navigationTitle(item.file.leafName).navigationBarTitleDisplayMode(.inline)
            .toolbar { Button("关闭") { dismiss() } }
        }
    }
}

/// 只读文本视图：基于 UITextView，自带滚动与文本选择，
/// 用于展示已完整读入内存的文本；控件本身不设置长度上限，大文件仍会占用内存。
/// 补充：文本内容已在 Store.show 中解密并解码，这里只负责显示，不做任何修改。
private struct ReadOnlyTextView: UIViewRepresentable {
    /// 要展示的完整文本内容。
    let text: String
    /// 功能：为明文文本预览建立可选择、可滚动但不可编辑的原生文本视图。
    /// - Parameter context: UIViewRepresentable.Context，SwiftUI 提供的 UIKit 桥接上下文。
    /// - Returns: 配置了等宽字体、透明背景及内边距的 UITextView。
    /// - Throws: 不抛出错误。
    /// - Note: 首次创建不填充 text；最新文本由 updateUIView 同步，视图操作遵循主线程约束。
    func makeUIView(context: Context) -> UITextView {
        let view = UITextView()
        view.isEditable = false
        view.isSelectable = true
        view.font = .monospacedSystemFont(ofSize: 15, weight: .regular)
        view.backgroundColor = .clear
        view.textContainerInset = UIEdgeInsets(top: 16, left: 16, bottom: 16, right: 16)
        view.alwaysBounceVertical = true
        return view
    }
    /// 将最新只读预览文本同步到已创建的原生文本控件。
    /// - Parameters: view: UITextView 当前原生视图；context: Context SwiftUI 桥接上下文，本实现不使用。
    /// - Returns: Void；文字相同时保持原状，不同时替换为当前 text。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 主线程更新；同步整份字符串，没有大小截断，内容改变时可能影响原有选择或滚动位置。
    func updateUIView(_ view: UITextView, context: Context) {
        // 文本变化时更新内容；相同文本不重复赋值，避免打断用户滚动与选择。
        if view.text != text { view.text = text }
    }
}

/// 图片预览：支持双指缩放（1–6 倍）、单指拖动平移（放大后）与双击放大 / 复原。
private struct ImagePreview: View {
    /// 解密后图片文件的本地临时位置。
    let url: URL
    /// 当前缩放倍数（持久值，手势结束后保留）。
    @State private var scale: CGFloat = 1
    /// 放大后平移的累计偏移量。
    @State private var offset: CGSize = .zero
    /// 双指捏合手势的实时缩放值（手势期间临时叠加，结束后并入 scale）。
    @GestureState private var pinch: CGFloat = 1
    /// 单指拖动手势的实时位移（手势期间临时叠加，结束后并入 offset）。
    @GestureState private var drag: CGSize = .zero
    var body: some View {
        GeometryReader { geometry in
            // 解密临时文件读不出图片时（格式不支持或已清理）展示兜底视图。
            if let image = UIImage(contentsOfFile: url.path) {
                Image(uiImage: image).resizable().scaledToFit()
                    .frame(width: geometry.size.width, height: geometry.size.height)
                    .scaleEffect(scale * pinch)
                    .offset(x: offset.width + drag.width, y: offset.height + drag.height)
                    .gesture(MagnificationGesture().updating($pinch) { value, state, _ in state = value }
                        .onEnded { value in
                            // 手势结束：把增量并入持久缩放倍数，并限制在 1–6 倍。
                            scale = min(max(scale * value, 1), 6)
                            // 缩回 1 倍时复位平移量。
                            if scale == 1 { offset = .zero }
                        })
                    .simultaneousGesture(DragGesture().updating($drag) { value, state, _ in
                        // 只有放大后才允许拖动平移，避免与页面手势冲突。
                        if scale > 1 { state = value.translation }
                    }.onEnded { value in
                        guard scale > 1 else { return }
                        // 累计平移量并夹紧在可平移范围内，防止图片被拖出可视区域。
                        offset.width += value.translation.width; offset.height += value.translation.height
                        let limitX = geometry.size.width * (scale - 1) / 2
                        let limitY = geometry.size.height * (scale - 1) / 2
                        offset.width = min(max(offset.width, -limitX), limitX)
                        offset.height = min(max(offset.height, -limitY), limitY)
                    })
                    // 双击在 1 倍与 2 倍之间切换，并复位平移。
                    .onTapGesture(count: 2) { withAnimation { scale = scale == 1 ? 2 : 1; offset = .zero } }
                    .clipped()
            } else {
                ContentUnavailableView("无法预览图片", systemImage: "photo", description: Text("图片格式可能不受支持。"))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }.background(Color.black.opacity(0.95)).accessibilityLabel("图片预览，可双指缩放")
    }
}

/// 视频预览：用 AVPlayer 播放解密后的视频临时文件；不可播放时给出导出建议。
private struct VideoPreview: View {
    /// 解密后视频文件的本地临时位置。
    let url: URL
    /// 播放器实例，加载成功后创建。
    @State private var player: AVPlayer?
    /// 是否判定为不可播放（格式不支持等）。
    @State private var failed = false
    var body: some View {
        Group {
            if failed {
                ContentUnavailableView("无法播放视频", systemImage: "video.slash", description: Text("视频格式可能不受支持，可导出后使用其他应用打开。"))
            } else if let player { VideoPlayer(player: player) }
            else { ProgressView().tint(.white) }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity).background(Color.black)
        .task {
            // 异步探测文件是否可播放，避免直接初始化播放器造成的卡顿。
            let asset = AVURLAsset(url: url)
            do {
                let playable = try await asset.load(.isPlayable)
                guard !Task.isCancelled else { return }
                if playable { player = AVPlayer(playerItem: AVPlayerItem(asset: asset)) }
                else { failed = true }
            } catch { AppLog.capture(error); if !Task.isCancelled { failed = true } }
        }
        // 关闭预览时停止播放并释放播放器，防止后台继续出声与占用临时文件。
        .onDisappear { player?.pause(); player?.replaceCurrentItem(with: nil); player = nil }
    }
}
