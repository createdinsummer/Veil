import SwiftUI
import PhotosUI
import AVKit
import UniformTypeIdentifiers

@main struct VeilApp: App {
    @StateObject private var store = Store()
    @Environment(\.scenePhase) private var phase
    var body: some Scene {
        WindowGroup { RootView().environmentObject(store).task { await store.load() } }
            .onChange(of: phase) { _, value in if value == .background { Task { await store.lock() } } }
    }
}

private let ink = Color.primary
private let surface = Color(uiColor: .secondarySystemGroupedBackground)
private let mutedSurface = Color(uiColor: .tertiarySystemFill)
private let outline = Color.primary.opacity(0.10)

struct PrimaryButton: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label.font(.subheadline.weight(.semibold)).padding(.horizontal, 16).frame(minHeight: 44)
            .foregroundStyle(Color(uiColor: .systemBackground)).background(Color.primary.opacity(configuration.isPressed ? 0.7 : 0.9), in: RoundedRectangle(cornerRadius: 9))
    }
}
struct SecondaryButton: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label.font(.subheadline.weight(.medium)).padding(.horizontal, 14).frame(minHeight: 44)
            .background(surface.opacity(configuration.isPressed ? 0.6 : 1), in: RoundedRectangle(cornerRadius: 9))
            .overlay(RoundedRectangle(cornerRadius: 9).stroke(outline))
    }
}
struct HomeActionButton: ButtonStyle {
    let primary: Bool
    func makeBody(configuration: Configuration) -> some View {
        configuration.label.font(.caption.weight(.semibold)).lineLimit(1)
            .padding(.horizontal, 11).frame(minHeight: 38)
            .foregroundStyle(primary ? Color(uiColor: .systemBackground) : Color.primary)
            .background(primary ? Color.primary.opacity(configuration.isPressed ? 0.7 : 0.9) : surface, in: RoundedRectangle(cornerRadius: 9))
            .overlay(RoundedRectangle(cornerRadius: 9).stroke(outline))
    }
}
struct RootView: View {
    @EnvironmentObject var store: Store
    @Environment(\.scenePhase) private var phase
    @State private var form: ContainerForm?
    @State private var showIntro = true
    @State private var newSpace = false
    @State private var confirmClearData = false
    @State private var importing = false
    @State private var importingPhotos = false
    @State private var selectedPhotos: [PhotosPickerItem] = []
    @State private var importingPackage = false
    @State private var pendingPackage: URL?
    @State private var deleting: BrowserEntry?
    var body: some View {
        NavigationStack {
            Group { if store.container == nil { home } else { browser } }
                .background(Color(uiColor: .systemGroupedBackground))
                .navigationTitle(store.container?.name ?? "我的容器")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar(store.container == nil ? .hidden : .visible, for: .navigationBar)
                .toolbar {
                    if store.container != nil { ToolbarItem(placement: .topBarLeading) { Button("锁定", systemImage: "lock") { Task { await store.lock() } } } }
                    ToolbarItem(placement: .topBarTrailing) {
                        Menu { if store.container == nil {
                            Button("新建空间", systemImage: "folder.badge.plus") { newSpace = true }
                            Button("导入加密备份", systemImage: "square.and.arrow.down") { importingPackage = true }
                            Divider()
                            Button("清理数据", systemImage: "trash", role: .destructive) { confirmClearData = true }
                        } else {
                            Button("从文件添加", systemImage: "folder") { importing = true }
                            Button("从相册添加", systemImage: "photo.on.rectangle") { importingPhotos = true }
                            Button("导出加密备份", systemImage: "square.and.arrow.up") { Task { await store.prepareExport() } }
                        } } label: { Image(systemName: "ellipsis.circle") }.disabled(store.busy)
                    }
                }
                .overlay { if store.busy { ProgressView().padding().background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14)) } }
        }
        .overlay {
            if showIntro {
                IntroView().transition(.opacity).allowsHitTesting(true)
            }
        }
        .task {
            do { try await Task.sleep(for: .seconds(1)) } catch { return }
            withAnimation(.easeOut(duration: 0.2)) { showIntro = false }
        }
        .privacySensitive()
        .overlay { if phase != .active { Color(uiColor: .systemBackground).ignoresSafeArea() } }
        .onChange(of: phase) { _, value in if value == .background { form = nil; deleting = nil; importing = false; importingPackage = false } }
        .tint(ink)
        .sheet(item: $form, onDismiss: { if let url = pendingPackage { url.stopAccessingSecurityScopedResource(); pendingPackage = nil } }) { ContainerFormView(form: $0) { form = nil } }
        .fileExporter(isPresented: Binding(get: { store.export != nil }, set: { if !$0 { store.export = nil } }), document: store.export?.document, contentType: .data, defaultFilename: store.export?.name ?? "Veil") { result in
            if case .failure(let error) = result { store.error = error.localizedDescription }; store.export = nil
        }
        .sheet(item: $store.preview) { PreviewView(item: $0) }
        .sheet(isPresented: $newSpace) { NewSpaceView() }
        .alert("清理所有本机数据？", isPresented: $confirmClearData) {
            Button("取消", role: .cancel) { }
            Button("清理所有数据", role: .destructive) { Task { await store.clearLocalData() } }
        } message: {
            Text("将永久删除此设备上 Veil 的所有空间、容器、文件和设置，无法撤销。请先导出需要保留的加密备份。")
        }
        .alert("清理完成", isPresented: Binding(get: { store.notice != nil }, set: { if !$0 { store.notice = nil } })) {
            Button("好") { store.notice = nil }
        } message: { Text(store.notice ?? "") }
        .alert("操作未完成", isPresented: Binding(get: { store.error != nil }, set: { if !$0 { store.error = nil } })) { Button("好") { store.error = nil } } message: { Text(store.error ?? "") }
        .confirmationDialog("删除后无法恢复", isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }), titleVisibility: .visible) {
            Button("删除", role: .destructive) { if let item = deleting { Task { await store.remove(item) } }; deleting = nil }
        }
        .photosPicker(isPresented: $importingPhotos, selection: $selectedPhotos, matching: .any(of: [.images, .videos]))
        .onChange(of: selectedPhotos) { _, items in
            guard !items.isEmpty else { return }
            selectedPhotos = []
            Task { await store.importPhotos(items) }
        }
        .fileImporter(isPresented: $importing, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            switch result { case .success(let urls): Task { await store.importFiles(urls) }; case .failure(let error): store.error = error.localizedDescription }
        }
        .fileImporter(isPresented: $importingPackage, allowedContentTypes: [.data]) { result in
            switch result { case .success(let url):
                guard let space = store.space else { store.error = "请先创建空间"; return }
                _ = url.startAccessingSecurityScopedResource(); pendingPackage = url
                form = .importPackage(path: url.path, space: space.id)
            case .failure(let error): store.error = error.localizedDescription }
        }
    }
    var home: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                HStack(spacing: 12) {
                    Image("Brand").resizable().scaledToFit().frame(width: 32, height: 32).clipShape(RoundedRectangle(cornerRadius: 7))
                    Text("veil").font(.headline)
                    Spacer()
                    Menu {
                        Section("切换空间") {
                            ForEach(store.spaces) { space in
                                Button { store.selectedSpace = space.id } label: {
                                    if space.id == store.space?.id { Label(space.name, systemImage: "checkmark") }
                                    else { Text(space.name) }
                                }
                            }
                            Button("新建空间", systemImage: "folder.badge.plus") { newSpace = true }
                        }
                        Section("其他操作") {
                            Button("导入加密备份", systemImage: "square.and.arrow.down") { importingPackage = true }
                            Button("清理数据", systemImage: "trash", role: .destructive) { confirmClearData = true }
                        }
                    } label: { HStack { Text(store.space?.name ?? "选择空间").lineLimit(1); Image(systemName: "chevron.down") }.font(.subheadline.weight(.medium)) }
                        .disabled(store.busy)
                }
                VStack(alignment: .leading, spacing: 14) {
                    HStack(spacing: 8) {
                        Text("\(store.space?.name ?? "空间")的容器").font(.headline).lineLimit(1)
                        Spacer(minLength: 4)
                        Button { if let space = store.space { form = .create(space: space.id) } } label: {
                            Label("创建", systemImage: "plus")
                        }.buttonStyle(HomeActionButton(primary: true)).disabled(store.space == nil)
                        Button { importingPackage = true } label: {
                            Label("导入", systemImage: "square.and.arrow.down")
                        }.buttonStyle(HomeActionButton(primary: false)).disabled(store.space == nil)
                    }
                    if let space = store.space {
                        if space.containers.isEmpty {
                            HStack(spacing: 12) {
                                Image(systemName: "folder").font(.title3).frame(width: 42, height: 42).background(mutedSurface, in: RoundedRectangle(cornerRadius: 9))
                                VStack(alignment: .leading, spacing: 5) { Text("这个空间还没有容器").font(.subheadline.weight(.semibold)); Text("创建或导入容器后，它会显示在这里。").font(.caption).foregroundStyle(.secondary) }
                            }.frame(maxWidth: .infinity, alignment: .leading).padding(16).background(surface, in: RoundedRectangle(cornerRadius: 12)).overlay(RoundedRectangle(cornerRadius: 12).stroke(outline, style: StrokeStyle(lineWidth: 1, dash: [4])))
                        }
                        ForEach(space.containers) { item in
                            Button { form = .open(path: item.path, name: item.name) } label: {
                                HStack(spacing: 12) {
                                    Image(systemName: "checkmark.shield").font(.title3).foregroundStyle(.secondary).frame(width: 42, height: 42).background(mutedSurface, in: RoundedRectangle(cornerRadius: 9))
                                    VStack(alignment: .leading, spacing: 5) { Text(item.name).font(.subheadline.weight(.semibold)); Text("本机 / \(space.name)").font(.caption).foregroundStyle(.secondary) }
                                    Spacer(); Image(systemName: "arrow.right").font(.caption).foregroundStyle(.secondary)
                                }.padding(15).background(surface, in: RoundedRectangle(cornerRadius: 12)).overlay(RoundedRectangle(cornerRadius: 12).stroke(outline))
                            }.buttonStyle(.plain)
                        }
                    }
                }
                HStack { Text("VEIL / PRIVATE BY DESIGN").tracking(1); Spacer(); Text("你的文件，你的空间") }.font(.system(size: 9)).foregroundStyle(.tertiary).padding(.top, 12)
            }.padding(20)
        }
    }
    var browser: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                HStack(spacing: 24) {
                    browserTab("按路径", mode: .path)
                    browserTab("按类型", mode: .type)
                    Spacer()
                    Button { store.layout = store.layout == .grid ? .list : .grid } label: { Image(systemName: store.layout == .grid ? "list.bullet" : "square.grid.2x2") }.accessibilityLabel(store.layout == .grid ? "列表" : "大图标")
                    Menu { importOptions } label: { Image(systemName: "plus") }.accessibilityLabel("添加内容")
                }
                if store.mode == .type {
                    ScrollView(.horizontal, showsIndicators: false) { HStack(spacing: 8) {
                        filterButton("全部", category: nil)
                        ForEach(FileCategory.allCases) { category in filterButton(category.title, category: category) }
                    } }
                }
                VStack(alignment: .leading, spacing: 16) {
                    HStack {
                        if store.mode == .path && !store.directory.isEmpty {
                            Button { store.directory = store.directory.split(separator: "/").dropLast().joined(separator: "/") } label: { Label("返回上一级", systemImage: "chevron.left") }
                        } else { Text(store.mode == .type ? "文件分类 / \(store.category?.title ?? "全部")" : "全部文件") }
                        Spacer(); Text("\(store.entries.count) 项")
                    }.font(.caption).foregroundStyle(.secondary)
                    if store.entries.isEmpty {
                        VStack(spacing: 10) {
                            Image(systemName: "folder").font(.largeTitle).foregroundStyle(.tertiary)
                            Text(store.search.isEmpty ? "这里还没有文件" : "没有找到文件").font(.subheadline.weight(.semibold))
                            Text(store.search.isEmpty ? "添加文件，安全地保存在容器中。" : "试试其他搜索内容。 ").font(.caption).foregroundStyle(.secondary)
                            if store.search.isEmpty { Menu { importOptions } label: { Label("添加内容", systemImage: "plus") }.buttonStyle(PrimaryButton()).padding(.top, 6) }
                        }.frame(maxWidth: .infinity).padding(.vertical, 65)
                    } else if store.layout == .grid {
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 98), spacing: 12)], spacing: 20) {
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
                        LazyVStack(spacing: 0) { ForEach(store.entries) { entry in
                            Button { activate(entry) } label: {
                                HStack(spacing: 12) { EntryThumbnail(entry: entry).frame(width: 40, height: 40); VStack(alignment: .leading, spacing: 4) { Text(entry.name).lineLimit(1); Text(detail(entry)).font(.caption).foregroundStyle(.secondary) }; Spacer(); Image(systemName: "chevron.right").font(.caption).foregroundStyle(.tertiary) }.padding(.vertical, 8)
                            }.buttonStyle(.plain).contextMenu { entryActions(entry) }
                            if entry.id != store.entries.last?.id { Divider() }
                        } }
                    }
                }.padding(16).frame(maxWidth: .infinity, alignment: .leading).background(surface, in: RoundedRectangle(cornerRadius: 12)).overlay(RoundedRectangle(cornerRadius: 12).stroke(outline))
                if !store.entries.isEmpty { Text("长按文件可导出或删除").font(.caption).foregroundStyle(.secondary) }
            }.padding(20)
        }.scrollDismissesKeyboard(.interactively).searchable(text: $store.search, prompt: "搜索文件")
    }
    @ViewBuilder var importOptions: some View {
        Button("从文件添加", systemImage: "folder") { importing = true }
        Button("从相册添加", systemImage: "photo.on.rectangle") { importingPhotos = true }
    }
    func browserTab(_ title: String, mode: BrowseMode) -> some View {
        Button { store.mode = mode; store.directory = ""; store.category = nil } label: {
            VStack(spacing: 8) { Text(title).font(.subheadline.weight(store.mode == mode ? .semibold : .regular)); Rectangle().fill(store.mode == mode ? Color.primary : .clear).frame(height: 2) }
        }.buttonStyle(.plain).foregroundStyle(store.mode == mode ? Color.primary : Color.secondary)
    }
    func filterButton(_ title: String, category: FileCategory?) -> some View {
        Button { store.category = category } label: { Text(title).font(.caption).padding(.horizontal, 12).padding(.vertical, 8).background(store.category == category ? mutedSurface : surface, in: RoundedRectangle(cornerRadius: 7)).overlay(RoundedRectangle(cornerRadius: 7).stroke(store.category == category ? Color.secondary : outline)) }.buttonStyle(.plain)
    }
    func activate(_ entry: BrowserEntry) {
        switch entry { case .folder(let path): store.directory = path; case .file(let file): Task { await store.show(file) } }
    }
    func detail(_ entry: BrowserEntry) -> String {
        switch entry { case .folder: return "文件夹"; case .file(let file): return store.mode == .type ? file.parentName : file.sizeLabel }
    }
    @ViewBuilder func entryActions(_ entry: BrowserEntry) -> some View {
        if case .file(let file) = entry { Button("导出", systemImage: "square.and.arrow.up") { Task { await store.prepareExport(file) } } }
        Button("删除", systemImage: "trash", role: .destructive) { deleting = entry }
    }

}

