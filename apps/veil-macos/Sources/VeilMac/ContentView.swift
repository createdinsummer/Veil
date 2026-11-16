/// macOS 主窗口、侧栏、首页和传输状态提示的组合界面。
/// 所属模块：桌面端应用导航与反馈；把 AppStore 中的业务状态呈现为窗口、弹窗和操作入口。
/// 核心内容：ContentView 选择容器表单 / 文件浏览 / 首页；SidebarView 导航空间与容器；TransferProgressView 显示传输过程。
/// 依赖 SwiftUI 窗口与弹窗、AppKit 保存面板和访达选择，以及 AppStore 的异步操作。整体目标是集中呈现本机容器管理流程，并让关键操作有明确反馈。
import AppKit
import SwiftUI

/// 主窗口根视图，根据当前业务状态切换容器表单、文件浏览器和欢迎页。
struct ContentView: View {
    /// 跨窗口共享的容器、空间、任务与错误状态来源。
    @EnvironmentObject private var store: AppStore
    /// 打开任务中心等独立窗口的系统动作。
    @Environment(\.openWindow) private var openWindow
    /// 打开系统设置窗口的动作。
    @Environment(\.openSettings) private var openSettings
    /// 当前是否显示左侧空间与容器导航栏。
    @State private var sidebarVisible = true

    /// 绑定主界面、后台刷新、冲突复核和删除确认等窗口级流程。
    var body: some View {
        content
            .task(id: store.notice) {
                guard let notice = store.notice else { return }
                do { try await Task.sleep(for: .seconds(3)) } catch { return }
                if store.notice == notice { store.notice = nil }
            }
            // 视图任务每次先等 15 秒，再刷新空间可用性；上一次刷新耗时也计入两次开始之间的间隔。
            .task {
                while !Task.isCancelled {
                    do { try await Task.sleep(for: .seconds(15)) } catch { return }
                    await store.refreshSpaceAvailability()
                }
            }
            .sheet(item: $store.importInspection) { inspection in
                ImportConflictReview(inspection: inspection, operationLabel: store.conflictOperation, progressText: store.importProgressDescription) { store.completeImportReview($0, id: inspection.id) }
                    .interactiveDismissDisabled()
                    // 弹窗退出兜底请求中止；Store 按批次 ID 检查，已提交或过期批次不会再次恢复同一等待。
                    .onDisappear { store.completeImportReview(nil, id: inspection.id) }
            }
            .sheet(isPresented: $store.showImportPackage) { ImportPackageView().environmentObject(store) }
            .sheet(isPresented: $store.showNewSpace) { NewSpaceView().environmentObject(store) }
            .sheet(item: $store.preview, onDismiss: { store.closePreview() }) { asset in PreviewSheet(asset: asset).environmentObject(store) }
            .deleteEntryAlert(store: store)
            .deleteSpaceAlert(store: store)
            .deleteContainerAlert(store: store)
    }

    /// 主内容与浮层提示；独立拆出以控制单个表达式的类型检查复杂度。
    private var content: some View {
        HStack(spacing: 0) {
            if sidebarVisible {
                SidebarView().frame(width: VeilDesign.layout("sidebar"))
                Rectangle().fill(VeilTheme.border).frame(width: 1)
            }
            VStack(spacing: 0) {
                header
                Rectangle().fill(VeilTheme.border).frame(height: 1)
                if store.form == nil, !store.showNewSpace, let error = store.error {
                    FeedbackBanner(
                        message: error,
                        isError: true,
                        action: FeedbackBanner.Action(title: "导出错误日志…", run: exportErrorLogs)
                    ) { store.error = nil }
                }
                if let form = store.form { ContainerFormView(form: form).id(form.id) }
                else if let container = store.container { BrowserView(container: container) }
                else { HomeView() }
            }.frame(maxWidth: .infinity, maxHeight: .infinity).background(VeilTheme.canvas)
        }
        .foregroundStyle(VeilTheme.text)
        .ignoresSafeArea(.container, edges: .top)
        .overlay(alignment: .bottomTrailing) {
            if let notice = store.notice {
                FeedbackBanner(message: notice, isError: false) { store.notice = nil }
                    .frame(maxWidth: 340)
                    .clipShape(RoundedRectangle(cornerRadius: 10))
                    .overlay(RoundedRectangle(cornerRadius: 10).stroke(VeilTheme.border))
                    .shadow(color: .black.opacity(0.08), radius: 12, y: 4)
                    .padding(.trailing, 20).padding(.bottom, 20)
                    .accessibilityAddTraits(.updatesFrequently)
            }
        }
        .overlay {
            if let status = store.decryptStatus {
                DecryptOverlay(status: status)
                    .transition(.opacity)
            }
        }
        .animation(.easeInOut(duration: 0.15), value: store.decryptStatus)
    }

