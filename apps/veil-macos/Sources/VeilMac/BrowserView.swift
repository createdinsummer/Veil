import AppKit
import SwiftUI

struct BrowserView: View {
    @EnvironmentObject private var store: AppStore
    let container: ContainerSnapshot
    private let columns = [GridItem(.adaptive(minimum: 120, maximum: 180), spacing: 12, alignment: .top)]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            toolbar
            Rectangle().fill(VeilTheme.border).frame(height: 1)
            if store.browseMode == .type {
                ScrollView(.horizontal) {
                    HStack(spacing: 7) {
                        FilterButton(title: "全部", selected: store.categories.isEmpty) { store.categories = [] }
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

            Group {
                if store.visibleEntries.isEmpty {
                    VStack(spacing: 10) {
                        Image(systemName: "folder").font(.system(size: 28, weight: .light)).foregroundStyle(VeilTheme.muted)
                            .frame(width: 56, height: 56).background(VeilTheme.thumbnail, in: RoundedRectangle(cornerRadius: 17)).padding(.bottom, 6)
                        Text(container.files.isEmpty ? "这里还是空的" : "这里没有文件").font(.system(size: 13, weight: .semibold))
                        Text(container.files.isEmpty ? "添加文件或将文件拖到这里。" : "试试选择其他分类、目录或搜索词。")
                            .font(.system(size: 11)).foregroundStyle(VeilTheme.muted)
                        Button("添加文件…") { Task { await store.chooseImport(directory: false) } }
                            .buttonStyle(VeilButtonStyle()).padding(.top, 8).disabled(store.isBusy)
                    }.frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if store.layout == .grid {
                    ScrollView {
                        LazyVGrid(columns: columns, alignment: .leading, spacing: 15) {
                            ForEach(store.visibleEntries) { entry in FileTile(entry: entry) }
                        }.padding(.bottom, 20)
                    }
                } else {
                    FileList(entries: store.visibleEntries)
                }
            }.padding(.horizontal, 24)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(VeilTheme.card)
        .onChange(of: store.layout) { _, _ in Task { await store.savePreferences() } }
        .onChange(of: store.browseMode) { _, _ in Task { await store.savePreferences() } }
        .task(id: store.thumbnailKey) { await store.loadThumbnails() }
        .dropDestination(for: URL.self) { urls, _ in
            guard !store.isBusy, !urls.isEmpty else { return false }
            Task { await store.importFiles(urls) }
            return true
        }
    }

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
            }.padding(2).background(VeilTheme.thumbnail, in: RoundedRectangle(cornerRadius: 7)).fixedSize()
            Menu {
                Button("添加文件…") { Task { await store.chooseImport(directory: false) } }
                Button("添加文件夹…") { Task { await store.chooseImport(directory: true) } }
            } label: { Label("添加", systemImage: "plus").font(.system(size: 12)).foregroundStyle(VeilTheme.text) }
                .menuStyle(.borderlessButton).fixedSize().disabled(store.isBusy).pointerCursor()
        }.padding(.horizontal, 24).frame(height: 48)
    }
}

private struct BrowseTab: View {
    let title: String
    let selected: Bool
    let action: () -> Void
    var body: some View {
        Button(action: action) {
            Text(title).font(.system(size: 13, weight: selected ? .semibold : .regular))
                .foregroundStyle(selected ? VeilTheme.text : VeilTheme.secondary).padding(.vertical, 15)
                .overlay(alignment: .bottom) { Rectangle().fill(selected ? VeilTheme.accent : .clear).frame(height: 2) }
        }.buttonStyle(VeilPlainButtonStyle()).accessibilityAddTraits(selected ? .isSelected : [])
    }
}

private struct FilterButton: View {
    let title: String
    let selected: Bool
    let action: () -> Void
    var body: some View {
        Button(action: action) {
            Text(title).font(.system(size: 12)).padding(.horizontal, 11).padding(.vertical, 7)
                .foregroundStyle(selected ? VeilTheme.text : VeilTheme.secondary)
                .background(selected ? VeilTheme.sidebar : VeilTheme.card, in: RoundedRectangle(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(selected ? VeilTheme.secondary.opacity(0.65) : VeilTheme.border))
        }.buttonStyle(VeilPlainButtonStyle()).accessibilityAddTraits(selected ? .isSelected : [])
    }
}

private struct LayoutButton: View {
    let title: String
    let symbol: String
    let selected: Bool
    let action: () -> Void
    var body: some View {
        Button(action: action) {
            Label(title, systemImage: symbol).labelStyle(.iconOnly).font(.system(size: 12)).frame(width: 28, height: 26)
                .foregroundStyle(selected ? VeilTheme.text : VeilTheme.muted)
                .background(selected ? VeilTheme.card : .clear, in: RoundedRectangle(cornerRadius: 6))
                .shadow(color: .black.opacity(selected ? 0.05 : 0), radius: 2, y: 1)
        }.buttonStyle(VeilPlainButtonStyle()).help(title).accessibilityLabel(title).accessibilityAddTraits(selected ? .isSelected : [])
    }
}

private struct FileTile: View {
    @EnvironmentObject private var store: AppStore
    let entry: BrowserEntry
    @State private var hovering = false
    var body: some View {
        Button { Task { await store.activate(entry) } } label: {
            VStack(alignment: .leading, spacing: 8) {
                ZStack {
                    VeilTheme.thumbnail
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

private struct FileList: View {
    @EnvironmentObject private var store: AppStore
    let entries: [BrowserEntry]
    var body: some View {
        GeometryReader { geometry in
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

private struct FileListRow: View {
    @EnvironmentObject private var store: AppStore
    let entry: BrowserEntry
    let showDate: Bool
    @State private var hovering = false
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

private struct EntryActions: View {
    @EnvironmentObject private var store: AppStore
    let entry: BrowserEntry
    var body: some View {
        Button("打开", systemImage: "arrow.up.forward.square") { Task { await store.activate(entry) } }
        if case .file(let file) = entry {
            Button("导出…", systemImage: "square.and.arrow.up") { Task { await store.exportFile(file) } }
        }
        Divider()
        Button("删除…", systemImage: "trash", role: .destructive) { store.pendingDelete = entry }
    }
}
