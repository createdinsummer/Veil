/// macOS 加密容器备份导入窗口。
/// 所属模块：桌面端导入流程；先选取 .veil 包，再收集密码并调用 AppStore 导入到当前空间。
/// 核心状态：selectedURL 保存通过初步校验的文件位置；password 保存待提交输入；showingPassword 决定步骤；selectionError / store.error 显示失败原因。
/// 依赖 AppKit 文件面板、SwiftUI 拖放、UniformTypeIdentifiers 和 AppStore。目标是引导用户选择单个普通 .veil 文件并提交，忙碌时禁用取消及交互式关闭。
import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// 备份导入的两步输入界面，保存文件选择、密码和步骤状态，由 Store 执行实际导入。
/// URL 初步校验不代表包内容有效；文件格式、密码和空间可用性还需要服务层验证。
struct ImportPackageView: View {
    /// 承担当前空间、忙碌状态和实际导入工作的应用 Store。
    @EnvironmentObject private var store: AppStore
    /// 已通过扩展名及普通文件校验的备份包 URL。
    @State private var selectedURL: URL?
    /// 是否进入密码确认步骤。
    @State private var showingPassword = false
    /// 当前导入密码；视图消失或任务提交时清空。
    @State private var password = ""
    /// 文件选择或拖放校验失败时显示的提示。
    @State private var selectionError: String?
    /// 拖放区域是否正被系统拖拽目标覆盖，用于突出可放置状态。
    @State private var isTargeted = false
    /// 请求密码输入框聚焦的界面状态。
    @FocusState private var passwordFocused: Bool

