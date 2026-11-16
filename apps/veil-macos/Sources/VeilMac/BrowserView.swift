/// macOS 容器文件浏览界面与复用的列表 / 网格条目组件。
/// 所属模块：桌面端容器浏览；支持按目录或文件类型浏览、搜索、缩略图、批量导入、导出、删除和新建文件夹。
/// 核心数据：container 是当前容器快照；AppStore 提供可见条目、筛选、搜索、缩略图和操作状态；BrowserEntry 统一表示文件或目录。
/// 核心组件：BrowserView 组合工具栏和结果区；FileTile / FileListRow 显示单项；EntryActions 提供上下文菜单。
/// 依赖 AppKit 文件位置打开、SwiftUI 惰性列表与 AppStore 服务。整体目标是让界面筛选和展示状态统一由 Store 管理，操作始终作用于对应容器节点。
import AppKit
import SwiftUI

/// 已解锁容器的浏览入口；本地只保存新建目录弹窗输入，筛选、导航、缩略图和操作交给共享 Store。
/// container 用于空状态说明，实际条目来自 Store.visibleEntries；两者不是在本视图中重新合并的数据源。
struct BrowserView: View {
    /// 提供目录树、筛选结果、预览缓存和文件操作的应用状态。
    @EnvironmentObject private var store: AppStore
    /// 控制新建文件夹名称弹窗是否显示。
    @State private var showNewFolder = false
    /// 用户输入的待创建目录名；提交前由 folderNameForCreation 校验。
    @State private var newFolderName = ""
    /// 当前已解锁容器的文件和目录元数据快照。
    let container: ContainerSnapshot
    /// 网格列配置集合，当前只有一项自适应列规则；SwiftUI 据此按可用宽度排出宽 120 至 180 点的卡片。
    private let columns = [GridItem(.adaptive(minimum: 120, maximum: 180), spacing: 12, alignment: .top)]

    /// 按浏览模式、筛选和布局状态组装工具栏、面包屑、空状态及条目列表。
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            toolbar
            Rectangle().fill(VeilTheme.border).frame(height: 1)
            // 仅按类型浏览时显示分类筛选；按路径时分类筛选不影响当前目录导航。
            if store.browseMode == .type {
                ScrollView(.horizontal) {
                    HStack(spacing: 7) {
                        FilterButton(title: "全部", selected: store.categories.isEmpty) { store.categories = [] }
                        // 每轮一类文件类型；点击切换 Set 成员，空 Set 表示全部类别，而不是无匹配项。
                        ForEach(FileCategory.allCases) { category in
                            FilterButton(title: category.title, selected: store.categories.contains(category)) {
                                if store.categories.contains(category) { store.categories.remove(category) }
                                else { store.categories.insert(category) }
                            }
                        }
                    }.padding(.horizontal, 24).padding(.top, 10)
                }.scrollIndicators(.hidden).frame(height: 44)
            }
            HStack(spacing: 7) {
                if store.browseMode == .path {
                    // 每轮一个从根目录到当前目录的祖先节点；点击只更新当前浏览路径，不访问磁盘目录。
                    ForEach(directoryBreadcrumbs(store.directory)) { crumb in
                        if !crumb.path.isEmpty { Image(systemName: "chevron.right").font(.system(size: 8)).foregroundStyle(VeilTheme.muted) }
                        Button { store.directory = crumb.path } label: { Text(crumb.title).padding(.horizontal, 3).frame(minHeight: 30).contentShape(Rectangle()) }
                            .buttonStyle(VeilPlainButtonStyle())
                    }
                } else {
                    Text("文件分类 / \(store.categories.isEmpty ? "全部" : FileCategory.allCases.filter { store.categories.contains($0) }.map(\.title).joined(separator: "、"))")
                }
                Spacer(minLength: 8)
                Text("\(store.visibleEntries.count) 项").foregroundStyle(VeilTheme.muted)
            }
            .font(.system(size: 11)).foregroundStyle(VeilTheme.secondary).lineLimit(1)
            .padding(.horizontal, 24).padding(.vertical, 12)

