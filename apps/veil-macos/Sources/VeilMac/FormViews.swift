/// macOS 容器密码表单和新建空间弹窗。
/// 所属模块：桌面端容器 / 空间创建与解锁入口。ContainerFormView 收集容器名称与密码；NewSpaceView 收集空间名称和可选磁盘位置。
/// 依赖 AppStore 提交表单、展示错误，SwiftUI 管理输入状态，AppKit 文件面板选择空间目录。整体目标是把敏感输入留在本机界面并交给服务层处理。
import AppKit
import SwiftUI

/// 创建 / 解锁容器的输入界面；只保存本次输入与焦点，不自行访问加密数据库。
/// form 决定当前是创建还是解锁；name、password 和 confirmation 是本次提交的表单输入。
struct ContainerFormView: View {
    /// 接收表单提交并提供当前容器 / 空间状态的共享应用对象。
    @EnvironmentObject private var store: AppStore
    /// 表单业务类型、标题和说明；创建与解锁使用同一视图但显示不同字段。
    let form: ContainerForm
    /// 创建容器时填写的名称；解锁现有容器时不参与提交。
    @State private var name = ""
    /// 本次创建或解锁使用的容器密码。
    @State private var password = ""
    /// 创建容器时再次输入的密码，用于提交前核对一致性。
    @State private var confirmation = ""
    /// 当前获得键盘输入的字段，用于焦点导航和字段边框提示。
    @FocusState private var focused: Field?
    /// 表单内可获得焦点的业务输入项。
    private enum Field { case name, password, confirmation }

    /// 根据创建 / 解锁场景组合输入项、错误反馈和提交按钮。
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                Button { store.cancelForm() } label: {
                    Label("返回容器", systemImage: "arrow.left").font(.system(size: 12))
                }.buttonStyle(VeilPlainButtonStyle()).foregroundStyle(VeilTheme.secondary)
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
            // 离开时移除表单持有的输入值，不代表异步任务或缓存中的密码副本也被清除。
            .onDisappear { password = ""; confirmation = "" }
    }

    /// 为容器表单字段提供一致的标题、输入外观和忙碌禁用规则。
    /// - Parameters:
    ///   - title: String 用户可见标题，同时作为无障碍说明和焦点边框匹配键。
    ///   - content: () -> Content 输入控件构建闭包，Content 为符合 View 的类型。
    /// - Returns: some View，包含标题和输入控件的字段视图。
    /// - Throws: 不抛出异常。
    /// - 注意事项: SwiftUI 求值时构建界面；调用方需设置对应焦点，title 必须与 focusedTitle 返回文字一致才显示聚焦边框。
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

    /// 把当前焦点字段转换为界面标题；未聚焦时返回 nil。
    private var focusedTitle: String? {
        switch focused {
        case .name: "容器名称"
        case .password: "容器密码"
        case .confirmation: "确认密码"
        case nil: nil
        }
    }

    /// 将用户输入交给 Store 创建或解锁容器，并先清空表单里的密码显示状态。
    /// - Parameters: 无；读取当前 form、name、password 和 confirmation。
    /// - Returns: Void；忙碌时忽略，其余创建异步任务，结果由 Store 更新界面或错误提示。
    /// - Throws: 不向调用方抛出异常，Store 负责报告业务错误。
    /// - 注意事项: 仅复制密码后置空表单字符串，不会清零所有内存副本；任务仍持有口令直到使用完毕。忙碌检查不预先占用任务槽，不能充当互斥锁。
    private func submit() {
        guard !store.isBusy else { return }
        // 任务使用本次密码快照；Swift String 的置空不保证旧缓冲区被安全擦除。
        let secret = password, repeated = confirmation
        password = ""
        confirmation = ""
        // 名称在任务执行时读取；具体校验和成功后的导航由 Store 统一处理。
        Task { await store.submit(form, name: name, password: secret, confirmation: repeated) }
    }
}
/// 新建存储空间弹窗，收集名称和可选目录，再交给 AppStore 创建登记。
/// name 是待创建空间的名称；directory 为用户选择的位置，nil 表示采用应用默认目录。
struct NewSpaceView: View {
    /// 创建空间并报告操作状态的应用对象。
    @EnvironmentObject private var store: AppStore
    /// 用户输入的空间显示名称。
    @State private var name = ""
    /// 可选的磁盘目录；nil 表示使用 Veil 管理的默认位置。
    @State private var directory: URL?
    /// 弹窗出现后将焦点放到空间名称输入框。
    @FocusState private var focused: Bool

    /// 展示空间名称、存储位置、错误提示和创建 / 取消操作。
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
                        Button("恢复默认位置") { directory = nil }.buttonStyle(VeilPlainButtonStyle())
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

    /// 让用户选择新空间的存储位置。
    /// - Parameters: 无；以已有 directory 作为面板起始目录。
    /// - Returns: Void；确认时将面板 URL 写入 directory，取消时保持原选择。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 应在主线程调用；模态面板等待选择，允许用户在面板中新建目录，但不创建空间登记或容器。
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