    /// 主窗口顶栏：展示当前位置，并提供当前上下文可用的容器操作。
    private var header: some View {
        HStack(spacing: 10) {
            Button {
                withAnimation(.easeInOut(duration: 0.18)) { sidebarVisible.toggle() }
            } label: { Image(systemName: "sidebar.left") }
                .buttonStyle(VeilIconButtonStyle())
                .help(sidebarVisible ? "收起侧栏" : "展开侧栏")
                .accessibilityLabel(sidebarVisible ? "收起侧栏" : "展开侧栏")
            Text(store.activeSpace?.name ?? "本机").foregroundStyle(VeilTheme.secondary)
            Text("/").foregroundStyle(VeilTheme.muted)
            Text(store.form?.title ?? store.container?.name ?? "我的容器").lineLimit(1)
            Spacer(minLength: 12)
            if store.isBusy { ProgressView().controlSize(.small).accessibilityLabel("正在处理") }
            if store.container != nil {
                Button { Task { await store.exportPackage() } } label: { Label("加密备份", systemImage: "externaldrive") }
                    .buttonStyle(VeilPlainButtonStyle()).disabled(store.isBusy)
                Button { Task { await store.refresh() } } label: { Image(systemName: "arrow.clockwise") }
                    .buttonStyle(VeilIconButtonStyle()).help("刷新").disabled(!store.canBrowse)
                Button(action: exportErrorLogs) { Image(systemName: "arrow.up.doc") }
                    .buttonStyle(VeilIconButtonStyle()).help("导出错误日志…").accessibilityLabel("导出错误日志")
                Button { Task { await store.lock(forgetPassword: true) } } label: { Label("锁定", systemImage: "lock") }
                    .buttonStyle(VeilPlainButtonStyle()).help("锁定容器并清除免密密码")
            } else {
                Label("本地模式", systemImage: "lock").font(.system(size: 11)).foregroundStyle(VeilTheme.muted)
                Menu {
                    Button("新建容器…") { Task { await store.newContainer() } }.disabled(store.activeSpace?.isAvailable != true)
                    Button("打开容器…") { Task { await store.chooseContainerDirectory() } }
                    Button("导入加密容器…") { Task { await store.choosePackage() } }.disabled(store.activeSpace?.isAvailable != true)
                    Divider()
                    Button("刷新") { Task { await store.refresh() } }
                    Divider()
                    Button("导出错误日志…") { exportErrorLogs() }
                    Divider()
                    Button("设置…") { openSettings() }
                } label: { Image(systemName: "ellipsis") }
                    .menuStyle(.borderlessButton).fixedSize().disabled(store.isBusy).pointerCursor().help("容器操作")
            }
            if let title = store.transferTitle {
                Button { openWindow(id: VeilWindowID.taskCenter) } label: {
                    Label(store.transferProgress?.percentageText ?? "任务", systemImage: "checklist")
                }
                .buttonStyle(VeilPlainButtonStyle())
                .help("查看任务进度：\(title)")
                .accessibilityLabel("查看任务进度")
            }
        }.font(.system(size: 12))
            .padding(.horizontal, store.container == nil ? 32 : 24)
            .padding(.leading, sidebarVisible ? 0 : 76)
            .frame(height: store.container == nil ? VeilDesign.layout("header") : 52)
    }

    /// 在主窗口错误反馈处提供本机日志导出入口。
    /// - Parameters: 无。
    /// - Returns: Void；显示保存面板，确认且有 URL 后请求 Store 导出，取消无操作。
    /// - Throws: 不向调用方抛错，导出错误由 Store 报告。
    /// - 注意事项: 主线程调用；回调时才读取日志，面板等待期间日志仍可增加；本方法不直接执行文件写入。
    private func exportErrorLogs() {
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.json]
        panel.nameFieldStringValue = "Veil-错误日志.json"
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            store.exportErrorLogs(to: url)
        }
    }
}