            // 根据已筛选条目选择空状态、网格或列表；三种模式共用 Store 的 visibleEntries。
            Group {
                if store.visibleEntries.isEmpty {
                    VStack(spacing: 10) {
                        Image(systemName: "folder").font(.system(size: 28, weight: .light)).foregroundStyle(VeilTheme.muted)
                            .frame(width: 56, height: 56).background(VeilTheme.thumbnail, in: RoundedRectangle(cornerRadius: 17)).padding(.bottom, 6)
                        Text(container.files.isEmpty ? "这里还是空的" : "这里没有文件").font(.system(size: 13, weight: .semibold))
                        Text(container.files.isEmpty ? "添加文件或将文件拖到这里。" : store.showHiddenFiles ? "试试选择其他分类、目录或搜索词。" : "试试选择其他分类、目录或搜索词，或显示以.开头的文件。")
                            .font(.system(size: 11)).foregroundStyle(VeilTheme.muted)
                        Button("添加文件…") { Task { await store.chooseImport(directory: false) } }
                            .buttonStyle(VeilButtonStyle()).padding(.top, 8).disabled(store.isBusy)
                        if store.browseMode == .path {
                            Button("新建文件夹…", systemImage: "folder.badge.plus") {
                                newFolderName = ""
                                showNewFolder = true
                            }
                            .buttonStyle(VeilButtonStyle(variant: .subtle))
                            .disabled(store.isBusy)
                        }
                    }.frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if store.layout == .grid {
                    ScrollView {
                        LazyVGrid(columns: columns, alignment: .leading, spacing: 15) {
                            // 每条可见业务记录渲染成单独卡片，操作通过 entry 传回 Store。
                            ForEach(store.visibleEntries) { entry in FileTile(entry: entry) }
                        }.padding(.bottom, 20)
                    }
                } else {
                    FileList(entries: store.visibleEntries)
                }
            }.padding(.horizontal, 24)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            if let space = store.activeSpace {
                Rectangle().fill(VeilTheme.border).frame(height: 1)
                HStack(spacing: 7) {
                    Label("空间磁盘路径", systemImage: "folder").foregroundStyle(VeilTheme.muted)
                    Spacer(minLength: 8)
                    Button {
                        NSWorkspace.shared.activateFileViewerSelecting([
                            URL(fileURLWithPath: space.path, isDirectory: true)
                        ])
                    } label: {
                        Text(space.path).underline().lineLimit(1).truncationMode(.middle)
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(VeilTheme.secondary)
                    .help("在访达中显示空间目录")
                    .accessibilityLabel("在访达中打开空间目录：\(space.path)")
                }
                .font(.system(size: 10))
                .padding(.horizontal, 24)
                .padding(.vertical, 10)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(VeilTheme.card)
        // 内存偏好改变后异步请求保存；失败由 Store 报告，设置窗口也订阅这些值时可能同时触发保存。
        .onChange(of: store.layout) { _, _ in Task { await store.savePreferences() } }
        .onChange(of: store.browseMode) { _, _ in Task { await store.savePreferences() } }
        // 筛选或容器相关 key 变化时替换缩略图任务；SwiftUI 取消旧任务不等于已启动的服务工作立即终止。
        .task(id: store.thumbnailKey) { await store.loadThumbnails() }
        // urls 集合每项是一个拖入来源 URL，可包含文件或目录；返回 true 只表示接受并启动异步导入，不代表成功。
        // 忙碌或空集合直接拒绝；目标目录由后续 Store 调用读取，不能把拖入时状态视为不可变快照。
        .dropDestination(for: URL.self) { urls, _ in
            guard !store.isBusy, !urls.isEmpty else { return false }
            Task { await store.importFiles(urls) }
            return true
        }
        .alert("新建文件夹", isPresented: $showNewFolder) {
            TextField("文件夹名称", text: $newFolderName)
            // 名称校验通过后才允许点击；读取输入副本并立即清空弹窗状态。
            Button("创建") {
                // 只捕获本次名称；当前目标目录由 Store 创建任务时读取，异步调度期间仍可能导航到其他目录。
                let name = newFolderName
                newFolderName = ""
                Task { await store.createFolder(named: name) }
            }
            .disabled(folderNameForCreation(newFolderName) == nil)
            Button("取消", role: .cancel) { newFolderName = "" }
        } message: {
            Text("将在「\(store.directory.isEmpty ? "全部文件" : store.directory)」下创建文件夹。")
        }
    }

    /// 生成路径 / 类型切换、搜索、布局、隐藏文件和添加菜单。
    private var toolbar: some View {
        HStack(spacing: 12) {
            HStack(spacing: 18) {
                BrowseTab(title: "按路径", selected: store.browseMode == .path) { store.browseMode = .path }
                BrowseTab(title: "按类型", selected: store.browseMode == .type) { store.browseMode = .type }
            }.fixedSize()
            Spacer(minLength: 0)
            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass").foregroundStyle(VeilTheme.muted)
                TextField("搜索文件", text: $store.search).textFieldStyle(.plain)
            }.font(.system(size: 12)).padding(.horizontal, 10)
                .frame(minWidth: 100, idealWidth: 160, maxWidth: 200).frame(height: 28)
                .background(VeilTheme.thumbnail, in: RoundedRectangle(cornerRadius: 7))
                .overlay(RoundedRectangle(cornerRadius: 7).stroke(VeilTheme.border))
            HStack(spacing: 2) {
                LayoutButton(title: "列表", symbol: "list.bullet", selected: store.layout == .list) { store.layout = .list }
                LayoutButton(title: "大图标", symbol: "square.grid.2x2", selected: store.layout == .grid) { store.layout = .grid }
                Divider().frame(height: 14).padding(.horizontal, 2)
                LayoutButton(
                    title: store.showHiddenFiles ? "隐藏以.开头的文件" : "显示以.开头的文件",
                    symbol: store.showHiddenFiles ? "eye" : "eye.slash",
                    selected: store.showHiddenFiles
                ) { store.showHiddenFiles.toggle() }
            }.padding(2).background(VeilTheme.thumbnail, in: RoundedRectangle(cornerRadius: 7)).fixedSize()
            Menu {
                if store.browseMode == .path {
                    Button("新建文件夹…", systemImage: "folder.badge.plus") {
                        newFolderName = ""
                        showNewFolder = true
                    }
                    Divider()
                }
                Button("添加文件…") { Task { await store.chooseImport(directory: false) } }
                Button("添加文件夹…") { Task { await store.chooseImport(directory: true) } }
            } label: { Label("添加", systemImage: "plus").font(.system(size: 12)).foregroundStyle(VeilTheme.text) }
                .menuStyle(.borderlessButton).fixedSize().disabled(store.isBusy).pointerCursor()
        }.padding(.horizontal, 24).frame(height: 48)
    }
}

/// 文件浏览器顶部的路径 / 类型模式切换按钮。
private struct BrowseTab: View {
    /// 切换按钮显示的用户可读名称。
    let title: String
    /// 当前路径 / 类型模式是否选中此按钮。
    let selected: Bool
    /// 用户点击后通知父浏览器切换模式的动作。
    let action: () -> Void
    /// 根据选中状态绘制路径 / 类型切换按钮。
    var body: some View {
        Button(action: action) {
            Text(title).font(.system(size: 13, weight: selected ? .semibold : .regular))
                .foregroundStyle(selected ? VeilTheme.text : VeilTheme.secondary).padding(.vertical, 15)
                .overlay(alignment: .bottom) { Rectangle().fill(selected ? VeilTheme.accent : .clear).frame(height: 2) }
        }.buttonStyle(VeilPlainButtonStyle()).accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// 文件分类筛选按钮；selected 表示当前分类是否已纳入筛选。
private struct FilterButton: View {
    /// 文件类别的显示名称。
    let title: String
    /// 该类别当前是否纳入筛选结果。
    let selected: Bool
    /// 用户点击后切换类别筛选状态的动作。
    let action: () -> Void
    /// 根据分类选择状态绘制可切换的筛选项。
    var body: some View {
        Button(action: action) {
            Text(title).font(.system(size: 12)).padding(.horizontal, 11).padding(.vertical, 7)
                .foregroundStyle(selected ? VeilTheme.text : VeilTheme.secondary)
                .background(selected ? VeilTheme.sidebar : VeilTheme.card, in: RoundedRectangle(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(selected ? VeilTheme.secondary.opacity(0.65) : VeilTheme.border))
        }.buttonStyle(VeilPlainButtonStyle()).accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// 网格、列表或隐藏文件开关的紧凑按钮。
private struct LayoutButton: View {
    /// 按钮的可访问名称和悬停提示。
    let title: String
    /// 表示布局或可见性状态的 SF Symbol 名称。
    let symbol: String
    /// 按钮对应的布局 / 显示开关当前是否开启。
    let selected: Bool
    /// 用户点击后由父浏览器更新布局或隐藏文件状态。
    let action: () -> Void
    /// 根据当前展示模式绘制列表 / 网格控制按钮。
    var body: some View {
        Button(action: action) {
            Label(title, systemImage: symbol).labelStyle(.iconOnly).font(.system(size: 12)).frame(width: 28, height: 26)
                .foregroundStyle(selected ? VeilTheme.text : VeilTheme.muted)
                .background(selected ? VeilTheme.card : .clear, in: RoundedRectangle(cornerRadius: 6))
                .shadow(color: .black.opacity(selected ? 0.05 : 0), radius: 2, y: 1)
        }.buttonStyle(VeilPlainButtonStyle()).help(title).accessibilityLabel(title).accessibilityAddTraits(selected ? .isSelected : [])
    }
}

/// 网格模式中的文件或文件夹卡片，显示缩略图并提供拖拽导出。
private struct FileTile: View {
    /// 处理条目打开、预览、导出和删除的共享状态。
    @EnvironmentObject private var store: AppStore
    /// 当前卡片代表的文件或目录条目。
    let entry: BrowserEntry
    /// 鼠标是否悬停在卡片上，用于切换背景。
    @State private var hovering = false
    /// 展示一个网格文件 / 目录条目，打开和导出动作都针对该 entry。
    var body: some View {
        Button { Task { await store.activate(entry) } } label: {
            VStack(alignment: .leading, spacing: 8) {
                ZStack {
                    VeilTheme.thumbnail
                    // 仅文件可命中按文件 ID 保存的图片字节缓存；缺失或无法解码时使用类型图标，目录始终走图标分支。
                    if case .file(let file) = entry, let data = store.thumbnails[file.id], let image = NSImage(data: data) {
                        GeometryReader { geometry in
                            Image(nsImage: image).resizable().scaledToFill().frame(width: geometry.size.width, height: VeilDesign.layout("file-preview-height")).clipped()
                        }
                    } else {
                        Image(systemName: entry.symbol).font(.system(size: 36, weight: .light)).foregroundStyle(VeilTheme.secondary)
                    }
                }.frame(height: VeilDesign.layout("file-preview-height")).clipShape(RoundedRectangle(cornerRadius: 10))
                HStack {
                    Text(entry.name).font(.system(size: 12, weight: .semibold)).foregroundStyle(VeilTheme.text).lineLimit(1).truncationMode(.middle)
                    if case .file(let file) = entry { DragExportHandle(file: file, store: store) }
                }
                Group {
                    switch entry {
                    case .folder: Text("文件夹")
                    case .file(let file): Text(store.browseMode == .type ? file.parentName : file.sizeLabel)
                    }
                }.font(.system(size: 11)).foregroundStyle(VeilTheme.muted).lineLimit(1)
            }.padding(5).contentShape(Rectangle())
                .background(hovering ? VeilTheme.canvas : .clear, in: RoundedRectangle(cornerRadius: 9))
        }.buttonStyle(VeilPlainButtonStyle()).onHover { hovering = $0 }.help(entry.path)
            .disabled(!store.canBrowse).contextMenu { EntryActions(entry: entry).disabled(store.isBusy) }
    }
}

/// 列表模式容器；根据视图宽度决定是否显示加密日期列。
private struct FileList: View {
    /// 当前容器浏览状态；列表显示的 entries 已由该状态筛选。
    @EnvironmentObject private var store: AppStore
    /// 当前筛选后要展示的文件 / 文件夹对象集合；逐项渲染成 FileListRow。
    let entries: [BrowserEntry]
    /// 按列展示当前筛选结果；窄窗口隐藏日期列以保留名称与操作空间。
    var body: some View {
        GeometryReader { geometry in
            // 窄窗口优先保留名称和操作列，窗口足够宽时再增加加密时间。
            let showDate = geometry.size.width > 620
            VStack(spacing: 0) {
                HStack(spacing: 8) {
                    Text("名称").frame(maxWidth: .infinity, alignment: .leading)
                    Text("大小").frame(width: 70, alignment: .leading)
                    Text("版本").frame(width: 40, alignment: .leading)
                    if showDate { Text("加密时间").frame(width: 100, alignment: .leading) }
                    Text("操作").frame(width: 84)
                }.font(.system(size: 11)).foregroundStyle(VeilTheme.muted).padding(.horizontal, 8).padding(.vertical, 10)
                Rectangle().fill(VeilTheme.border).frame(height: 1)
                ScrollView {
                    LazyVStack(spacing: 0) {
                        // 每轮渲染一条可见条目，并用同一 showDate 规则保持列对齐。
                        ForEach(entries) { entry in
                            FileListRow(entry: entry, showDate: showDate)
                            Rectangle().fill(VeilTheme.border.opacity(0.5)).frame(height: 1)
                        }
                    }
                }
            }
        }
    }
}

/// 列表中的一行文件或目录；提供打开、导出、删除和上下文操作。
private struct FileListRow: View {
    /// 处理列表行操作并读取当前浏览模式的共享状态。
    @EnvironmentObject private var store: AppStore
    /// 当前行对应的文件或文件夹条目。
    let entry: BrowserEntry
    /// 当前窗口是否有空间展示日期列。
    let showDate: Bool
    /// 鼠标悬停状态，用于强调当前行。
    @State private var hovering = false
    /// 展示一个文件或目录的列表记录，并把行内动作绑定到该条目。
    var body: some View {
        HStack(spacing: 8) {
            Button { Task { await store.activate(entry) } } label: {
                HStack(spacing: 9) {
                    Image(systemName: entry.symbol).font(.system(size: 17, weight: .light)).foregroundStyle(VeilTheme.secondary).frame(width: 21)
                    VStack(alignment: .leading, spacing: 3) {
                        Text(entry.name).font(.system(size: 12)).lineLimit(1).truncationMode(.middle)
                        if case .file(let file) = entry, store.browseMode == .type {
                            Text(file.parentName).font(.system(size: 10)).foregroundStyle(VeilTheme.muted).lineLimit(1)
                        }
                    }
                    Spacer(minLength: 0)
                }.frame(minHeight: VeilDesign.layout("row-height")).contentShape(Rectangle())
            }.buttonStyle(VeilPlainButtonStyle()).frame(maxWidth: .infinity, alignment: .leading)
            Group {
                if case .file(let file) = entry {
                    Text(file.sizeLabel).frame(width: 70, alignment: .leading)
                    Text("v\(file.version)").frame(width: 40, alignment: .leading)
                    if showDate { Text(file.dateLabel).frame(width: 100, alignment: .leading) }
                } else {
                    Text("—").frame(width: 70, alignment: .leading)
                    Text("—").frame(width: 40, alignment: .leading)
                    if showDate { Text("—").frame(width: 100, alignment: .leading) }
                }
            }.font(.system(size: 11)).foregroundStyle(VeilTheme.secondary)
            HStack(spacing: 0) {
                if case .file(let file) = entry {
                    DragExportHandle(file: file, store: store)
                    Button { Task { await store.exportFile(file) } } label: { Image(systemName: "square.and.arrow.up") }
                        .buttonStyle(VeilIconButtonStyle()).help("导出 \(file.leafName)")
                } else { Color.clear.frame(width: 52, height: 32) }
                Button { store.pendingDelete = entry } label: { Image(systemName: "trash") }
                    .buttonStyle(VeilIconButtonStyle()).help("删除 \(entry.name)")
            }.frame(width: 84).disabled(store.isBusy)
        }
        .padding(.horizontal, 8).frame(minHeight: VeilDesign.layout("row-height"))
        .background(hovering ? VeilTheme.canvas : .clear, in: RoundedRectangle(cornerRadius: 6))
        .onHover { hovering = $0 }.disabled(!store.canBrowse).contextMenu { EntryActions(entry: entry).disabled(store.isBusy) }
    }
}

/// 绑定单个文件或目录的右键菜单，不保存独立任务状态。
/// 文件提供预览及外部打开，目录仅支持打开导航和整目录导出；删除只登记待确认条目，由外层对话框执行。
private struct EntryActions: View {
    /// 将菜单操作提交给当前容器状态。
    @EnvironmentObject private var store: AppStore
    /// 此菜单要操作的容器文件或文件夹。
    let entry: BrowserEntry
    /// 依据条目是文件还是目录提供相应的打开、预览、导出或删除动作。
    var body: some View {
        Button("打开", systemImage: "arrow.up.forward.square") { Task { await store.activate(entry) } }
        if case .file(let file) = entry {
            Button("在应用内预览", systemImage: "eye") { Task { await store.showPreview(file) } }
            Button("用系统应用打开", systemImage: "arrow.up.forward.app") { Task { await store.openExternally(file) } }
            Button("选择应用打开…", systemImage: "ellipsis.circle") { Task { await store.chooseApplication(for: file) } }
            Button("导出…", systemImage: "square.and.arrow.up") { Task { await store.exportFile(file) } }
        } else {
            Button("导出…", systemImage: "square.and.arrow.up") { Task { await store.exportFolder(entry.path) } }
        }
        Divider()
        Button("删除…", systemImage: "trash", role: .destructive) { store.pendingDelete = entry }
    }
}