private struct EntryThumbnail: View {
    @EnvironmentObject private var store: Store
    let entry: BrowserEntry
    var body: some View {
        GeometryReader { geometry in
            ZStack {
                mutedSurface
                if case .file(let file) = entry, let image = store.thumbnails[store.thumbnailKey(file)] {
                    Image(uiImage: image).resizable().scaledToFill()
                        .frame(width: geometry.size.width, height: geometry.size.height).clipped()
                    if file.category == .video {
                        Image(systemName: "play.fill").font(.caption).foregroundStyle(.white)
                            .padding(7).background(.black.opacity(0.55), in: Circle())
                    }
                } else if case .file(let file) = entry, let text = store.textThumbnails[store.thumbnailKey(file)] {
                    Text(text).font(.system(size: 9, design: .monospaced)).foregroundStyle(.secondary)
                        .frame(width: geometry.size.width - 16, height: geometry.size.height - 16, alignment: .topLeading)
                        .clipped()
                } else {
                    Image(systemName: entry.symbol).font(.title3).foregroundStyle(.secondary)
                }
            }.clipShape(RoundedRectangle(cornerRadius: 9))
        }
        .task(id: entry.id) { if case .file(let file) = entry { await store.loadThumbnail(file) } }
    }
}

/// 启动时短暂展示品牌引导，随后自动进入首页。
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