    /// 根据导入步骤和 Store 进度绘制窗口，并禁用进行中的关闭操作。
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("导入容器").font(.system(size: 20, weight: .semibold))
            Text(showingPassword ? "输入容器密码，导入到「\(store.activeSpace?.name ?? "本机")」。" : "选择 .veil 文件，导入到「\(store.activeSpace?.name ?? "本机")」。")
                .font(.system(size: 12)).foregroundStyle(VeilTheme.secondary)
            if showingPassword {
                HStack(spacing: 12) {
                    Image(systemName: "checkmark.shield").font(.system(size: 20))
                    VStack(alignment: .leading, spacing: 4) {
                        Text(selectedURL?.lastPathComponent ?? "").font(.system(size: 12, weight: .semibold)).lineLimit(1)
                        Text("导入到 \(store.activeSpace?.name ?? "本机")").font(.system(size: 11)).foregroundStyle(VeilTheme.muted)
                    }
                    Spacer()
                }.padding(14).frame(maxWidth: .infinity, alignment: .leading)
                    .background(VeilTheme.canvas, in: RoundedRectangle(cornerRadius: 9))
                VStack(alignment: .leading, spacing: 8) {
                    Text("容器密码").font(.system(size: 12, weight: .medium))
                    SecureField("输入容器密码", text: $password).textFieldStyle(.roundedBorder)
                        .focused($passwordFocused).disabled(store.isBusy)
                }
                if let error = store.error { Text(error).font(.system(size: 11)).foregroundStyle(VeilTheme.danger) }
            } else {
                Button(action: chooseFile) {
                    HStack(spacing: 13) {
                        Image(systemName: "doc").font(.system(size: 20)).foregroundStyle(VeilTheme.secondary)
                        VStack(alignment: .leading, spacing: 4) {
                            Text(selectedURL?.lastPathComponent ?? "选择或拖入文件")
                                .font(.system(size: 12, weight: .semibold)).lineLimit(1)
                            Text(selectedURL == nil ? "支持 .veil 文件" : "点击更换文件")
                                .font(.system(size: 11)).foregroundStyle(VeilTheme.muted)
                        }
                        Spacer()
                    }.padding(20).frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
                        .background(isTargeted ? VeilTheme.canvas : VeilTheme.card, in: RoundedRectangle(cornerRadius: 12))
                        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(VeilTheme.border, style: StrokeStyle(lineWidth: 1, dash: [5, 4])))
                }.buttonStyle(VeilPlainButtonStyle())
                    .dropDestination(for: URL.self) { urls, _ in
                        guard let url = urls.first, urls.count == 1 else { selectionError = AppLog.validation("请选择一个 .veil 文件"); return false }
                        return selectFile(url)
                    } isTargeted: { isTargeted = $0 }
                if let selectionError { Text(selectionError).font(.system(size: 11)).foregroundStyle(VeilTheme.danger) }
            }
            if let title = store.transferTitle {
                TransferProgressView(title: title, progress: store.transferProgress)
                    .frame(maxWidth: .infinity)
            }
            HStack {
                Spacer()
                Button("取消") { store.showImportPackage = false }.buttonStyle(VeilButtonStyle())
                    .keyboardShortcut(.cancelAction).disabled(store.isBusy)
                if showingPassword {
                    Button(store.isBusy ? "正在导入…" : "导入容器") { importPackage() }
                        .buttonStyle(VeilButtonStyle(variant: .primary)).keyboardShortcut(.defaultAction)
                        .disabled(store.isBusy || password.isEmpty)
                } else {
                    Button("继续") { showingPassword = true; passwordFocused = true }
                        .buttonStyle(VeilButtonStyle(variant: .primary)).keyboardShortcut(.defaultAction)
                        .disabled(selectedURL == nil)
                }
            }.padding(.top, 4)
        }.padding(25).frame(width: 430).background(VeilTheme.card)
            .interactiveDismissDisabled(store.isBusy)
            .onDisappear { password = ""; store.error = nil }
    }

    /// 让用户为加密备份导入选择一个候选文件。
    /// - Parameters: 无。
    /// - Returns: Void；取消或没有 URL 时保留当前选择，确认后交给 selectFile 校验。
    /// - Throws: 不向调用方抛出异常。
    /// - 注意事项: 主线程显示模态面板；UTType 无法识别 veil 时以通用数据类型筛选，仍需后续扩展名检查。
    private func chooseFile() {
        let panel = NSOpenPanel()
        panel.title = "选择 .veil 文件"
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.allowedContentTypes = [UTType(filenameExtension: "veil") ?? .data]
        guard panel.runModal() == .OK, let url = panel.url else { return }
        _ = selectFile(url)
    }

    @discardableResult
    /// 检查候选位置是否适合作为备份导入入口，并更新表单选择。
    /// - Parameters: url: URL 文件面板或拖放提供的位置；要求本地文件 URL、大小写不敏感的 veil 扩展名及普通文件属性。
    /// - Returns: Bool；初步校验通过为 true 并清除提示，否则 false、清空原选择并显示提示。
    /// - Throws: 不向调用方抛错；属性读取失败通过 AppLog.attempt 尝试记录，并按校验失败处理。
    /// - 注意事项: 不读取或验证备份内容，也不在此申请安全作用域访问；沙盒权限不足会导致属性校验失败，稍后文件仍可能被移动或修改。
    private func selectFile(_ url: URL) -> Bool {
        guard url.isFileURL, url.pathExtension.lowercased() == "veil",
              (AppLog.attempt({ try url.resourceValues(forKeys: [.isRegularFileKey]) })?.isRegularFile) == true else {
            selectedURL = nil
            selectionError = AppLog.validation("请选择 .veil 文件")
            return false
        }
        selectedURL = url
        selectionError = nil
        return true
    }

    /// 用本次选择的备份、空间和口令启动异步导入。
    /// - Parameters: 无；从 selectedURL、当前 activeSpace 和 password 获取输入。
    /// - Returns: Void；缺少文件或空间、Store 忙碌时忽略，否则清空密码输入后创建任务；任务结束时无错误且有打开容器才关闭窗口。
    /// - Throws: 不向调用方抛出异常；具体业务错误由 Store 处理。
    /// - 注意事项: 文件和空间在提交时捕获，实际内容由服务验证；此方法自身不拒绝空密码，按钮禁用只是界面约束。任务持有密码副本，不保证内存清零；末尾关闭判断读取共享 Store 状态。
    private func importPackage() {
        guard let url = selectedURL, let space = store.activeSpace, !store.isBusy else { return }
        // 把口令交给任务持有，再清空输入状态；普通 String 不保证旧内存缓冲区被擦除。
        let secret = password
        password = ""
        Task {
            // 请求系统授予的安全作用域访问；返回 false 不直接中止（应用本来可访问时也可能为 false）。
            // 仅在成功取得作用域后成对结束访问，不会永久保存访问书签。
            let scoped = url.startAccessingSecurityScopedResource()
            defer { if scoped { url.stopAccessingSecurityScopedResource() } }
            await store.submit(.importPackage(path: url.path, space: space.id), name: "", password: secret, confirmation: "")
            if store.error == nil, store.container != nil { store.showImportPackage = false }
        }
    }
}