/// 将文件、空间和容器删除确认拆为独立视图修饰符，统一绑定状态并降低主视图表达式的复杂度。
private extension View {
    /// 为当前视图附加文件或目录的删除确认流程。
    /// - Parameters: store: AppStore，提供 pendingDelete 条目及确认后的异步删除动作。
    /// - Returns: some View，绑定待删状态、按条目类型显示影响说明的视图。
    /// - Throws: 不向调用方抛出异常，删除失败由 Store 处理。
    /// - 注意事项: 取消或关闭清除待删条目；确认后创建任务，不能保证后台调度时容器状态未变化；删除不是在此同步完成。
    func deleteEntryAlert(store: AppStore) -> some View {
        alert("删除「\(store.pendingDelete?.name ?? "")」？", isPresented: Binding(
            get: { store.pendingDelete != nil }, set: { if !$0 { store.pendingDelete = nil } }
        ), presenting: store.pendingDelete) { target in
            Button("删除", role: .destructive) { Task { await store.confirmDelete(target) } }
            Button("取消", role: .cancel) { store.pendingDelete = nil }
        } message: { entry in
            switch entry {
            case .folder: Text("此文件夹及其全部文件将从容器中删除。此操作无法撤销。")
            case .file: Text("此文件将从容器中删除。此操作无法撤销。")
            }
        }
    }

    /// 按空间可用性展示删除磁盘内容或仅删除登记的确认说明。
    /// - Parameters: store: AppStore，提供 pendingSpaceDelete 空间快照和空间删除入口。
    /// - Returns: some View，附加空间确认弹窗的视图。
    /// - Throws: 不向调用方抛错，删除错误由 Store 报告。
    /// - 注意事项: 使用弹窗空间快照的 isAvailable 选择流程；不可用空间请求删除登记，可用空间请求永久删除。确认先清除待删状态，再异步执行，不保证此时磁盘状态仍与快照一致。
    func deleteSpaceAlert(store: AppStore) -> some View {
        alert("\(store.pendingSpaceDelete?.isAvailable == true ? "删除空间" : "删除空间记录")「\(store.pendingSpaceDelete?.name ?? "")」？", isPresented: Binding(
            get: { store.pendingSpaceDelete != nil }, set: { if !$0 { store.pendingSpaceDelete = nil } }
        ), presenting: store.pendingSpaceDelete) { space in
            Button(space.isAvailable ? "删除空间" : "删除记录", role: .destructive) { store.pendingSpaceDelete = nil; Task { await store.deleteSpace(space) } }
            Button("取消", role: .cancel) { store.pendingSpaceDelete = nil }
        } message: { space in
            if space.isAvailable {
                Text(space.isDefault ? "将永久删除整个空间文件夹、其中的容器和其他文件。完成后会自动重新建立一个空的默认空间。无法撤销。" : "将永久删除整个空间文件夹、其中的容器和其他文件。无法撤销。")
            } else {
                Text("删除后，Veil 中的空间和容器记录无法恢复。磁盘上的空间文件和容器文件不会被删除。")
            }
        }
    }

    /// 为已登记容器附加无需密码的永久删除确认。
    /// - Parameters: store: AppStore，提供 pendingContainerDelete 记录及删除入口。
    /// - Returns: some View，附加容器删除弹窗的视图。
    /// - Throws: 不向调用方抛错，实际删除失败由 Store 处理。
    /// - 注意事项: 用户确认才创建任务，取消只清除待删状态；确认并不代表磁盘删除成功，服务端仍执行权限和资源检查。
    func deleteContainerAlert(store: AppStore) -> some View {
        alert("永久删除容器「\(store.pendingContainerDelete?.name ?? "")」？", isPresented: Binding(
            get: { store.pendingContainerDelete != nil }, set: { if !$0 { store.pendingContainerDelete = nil } }
        ), presenting: store.pendingContainerDelete) { item in
            Button("永久删除", role: .destructive) { Task { await store.deleteContainer(item) } }
            Button("取消", role: .cancel) { store.pendingContainerDelete = nil }
        } message: { _ in
            Text("加密容器目录及其中的文件将被永久删除。此操作不需要容器密码，无法撤销。")
        }
    }
}

/// 在主窗口中显示导入、导出或容器打包任务的简要进度。
struct TransferProgressView: View {
    /// 只用于提交取消请求；进度展示读取调用方传入的快照，不在视图中轮询后端。
    @EnvironmentObject private var store: AppStore
    /// 当前任务的用户可读标题。
    let title: String
    /// 最新任务快照；尚未生成时为空，界面显示准备状态。
    let progress: TransferProgress?

    /// 把内部阶段码翻译为用户能理解的当前处理环节。
    private var stage: String {
        if progress?.cancelRequested == true { return "正在中止" }
        return switch progress?.phase {
        case "snapshot": "正在创建快照"
        case "packing": "正在打包"
        case "unpacking": "正在解包"
        case "verifying": "正在核对"
        case "processing": "正在处理"
        case "finalizing": "正在完成"
        default: "正在准备"
        }
    }

