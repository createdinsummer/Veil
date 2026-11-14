import AppKit
import SwiftUI

struct ContentView: View {
    @EnvironmentObject private var store: AppStore
    @Environment(\.openWindow) private var openWindow
    @State private var sidebarVisible = true

    var body: some View {
        content
            .task(id: store.notice) {
                guard let notice = store.notice else { return }
                do { try await Task.sleep(for: .seconds(3)) } catch { return }
                if store.notice == notice { store.notice = nil }
            }
            .task {
                while !Task.isCancelled {
                    do { try await Task.sleep(for: .seconds(15)) } catch { return }
                    await store.refreshSpaceAvailability()
                }
            }
            .sheet(item: $store.importInspection) { inspection in
                ImportConflictReview(inspection: inspection, operationLabel: store.conflictOperation, progressText: store.importProgressDescription) { store.completeImportReview($0, id: inspection.id) }
                    .interactiveDismissDisabled()
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
    }

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

    /// 主窗口内的日志导出入口：与任务中心共用同一保存流程，出错时可就地导出反馈。
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

/// 删除确认弹窗独立为视图修饰符，避免主视图表达式超出类型检查预算。
private extension View {
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

struct TransferProgressView: View {
    @EnvironmentObject private var store: AppStore
    let title: String
    let progress: TransferProgress?

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

private struct SidebarView: View {
    @EnvironmentObject private var store: AppStore
    @Environment(\.openWindow) private var openWindow
    @State private var collapsed: Set<String> = []

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
                Label("本机", systemImage: "lock").font(.system(size: 11)).foregroundStyle(VeilTheme.muted)
            }.padding(.horizontal, 23).padding(.bottom, 20).padding(.top, 12)
        }.frame(maxWidth: .infinity, maxHeight: .infinity).background(VeilTheme.sidebar)
    }
}

private struct SidebarContainerRow: View {
    @EnvironmentObject private var store: AppStore
    let item: ContainerSummary
    let selected: Bool
    @State private var hovering = false
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

private struct HomeView: View {
    @EnvironmentObject private var store: AppStore
    private let columns = [GridItem(.flexible()), GridItem(.flexible())]

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
                    if let space = store.activeSpace, !space.isAvailable {
                        EmptyView()
                    } else if let space = store.activeSpace, !space.containers.isEmpty {
                        LazyVGrid(columns: columns, spacing: 12) {
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

private struct ContainerCard: View {
    @EnvironmentObject private var store: AppStore
    let name: String
    let subtitle: String
    let action: () -> Void
    @State private var hovering = false

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

struct FeedbackBanner: View {
    /// 横幅右侧可选动作，例如「导出错误日志」，便于出错时就地导出诊断信息。
    struct Action {
        let title: String
        let run: () -> Void
    }

    let message: String
    let isError: Bool
    var action: Action? = nil
    let dismiss: () -> Void

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