struct NewSpaceView: View {
    @EnvironmentObject var store: Store
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
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
                    Text("存储位置").font(.subheadline.weight(.semibold))
                    HStack(spacing: 12) { Image(systemName: "folder").foregroundStyle(.secondary); VStack(alignment: .leading, spacing: 4) { Text("本机私有空间").font(.subheadline); Text("容器保存在此设备的应用目录中。").font(.caption).foregroundStyle(.secondary) }; Spacer() }.padding(14).background(surface, in: RoundedRectangle(cornerRadius: 9)).overlay(RoundedRectangle(cornerRadius: 9).stroke(outline))
                }
                HStack { Spacer(); Button("取消") { dismiss() }.buttonStyle(SecondaryButton()); Button("创建空间") { let value = name.trimmingCharacters(in: .whitespacesAndNewlines); Task { await store.createSpace(value); if store.error == nil { dismiss() } } }.buttonStyle(PrimaryButton()).disabled(store.busy || name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty) }
            }.padding(24).frame(maxWidth: 560).frame(maxWidth: .infinity)
            }.scrollDismissesKeyboard(.interactively).background(Color(uiColor: .systemGroupedBackground)).navigationBarTitleDisplayMode(.inline)
                .toolbar { ToolbarItem(placement: .cancellationAction) { Button("关闭") { dismiss() } } }
                .alert("操作未完成", isPresented: Binding(get: { store.error != nil }, set: { if !$0 { store.error = nil } })) { Button("好") { store.error = nil } } message: { Text(store.error ?? "") }
        }
    }
}