    /// 把任务快照中的阶段、文件 / 字节进度和取消能力转换为紧凑浮层。
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(title).fontWeight(.semibold)
                Spacer()
                if let progress, ["import", "export"].contains(progress.operation), progress.totalFiles > 0 {
                    Text("\(Int(min(1, Double(progress.completedFiles) / Double(progress.totalFiles)) * 100))%")
                        .monospacedDigit().foregroundStyle(VeilTheme.secondary)
                } else if let progress, let total = progress.totalBytes, total > 0 {
                    Text("\(Int(min(1, Double(progress.processedBytes) / Double(total)) * 100))%")
                        .monospacedDigit().foregroundStyle(VeilTheme.secondary)
                }
            }
            // 普通传输优先按文件数计算，扫描中分母仍会增长；其他任务使用字节数，均截断上限为 100%。
            if let progress, ["import", "export"].contains(progress.operation), progress.totalFiles > 0 {
                ProgressView(value: min(1, Double(progress.completedFiles) / Double(progress.totalFiles)))
                Text(progress.scanComplete ? "文件遍历完成" : "正在发现文件，总数持续增加")
                    .foregroundStyle(VeilTheme.secondary)
                if progress.waitingFiles > 0 { Text("待选择：\(progress.waitingFiles) 个文件").foregroundStyle(VeilTheme.secondary) }
            } else if let progress, let total = progress.totalBytes, total > 0 {
                ProgressView(value: min(1, Double(progress.processedBytes) / Double(total)))
                Text("\(stage) · \(ByteCountFormatter.string(fromByteCount: Int64(clamping: progress.processedBytes), countStyle: .file)) / \(ByteCountFormatter.string(fromByteCount: Int64(clamping: total), countStyle: .file))")
                    .foregroundStyle(VeilTheme.secondary)
            } else {
                ProgressView().controlSize(.small)
                Text(["import", "export"].contains(progress?.operation ?? "") ? "正在发现文件…" : "\(stage)…").foregroundStyle(VeilTheme.secondary)
            }
            if let progress, progress.totalFiles > 0 {
                Text("已完成 \(progress.completedFiles) / \(progress.totalFiles) 个文件" + (progress.phase == "verifying" ? " · 正在核对" : ""))
                    .foregroundStyle(VeilTheme.secondary)
                if progress.operation == "import", progress.pendingFiles > 0 {
                    Text("已加密，等待提交：\(progress.pendingFiles) 个文件").foregroundStyle(VeilTheme.secondary)
                }
                if progress.parallelWorkers > 1 {
                    Text("最多同时处理 \(progress.parallelWorkers) 个文件").foregroundStyle(VeilTheme.secondary)
                } else if let name = progress.currentFile {
                    Text(name).lineLimit(1).truncationMode(.middle).foregroundStyle(VeilTheme.secondary)
                }
            }
            // 这里只对普通导入/导出且阶段在白名单内显示取消；按钮文案固定为“中止导入”，动作仍取消当前传输。
            // cancelRequested 只是已发出请求，不能据此断言工作线程已经停止或已完成文件被回滚。
            if let progress, ["import", "export"].contains(progress.operation), ["preparing", "processing", "verifying"].contains(progress.phase) {
                Button(progress.cancelRequested ? "正在中止…" : "中止导入") { store.cancelTransfer() }
                    .buttonStyle(VeilButtonStyle()).disabled(progress.cancelRequested)
            }
        }
        .font(.system(size: 12)).padding(14).frame(width: 300)
        .background(VeilTheme.canvas).clipShape(RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(VeilTheme.border))
        .shadow(color: .black.opacity(0.1), radius: 12, y: 4)
        .accessibilityElement(children: .contain)
    }
}

/// 左侧空间 / 容器导航栏，允许展开空间、切换容器并打开任务中心与设置。
private struct SidebarView: View {
    /// 注册空间及其中容器的共享快照，同时承接选择、创建和删除确认请求。
    @EnvironmentObject private var store: AppStore
    /// 系统窗口动作，用于显示任务中心，不创建或恢复传输任务。
    @Environment(\.openWindow) private var openWindow
    /// 当前折叠的空间 ID 集合；遍历空间时据此决定是否显示其容器。
    @State private var collapsed: Set<String> = []

