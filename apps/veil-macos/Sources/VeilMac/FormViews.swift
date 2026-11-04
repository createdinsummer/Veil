import AppKit
import SwiftUI

struct ContainerFormView: View {
    @EnvironmentObject private var store: AppStore
    let form: ContainerForm
    @State private var name = ""
    @State private var password = ""
    @State private var confirmation = ""
    @FocusState private var focused: Field?
    private enum Field { case name, password, confirmation }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                Button { store.cancelForm() } label: {
                    Label("返回容器", systemImage: "arrow.left").font(.system(size: 12))
                }.buttonStyle(.plain).foregroundStyle(VeilTheme.secondary)
                    .keyboardShortcut(.cancelAction).disabled(store.isBusy).padding(.bottom, 31)
                VStack(spacing: 11) {
                    Image(systemName: form.isCreate ? "lock.shield" : "lock.open")
                        .font(.system(size: 23, weight: .light)).foregroundStyle(VeilTheme.secondary)
                        .frame(width: 50, height: 50).background(VeilTheme.sidebar, in: RoundedRectangle(cornerRadius: 14))
                        .padding(.bottom, 4)
                    Text(form.isCreate ? "A SPACE OF YOUR OWN" : "WELCOME BACK")
                        .font(.system(size: 10, weight: .semibold)).tracking(1.6).foregroundStyle(VeilTheme.muted)
                    Text(form.title).font(.system(size: 27, weight: .bold)).foregroundStyle(VeilTheme.heading)
                    Text(form.isCreate ? form.detail : "输入密码，打开你的私密空间。")
                        .font(.system(size: 13)).foregroundStyle(VeilTheme.secondary)
                }.frame(maxWidth: .infinity)
                VStack(alignment: .leading, spacing: 19) {
                    if !form.isCreate {
                        HStack(spacing: 12) {
                            Image(systemName: "checkmark.shield").font(.system(size: 22, weight: .light))
                                .foregroundStyle(VeilTheme.secondary)
                            VStack(alignment: .leading, spacing: 6) {
                                Text(form.detail).font(.system(size: 12, weight: .semibold)).lineLimit(2)
                                Text("本机 / \(store.activeSpace?.name ?? "本机")")
                                    .font(.system(size: 10)).foregroundStyle(VeilTheme.muted)
                            }
                            Spacer(minLength: 0)
                        }.padding(15).frame(maxWidth: .infinity, alignment: .leading)
                            .background(VeilTheme.canvas, in: RoundedRectangle(cornerRadius: 10))
                    }
                    if let error = store.error {
                        FeedbackBanner(message: error, isError: true) { store.error = nil }
                            .clipShape(RoundedRectangle(cornerRadius: 8))
                    }
                    if form.isCreate {
                        fieldLabel("容器名称") {
                            TextField("例如：个人文件", text: $name).focused($focused, equals: .name)
                        }
                    }
                    fieldLabel("容器密码") {
                        SecureField(form.isCreate ? "设置容器密码" : "输入密码解锁", text: $password)
                            .focused($focused, equals: .password)
                    }
                    if form.isCreate {
                        fieldLabel("确认密码") {
                            SecureField("再次输入密码", text: $confirmation).focused($focused, equals: .confirmation)
                        }
                    }
                    Button(action: submit) {
                        HStack(spacing: 8) {
                            if store.isBusy { ProgressView().controlSize(.mini).tint(VeilTheme.onAccent) }
                            Text(store.isBusy ? "正在处理…" : form.title)
                            if !store.isBusy { Image(systemName: "arrow.right") }
                        }.frame(maxWidth: .infinity)
                    }.buttonStyle(VeilButtonStyle(variant: .primary)).keyboardShortcut(.defaultAction)
                        .disabled(store.isBusy).padding(.top, 3)
                    Label("密码仅用于本地加密。忘记密码后无法恢复。", systemImage: "lock")
                        .font(.system(size: 10)).foregroundStyle(VeilTheme.muted).frame(maxWidth: .infinity)
                    if !form.isCreate {
                        Label(store.unlockCacheSeconds > 0
                              ? "解锁成功后，密码只在本机内存中保留 \(store.unlockCacheDescription)，期间重新打开此容器无需再次输入。"
                              : "免密重开已关闭，每次打开都需要输入密码。",
                              systemImage: "clock")
                            .font(.system(size: 10)).foregroundStyle(VeilTheme.muted)
                            .multilineTextAlignment(.center).frame(maxWidth: .infinity)
                    }
                }.padding(27).background(VeilTheme.card, in: RoundedRectangle(cornerRadius: 16))
                    .overlay(RoundedRectangle(cornerRadius: 16).stroke(VeilTheme.border))
                    .shadow(color: .black.opacity(0.02), radius: 15, y: 10).padding(.top, 29)
            }.frame(maxWidth: 520).padding(.horizontal, VeilDesign.layout("content-padding-x"))
                .padding(.top, 27).padding(.bottom, 45).frame(maxWidth: .infinity)
        }.onAppear { focused = form.isCreate ? .name : .password }
            .onDisappear { password = ""; confirmation = "" }
    }

    private func fieldLabel<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 9) {
            Text(title).font(.system(size: 12, weight: .medium))
            content().font(.system(size: 12)).textFieldStyle(.plain).accessibilityLabel(title)
                .padding(.horizontal, 13).frame(height: VeilDesign.layout("field-height"))
                .background(VeilTheme.card, in: RoundedRectangle(cornerRadius: VeilDesign.radius("control")))
                .overlay(RoundedRectangle(cornerRadius: VeilDesign.radius("control"))
                    .stroke(focusedTitle == title ? VeilTheme.secondary : VeilTheme.border))
                .disabled(store.isBusy)
        }
    }

    private var focusedTitle: String? {
        switch focused {
        case .name: "容器名称"
        case .password: "容器密码"
        case .confirmation: "确认密码"
        case nil: nil
        }
    }

    private func submit() {
        guard !store.isBusy else { return }
        let secret = password, repeated = confirmation
        password = ""
        confirmation = ""
        Task { await store.submit(form, name: name, password: secret, confirmation: repeated) }
    }
}