struct ContainerFormView: View {
    @EnvironmentObject var store: Store
    let form: ContainerForm
    let dismiss: () -> Void
    @State private var name = ""
    @State private var password = ""
    @State private var confirm = ""
    @State private var validation: String?
    private enum InputField: Hashable { case name, password, confirm }
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
                        if form.isCreate { field("容器名称") { TextField("例如：个人文件", text: $name).textInputAutocapitalization(.never).focused($focused, equals: .name).submitLabel(.next).onSubmit { focused = .password } }.id(InputField.name) }
                        else { HStack { Image(systemName: "checkmark.shield").foregroundStyle(.secondary); VStack(alignment: .leading, spacing: 5) { Text(form.detail).font(.subheadline.weight(.semibold)); Text("本机 / \(store.space?.name ?? "空间")").font(.caption).foregroundStyle(.secondary) }; Spacer() }.padding(14).background(mutedSurface, in: RoundedRectangle(cornerRadius: 9)) }
                        field("容器密码") { SecureField(form.isCreate ? "设置容器密码" : "输入密码解锁", text: $password).textContentType(form.isCreate ? .newPassword : .password).focused($focused, equals: .password).submitLabel(form.isCreate ? .next : .done).onSubmit { focused = form.isCreate ? .confirm : nil } }.id(InputField.password)
                        if form.isCreate { field("确认密码") { SecureField("再次输入密码", text: $confirm).textContentType(.newPassword).focused($focused, equals: .confirm).submitLabel(.done).onSubmit { focused = nil } }.id(InputField.confirm) }
                        if let validation { Text(validation).font(.footnote).foregroundStyle(.red) }
                        Button {
                            guard !password.isEmpty else { validation = "请输入密码"; return }
                            guard !form.isCreate || (!name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && password == confirm) else { validation = "请输入名称并确认两次密码一致"; return }
                            Task { if await store.open(form: form, name: name.trimmingCharacters(in: .whitespacesAndNewlines), password: password) { password = ""; confirm = ""; dismiss() } }
                        } label: { HStack { Spacer(); Text(form.isCreate ? "创建容器" : "解锁容器"); Spacer(); Image(systemName: "arrow.right") } }.buttonStyle(PrimaryButton()).disabled(store.busy)
                    }.padding(20).background(surface, in: RoundedRectangle(cornerRadius: 16)).overlay(RoundedRectangle(cornerRadius: 16).stroke(outline))
                    Label("密码用于本地解锁容器，请妥善保管。", systemImage: "lock").font(.caption).foregroundStyle(.secondary)
                }.padding(20).frame(maxWidth: 560).frame(maxWidth: .infinity)
            }
            .scrollDismissesKeyboard(.interactively)
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
    func field<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.subheadline.weight(.semibold))
            content().padding(12).background(Color(uiColor: .systemBackground), in: RoundedRectangle(cornerRadius: 8)).overlay(RoundedRectangle(cornerRadius: 8).stroke(outline))
        }
    }
}
struct PreviewView: View {
    let item: PreviewItem
    @Environment(\.dismiss) private var dismiss
    var body: some View {
        NavigationStack {
            Group {
                switch item.content {
                case .image(let url): ImagePreview(url: url)
                case .video(let url): VideoPreview(url: url)
                case .text(let text):
                    ScrollView([.vertical, .horizontal]) {
                        Text(text).font(.system(.body, design: .monospaced)).textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading).padding()
                    }.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                }
            }
            .navigationTitle(item.file.leafName).navigationBarTitleDisplayMode(.inline)
            .toolbar { Button("关闭") { dismiss() } }
        }
    }
}