    /// 按空间和容器层级显示导航，并在每次点击时调用 Store 更新当前目标。
    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                if let url = Bundle.module.url(forResource: "Brand", withExtension: "png"), let image = NSImage(contentsOf: url) {
                    Image(nsImage: image).resizable().interpolation(.high).frame(width: 27, height: 27)
                }
                Text("veil").font(.system(size: 17, weight: .semibold, design: .rounded)).tracking(-0.3)
                Spacer()
            }.padding(.horizontal, 21).padding(.top, 48).padding(.bottom, 34)
            Text("空间").font(.system(size: 11, weight: .semibold)).foregroundStyle(VeilTheme.secondary)
                .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 23).padding(.bottom, 8)
            ScrollView {
                VStack(spacing: 2) {
                    // 每轮一个注册空间，包含可用性和嵌套容器快照；离线空间仍可选择或删除登记。
                    ForEach(store.spaces) { space in
                        VStack(spacing: 2) {
                            HStack(spacing: 0) {
                                Button {
                                    if collapsed.contains(space.id) { collapsed.remove(space.id) }
                                    else { collapsed.insert(space.id) }
                                } label: {
                                    Image(systemName: collapsed.contains(space.id) ? "chevron.right" : "chevron.down")
                                        .font(.system(size: 8, weight: .semibold)).frame(width: 28, height: 34)
                                }.buttonStyle(VeilPlainButtonStyle()).foregroundStyle(VeilTheme.secondary)
                                    .accessibilityLabel(collapsed.contains(space.id) ? "展开\(space.name)" : "收起\(space.name)")
                                Button { Task { await store.selectSpace(space) } } label: {
                                    HStack(spacing: 8) {
                                        Image(systemName: "folder").font(.system(size: 14)).foregroundStyle(VeilTheme.secondary)
                                        Text(space.name).font(.system(size: 12)).lineLimit(1)
                                        if !space.isAvailable {
                                            Text("不可用").font(.system(size: 9, weight: .medium))
                                                .foregroundStyle(VeilTheme.muted)
                                                .padding(.horizontal, 5).padding(.vertical, 2)
                                                .background(VeilTheme.thumbnail, in: Capsule())
                                        }
                                        Spacer(minLength: 0)
                                    }.padding(.vertical, 8).contentShape(Rectangle())
                                }.buttonStyle(VeilPlainButtonStyle())
                                Button { Task { await store.newContainer(in: space.id) } } label: { Image(systemName: "plus") }
                                    .buttonStyle(VeilIconButtonStyle()).help("在\(space.name)中创建容器")
                                    .disabled(!space.isAvailable)
                            }
                            .background(store.container == nil && store.activeSpace?.id == space.id ? VeilTheme.selected : .clear, in: RoundedRectangle(cornerRadius: 7))
                            if !collapsed.contains(space.id) {
                                // 当前空间未折叠时，每轮一个容器摘要；是否选中按当前容器 ID 比较，点击再交给 Store 处理解锁。
                                ForEach(space.containers) { item in SidebarContainerRow(item: item, selected: store.container?.id == item.id) }
                            }
                        }
                        .contextMenu {
                            Button(space.isAvailable ? "删除空间" : "删除空间记录", systemImage: "trash", role: .destructive) {
                                store.pendingSpaceDelete = space
                            }
                        }
                    }
                    Button { store.error = nil; store.showNewSpace = true } label: {
                        Label("新建空间", systemImage: "plus").font(.system(size: 12))
                            .foregroundStyle(VeilTheme.secondary).frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.leading, 25).padding(.vertical, 9).contentShape(Rectangle())
                    }.buttonStyle(VeilPlainButtonStyle()).padding(.top, 2)
                }.padding(.horizontal, 13).disabled(store.isBusy)
            }.scrollIndicators(.hidden)
            VStack(alignment: .leading, spacing: 8) {
                Rectangle().fill(VeilTheme.border).frame(height: 1).padding(.bottom, 4)
                Button { openWindow(id: VeilWindowID.taskCenter) } label: {
                    HStack(spacing: 8) {
                        Image(systemName: "checklist").font(.system(size: 13)).frame(width: 16)
                        Text("任务中心").font(.system(size: 12))
                        Spacer(minLength: 4)
                        if store.transferTitle != nil {
                            // 固定 1 表示 Store 当前公开的一组传输状态，不是统计所有后台任务的真实数量。
                            HStack(spacing: 4) {
                                Text("1")
                                if let percentage = store.transferProgress?.percentageText { Text(percentage) }
                            }
                            .font(.system(size: 10, weight: .medium)).monospacedDigit()
                            .foregroundStyle(VeilTheme.secondary)
                            .padding(.horizontal, 7).padding(.vertical, 3)
                            .background(VeilTheme.thumbnail, in: Capsule())
                        }
                    }
                    .foregroundStyle(VeilTheme.secondary)
                    .padding(.horizontal, 10).frame(maxWidth: .infinity, minHeight: 34, alignment: .leading)
                    .contentShape(Rectangle())
                    .background(store.transferTitle != nil ? VeilTheme.selected : .clear, in: RoundedRectangle(cornerRadius: 7))
                }
                .buttonStyle(VeilPlainButtonStyle())
                SettingsLink {
                    HStack(spacing: 8) {
                        Image(systemName: "gearshape").font(.system(size: 13)).frame(width: 16)
                        Text("设置").font(.system(size: 12))
                        Spacer(minLength: 4)
                    }
                    .foregroundStyle(VeilTheme.secondary)
                    .padding(.horizontal, 10).frame(maxWidth: .infinity, minHeight: 34, alignment: .leading)
                    .contentShape(Rectangle())
                }
                .buttonStyle(VeilPlainButtonStyle())
                .accessibilityLabel("打开设置")
                Label("本机", systemImage: "lock").font(.system(size: 11)).foregroundStyle(VeilTheme.muted)
            }.padding(.horizontal, 23).padding(.bottom, 20).padding(.top, 12)
        }.frame(maxWidth: .infinity, maxHeight: .infinity).background(VeilTheme.sidebar)
    }
}