struct NewSpaceView: View {
    @EnvironmentObject private var store: AppStore
    @State private var name = ""
    @State private var directory: URL?
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            Label("新建空间", systemImage: "folder.badge.plus").font(.title2.weight(.semibold))
            Text("用不同空间整理工作与生活中的加密容器。").font(.callout).foregroundStyle(.secondary)
            if let error = store.error { FeedbackBanner(message: error, isError: true) { store.error = nil } }
            VStack(alignment: .leading, spacing: 9) {
                Text("空间名称").font(.system(size: 12, weight: .medium))
                TextField("例如：工作文件", text: $name).textFieldStyle(.plain).focused($focused)
                    .accessibilityLabel("空间名称").font(.system(size: 12)).padding(.horizontal, 13)
                    .frame(height: VeilDesign.layout("field-height"))
                    .background(VeilTheme.canvas, in: RoundedRectangle(cornerRadius: 8))
                    .overlay(RoundedRectangle(cornerRadius: 8).stroke(VeilTheme.border))
            }.disabled(store.isBusy)
            VStack(alignment: .leading, spacing: 9) {
                HStack {
                    Text("存储位置").font(.system(size: 12, weight: .medium))
                    Spacer()
                    if directory != nil {
                        Button("恢复默认位置") { directory = nil }.buttonStyle(.plain)
                            .font(.system(size: 11)).foregroundStyle(VeilTheme.secondary)
                    }
                }
                HStack(spacing: 10) {
                    Image(systemName: "folder").foregroundStyle(VeilTheme.secondary)
                    VStack(alignment: .leading, spacing: 5) {
                        Text(directory?.lastPathComponent ?? "默认位置")
                            .font(.system(size: 12, weight: .medium)).lineLimit(1)
                        Text(directory?.path ?? "由 Veil 自动管理")
                            .font(.system(size: 10)).foregroundStyle(VeilTheme.muted)
                            .lineLimit(2).truncationMode(.middle).textSelection(.enabled)
                    }.frame(maxWidth: .infinity, alignment: .leading)
                    Button("选择目录…", action: chooseDirectory).buttonStyle(VeilButtonStyle())
                }.padding(12).background(VeilTheme.canvas, in: RoundedRectangle(cornerRadius: 9))
                    .overlay(RoundedRectangle(cornerRadius: 9).stroke(VeilTheme.border))
                Text("此目录用于存放该空间内新建的容器。")
                    .font(.system(size: 10)).foregroundStyle(VeilTheme.muted)
            }.disabled(store.isBusy)
            HStack {
                if store.isBusy { ProgressView().controlSize(.small) }
                Spacer()
                Button("取消") { store.error = nil; store.showNewSpace = false }.keyboardShortcut(.cancelAction).buttonStyle(VeilButtonStyle()).disabled(store.isBusy)
                Button("创建空间") { Task { await store.createSpace(name: name, directory: directory) } }
                    .buttonStyle(VeilButtonStyle(variant: .primary)).keyboardShortcut(.defaultAction).disabled(store.isBusy)
            }
        }.padding(30).frame(width: 400).foregroundStyle(VeilTheme.text).background(VeilTheme.card).tint(VeilTheme.accent)
            .interactiveDismissDisabled(store.isBusy).onAppear { focused = true }
    }

    private func chooseDirectory() {
        let panel = NSOpenPanel()
        panel.title = "选择空间存储目录"
        panel.prompt = "选择目录"
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        panel.directoryURL = directory
        if panel.runModal() == .OK { directory = panel.url }
    }
}