private struct ImagePreview: View {
    let url: URL
    @State private var scale: CGFloat = 1
    @State private var offset: CGSize = .zero
    @GestureState private var pinch: CGFloat = 1
    @GestureState private var drag: CGSize = .zero
    var body: some View {
        GeometryReader { geometry in
            if let image = UIImage(contentsOfFile: url.path) {
                Image(uiImage: image).resizable().scaledToFit()
                    .frame(width: geometry.size.width, height: geometry.size.height)
                    .scaleEffect(scale * pinch)
                    .offset(x: offset.width + drag.width, y: offset.height + drag.height)
                    .gesture(MagnificationGesture().updating($pinch) { value, state, _ in state = value }
                        .onEnded { value in
                            scale = min(max(scale * value, 1), 6)
                            if scale == 1 { offset = .zero }
                        })
                    .simultaneousGesture(DragGesture().updating($drag) { value, state, _ in
                        if scale > 1 { state = value.translation }
                    }.onEnded { value in
                        guard scale > 1 else { return }
                        offset.width += value.translation.width; offset.height += value.translation.height
                        let limitX = geometry.size.width * (scale - 1) / 2
                        let limitY = geometry.size.height * (scale - 1) / 2
                        offset.width = min(max(offset.width, -limitX), limitX)
                        offset.height = min(max(offset.height, -limitY), limitY)
                    })
                    .onTapGesture(count: 2) { withAnimation { scale = scale == 1 ? 2 : 1; offset = .zero } }
                    .clipped()
            } else {
                ContentUnavailableView("无法预览图片", systemImage: "photo", description: Text("图片格式可能不受支持。"))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }.background(Color.black.opacity(0.95)).accessibilityLabel("图片预览，可双指缩放")
    }
}

private struct VideoPreview: View {
    let url: URL
    @State private var player: AVPlayer?
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
            let asset = AVURLAsset(url: url)
            do {
                let playable = try await asset.load(.isPlayable)
                guard !Task.isCancelled else { return }
                if playable { player = AVPlayer(playerItem: AVPlayerItem(asset: asset)) }
                else { failed = true }
            } catch { if !Task.isCancelled { failed = true } }
        }
        .onDisappear { player?.pause(); player?.replaceCurrentItem(with: nil); player = nil }
    }
}