/// 侧栏中的单个容器入口，点击后请求 Store 打开该容器。
private struct SidebarContainerRow: View {
    /// 负责选择容器及登记删除确认目标的共享状态；本行不直接删除文件。
    @EnvironmentObject private var store: AppStore
    /// 当前行代表的容器摘要，包含标识、名称和磁盘路径。
    let item: ContainerSummary
    /// 当前容器是否已在主窗口中选中。
    let selected: Bool
    /// 鼠标悬停的界面状态，只影响背景或边框，不表示容器已选中或解锁。
    @State private var hovering = false
    /// 展示并打开单个容器，选中状态与鼠标悬停状态只影响呈现。
    var body: some View {
        Button { Task { await store.chooseContainer(item) } } label: {
            HStack(spacing: 8) {
                Image(systemName: "checkmark.shield").font(.system(size: 13)).foregroundStyle(VeilTheme.secondary)
                Text(item.name).font(.system(size: 12)).lineLimit(1)
                Spacer(minLength: 0)
            }.padding(.leading, 33).padding(.trailing, 8).frame(minHeight: 32).contentShape(Rectangle())
                .background(selected ? VeilTheme.selected : hovering ? VeilTheme.hover : .clear, in: RoundedRectangle(cornerRadius: 7))
        }.buttonStyle(VeilPlainButtonStyle()).onHover { hovering = $0 }.help(item.path)
            .accessibilityAddTraits(selected ? .isSelected : [])
            .contextMenu {
                Button("删除容器", systemImage: "trash", role: .destructive) {
                    store.pendingContainerDelete = item
                }
            }
    }
}

/// 尚未进入容器时的首页，展示当前空间、容器卡片和最近打开记录。
private struct HomeView: View {
    /// 提供当前空间、已注册容器及外部最近打开记录的共享状态。
    @EnvironmentObject private var store: AppStore
    /// 两项弹性列配置组成固定两列网格；同一规则用于当前空间容器和外部最近记录。
    private let columns = [GridItem(.flexible()), GridItem(.flexible())]

    /// 汇总当前空间状态、容器卡片和最近打开记录，并提供创建 / 导入入口。
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 27) {
                HStack(alignment: .bottom) {
                    VStack(alignment: .leading, spacing: 10) {
                        Text("YOUR PRIVATE SPACE").font(.system(size: 10, weight: .semibold)).tracking(1.6).foregroundStyle(VeilTheme.muted)
                        Text("我的容器").font(.system(size: VeilDesign.font("title"), weight: .bold)).tracking(-0.8).foregroundStyle(VeilTheme.heading)
                        Text("\(store.activeSpace?.name ?? "本机") · 为重要文件留一个安心的地方。")
                            .font(.system(size: 13)).foregroundStyle(VeilTheme.secondary)
                    }
                    Spacer()
                    Button("新建容器", systemImage: "plus") { Task { await store.newContainer() } }
                        .buttonStyle(VeilButtonStyle(variant: .primary)).disabled(store.isBusy || store.activeSpace?.isAvailable != true)
                }
                if let space = store.activeSpace {
                    HStack(spacing: 11) {
                        Image(systemName: space.isAvailable ? "externaldrive" : "externaldrive.badge.xmark")
                            .font(.system(size: 18)).foregroundStyle(VeilTheme.muted)
                        VStack(alignment: .leading, spacing: 4) {
                            Text(space.isAvailable ? "空间可用" : "空间当前不可用")
                                .font(.system(size: 12, weight: .semibold))
                            HStack(spacing: 4) {
                                Text(space.isAvailable ? "空间目录：" : "请重新挂载外置磁盘，或检查空间目录：")
                                    .font(.system(size: 11)).foregroundStyle(VeilTheme.muted)
                                Button {
                                    NSWorkspace.shared.activateFileViewerSelecting([
                                        URL(fileURLWithPath: space.path, isDirectory: true)
                                    ])
                                } label: {
                                    Text(space.path).font(.system(size: 11)).underline()
                                }
                                .buttonStyle(.plain)
                                .foregroundStyle(VeilTheme.muted)
                                .help("在访达中显示空间目录")
                                .accessibilityLabel("在访达中打开空间目录：\(space.path)")
                            }
                        }
                        Spacer()
                    }.padding(15).frame(maxWidth: .infinity, alignment: .leading)
                        .background(VeilTheme.card, in: RoundedRectangle(cornerRadius: 12))
                        .overlay(RoundedRectangle(cornerRadius: 12).stroke(VeilTheme.border))
                }
                WelcomeCard()
                VStack(alignment: .leading, spacing: 18) {
                    HStack {
                        VStack(alignment: .leading, spacing: 6) {
                            Text("\(store.activeSpace?.name ?? "本机")的容器").font(.system(size: 15, weight: .semibold))
                            Text(store.unlockCacheSeconds > 0
                                 ? "选择容器打开，\(store.unlockCacheDescription)内重开无需密码"
                                 : "选择容器并输入密码打开").font(.system(size: 11)).foregroundStyle(VeilTheme.muted)
                        }
                        Spacer()
                        Button { Task { await store.choosePackage() } } label: {
                            HStack(spacing: 6) {
                                Text("导入容器")
                                Image(systemName: "arrow.right")
                            }.font(.system(size: 11, weight: .semibold)).foregroundStyle(VeilTheme.secondary)
                                .padding(.horizontal, 8).frame(minHeight: 32).contentShape(Rectangle())
                        }.buttonStyle(VeilPlainButtonStyle()).disabled(store.isBusy || store.activeSpace?.isAvailable != true)
                    }
                    // 离线时不把空容器摘要误显示为“没有容器”；上方保留不可用说明和目录路径。
                    if let space = store.activeSpace, !space.isAvailable {
                        EmptyView()
                    } else if let space = store.activeSpace, !space.containers.isEmpty {
                        LazyVGrid(columns: columns, spacing: 12) {
                            // 每轮一个当前空间已登记容器；点击把该摘要交给 Store 决定复用密码还是显示解锁表单。
                            ForEach(space.containers) { item in
                                ContainerCard(name: item.name, subtitle: "本机 / \(space.name)") { Task { await store.chooseContainer(item) } }.help(item.path)
                            }
                        }
                    } else {
                        HStack(spacing: 13) {
                            Image(systemName: "folder").font(.system(size: 22, weight: .light)).foregroundStyle(VeilTheme.muted)
                                .frame(width: 40, height: 40).background(VeilTheme.thumbnail, in: RoundedRectangle(cornerRadius: 10))
                            VStack(alignment: .leading, spacing: 5) {
                                Text("这个空间还没有容器").font(.system(size: 12, weight: .semibold))
                                Text("创建容器后，它会显示在这里。").font(.system(size: 11)).foregroundStyle(VeilTheme.muted)
                            }
                            Spacer()
                        }.padding(20).frame(maxWidth: .infinity).background(VeilTheme.card, in: RoundedRectangle(cornerRadius: 12))
                            .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(VeilTheme.border, style: StrokeStyle(lineWidth: 1, dash: [4])))
                    }
                }.padding(.top, 9)
                if !store.externalRecents.isEmpty {
                    VStack(alignment: .leading, spacing: 18) {
                        Text("最近打开").font(.system(size: 15, weight: .semibold))
                        LazyVGrid(columns: columns, spacing: 12) {
                            // 每轮一条外部容器最近记录；这里只组装摘要，不预先验证磁盘位置仍然可用。
                            ForEach(store.externalRecents) { recent in
                                ContainerCard(name: recent.name, subtitle: recent.path) {
                                    Task { await store.chooseContainer(ContainerSummary(id: recent.id, name: recent.name, path: recent.path)) }
                                }
                            }
                        }
                    }.padding(.top, 9)
                }
            }.padding(.horizontal, VeilDesign.layout("content-padding-x")).padding(.top, VeilDesign.layout("content-padding-y")).padding(.bottom, 48)
                .frame(maxWidth: VeilDesign.layout("content-max"), alignment: .leading).frame(maxWidth: .infinity)
        }
    }
}

/// 首页可点击的容器卡片，展示容器名称与所属空间或路径。
private struct ContainerCard: View {
    /// 用于读取忙碌状态并禁用点击；实际目标由调用方 action 捕获。
    @EnvironmentObject private var store: AppStore
    /// 卡片所代表的容器名称。
    let name: String
    /// 用于区分容器位置的空间名或路径说明。
    let subtitle: String
    /// 用户点击卡片后执行的容器选择动作。
    let action: () -> Void
    /// 鼠标悬停的界面状态，只影响背景或边框，不表示容器已选中或解锁。
    @State private var hovering = false

    /// 展示一个容器摘要并把卡片点击委托给调用方。
    var body: some View {
        Button(action: action) {
            HStack(spacing: 12) {
                Image(systemName: "checkmark.shield").font(.system(size: 20, weight: .light)).foregroundStyle(VeilTheme.secondary)
                    .frame(width: 40, height: 40).background(VeilTheme.thumbnail, in: RoundedRectangle(cornerRadius: 10))
                VStack(alignment: .leading, spacing: 6) {
                    Text(name).font(.system(size: 12, weight: .semibold)).foregroundStyle(VeilTheme.text)
                    Text(subtitle).font(.system(size: 10)).foregroundStyle(VeilTheme.muted).lineLimit(1).truncationMode(.middle)
                }
                Spacer(minLength: 0)
                Image(systemName: "arrow.right").font(.system(size: 12)).foregroundStyle(VeilTheme.muted)
            }.padding(16).frame(maxWidth: .infinity, alignment: .leading)
                .background(VeilTheme.card, in: RoundedRectangle(cornerRadius: 12))
                .overlay(RoundedRectangle(cornerRadius: 12).stroke(hovering ? VeilTheme.secondary.opacity(0.45) : VeilTheme.border))
        }.buttonStyle(VeilPlainButtonStyle()).onHover { hovering = $0 }.disabled(store.isBusy).help(subtitle)
    }
}

/// 通用反馈横幅，只展示调用方传入的文案及错误样式，不保存消息生命周期。
/// 可选 Action 提供后续处理，dismiss 委托调用方清除状态；组件不自行判断成功与否或自动关闭。
struct FeedbackBanner: View {
    /// 横幅右侧可选动作，例如「导出错误日志」，便于出错时就地导出诊断信息。
    struct Action {
        /// 按钮上显示的操作名称。
        let title: String
        /// 用户点击后执行的业务动作。
        let run: () -> Void
    }

    /// 要向用户展示的业务结果或错误说明。
    let message: String
    /// true 表示错误反馈，false 表示普通提示或成功结果。
    let isError: Bool
    /// 可选的后续处理动作；为空时只显示消息和关闭按钮。
    var action: Action? = nil
    /// 关闭横幅并由调用方清理对应状态的动作。
    let dismiss: () -> Void

    /// 显示一条成功或错误反馈，并提供可选后续动作和关闭入口。
    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: isError ? "exclamationmark.circle" : "checkmark.circle")
            Text(message).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
            if let action {
                Button(action.title, action: action.run).buttonStyle(VeilButtonStyle(variant: .subtle))
            }
            Button(action: dismiss) { Image(systemName: "xmark") }.buttonStyle(VeilPlainButtonStyle()).accessibilityLabel("关闭提示")
        }.font(.system(size: 12)).foregroundStyle(isError ? VeilTheme.danger : VeilTheme.secondary)
            .padding(12).background(isError ? VeilTheme.dangerSurface : VeilTheme.thumbnail)
    }
}

/// 解密期间的等待浮层，展示文件名、打开目标及临时副本用途。
/// 只读 status，不执行解密、打开或清理；提示中的清理由 Store 尽力完成，不代表删除必然成功。
private struct DecryptOverlay: View {
    /// 当前解密任务的文件名和打开目标等状态。
    let status: DecryptStatus

    /// 遮罩界面并说明当前明文临时副本的生成、打开目标和清理时机。
    var body: some View {
        ZStack {
            Color.black.opacity(0.18).ignoresSafeArea()
            VStack(spacing: 12) {
                ProgressView().controlSize(.large)
                Text("正在解密文件…").font(.system(size: 14, weight: .semibold))
                Text(status.fileName).font(.system(size: 12)).foregroundStyle(VeilTheme.secondary)
                    .lineLimit(1).truncationMode(.middle)
                Text(hint).font(.system(size: 11)).foregroundStyle(VeilTheme.muted)
                    .multilineTextAlignment(.center).frame(maxWidth: 280).fixedSize(horizontal: false, vertical: true)
            }
            .padding(24).frame(width: 330)
            .background(VeilTheme.card, in: RoundedRectangle(cornerRadius: 14))
            .overlay(RoundedRectangle(cornerRadius: 14).stroke(VeilTheme.border))
            .shadow(color: .black.opacity(0.18), radius: 24, y: 10)
        }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.updatesFrequently)
    }

    /// 根据打开目标生成临时文件用途和清理时机说明。
    private var hint: String {
        "会先把文件解密到临时文件，再用\(status.destination)打开；锁定或退出后自动清理。"
    }
}
