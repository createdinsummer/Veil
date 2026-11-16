/// macOS 设置窗口及其用户偏好、打开方式和设备配置行。
/// 所属模块：桌面端本机设置；偏好通过 AppStore 写入共享数据库，系统配置展示本机设备和默认空间。
/// 核心视图：SettingsView 按业务分区组装控件；SettingsRow / SettingsSection 统一设置行与卡片布局。
/// 依赖 AppKit 复制设备标识、SwiftUI 设置控件和 AppStore 持久化动作。整体目标是让设置变更即时生效，并显示保存结果或错误。
import AppKit
import SwiftUI

/// 独立设置窗口，读取共享 Store 并发起用户配置、外部打开规则和系统设置的保存请求。
/// 视图自身不写数据库；控件修改后由异步任务保存，失败和并发更新行为取决于 Store，并不保证立即落盘。
struct SettingsView: View {
    /// 读取并更新本机设置、应用偏好和保存反馈的共享状态。
    @EnvironmentObject private var store: AppStore

    /// 按用户偏好、打开方式和系统配置分区展示可编辑设置。
    var body: some View {
        VStack(spacing: 0) {
            header
            Rectangle().fill(VeilTheme.border).frame(height: 1)
            ScrollView {
                VStack(alignment: .leading, spacing: 26) {
                    SettingsSection(title: "用户配置", detail: "本机偏好，保存于 app.db 的 user_setting 表，与命令行共用。") {
                        hintLevelRow
                        rowDivider
                        layoutRow
                        rowDivider
                        browseModeRow
                        rowDivider
                        unlockCacheRow
                        rowDivider
                        importIntervalRow
                        rowDivider
                        importCountRow
                    }
                    SettingsSection(title: "打开方式", detail: "按后缀指定默认应用；点击该后缀的文件会先解密到临时文件，再交给指定应用打开，锁定或退出后清理。未指定的后缀在应用内预览。") {
                        openWithRows
                        rowDivider
                        openWithAddRow
                        rowDivider
                        temporaryDirectoryRow
                    }
                    SettingsSection(title: "系统配置", detail: "本机系统信息，来自 app.db 的 system_setting 表。") {
                        deviceRow
                        rowDivider
                        defaultSpaceRow
                    }
                }
                .padding(22)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            Rectangle().fill(VeilTheme.border).frame(height: 1)
            footer
        }
        .frame(width: 520)
        .background(VeilTheme.canvas)
        .foregroundStyle(VeilTheme.text)
        // 每次提示值变化重启三秒倒计时；被取消就保留新任务处理，结束时再次比对避免清除不同文案。
        .task(id: store.settingsFeedback) {
            guard let feedback = store.settingsFeedback else { return }
            do { try await Task.sleep(for: .seconds(3)) } catch { return }
            if store.settingsFeedback == feedback { store.settingsFeedback = nil }
        }
        // 绑定先更新内存布局，再异步保存完整偏好；连续切换可同时存在多个保存任务。
        .onChange(of: store.layout) { _, _ in Task { await store.savePreferences() } }
        .onChange(of: store.browseMode) { _, _ in Task { await store.savePreferences() } }
    }

    /// 窗口标题和即时保存说明。
    private var header: some View {
        HStack {
            VStack(alignment: .leading, spacing: 3) {
                Text("设置").font(.system(size: 16, weight: .semibold))
                Text("调整 Veil 的偏好与本机配置，修改即时保存。").font(.system(size: 11)).foregroundStyle(VeilTheme.secondary)
            }
            Spacer()
        }
        .padding(.horizontal, 22).padding(.vertical, 14)
    }

    /// 按优先级展示最近一次保存成功提示、错误或默认保存说明。
    private var footer: some View {
        Group {
            if let feedback = store.settingsFeedback {
                HStack(spacing: 8) {
                    Image(systemName: "checkmark.circle")
                    Text(feedback).lineLimit(2)
                    Spacer()
                    Button { store.settingsFeedback = nil } label: { Image(systemName: "xmark") }
                        .buttonStyle(VeilPlainButtonStyle()).accessibilityLabel("关闭提示")
                }
                .font(.system(size: 12)).foregroundStyle(VeilTheme.secondary)
                .padding(.horizontal, 22).padding(.vertical, 12).background(VeilTheme.thumbnail)
            } else if let error = store.error {
                HStack(spacing: 8) {
                    Image(systemName: "exclamationmark.circle")
                    Text(error).lineLimit(2)
                    Spacer()
                    Button { store.error = nil } label: { Image(systemName: "xmark") }
                        .buttonStyle(VeilPlainButtonStyle()).accessibilityLabel("关闭提示")
                }
                .font(.system(size: 12)).foregroundStyle(VeilTheme.danger)
                .padding(.horizontal, 22).padding(.vertical, 12).background(VeilTheme.dangerSurface)
            } else {
                Text("设置即时保存到本机 app.db").font(.system(size: 11)).foregroundStyle(VeilTheme.muted)
                    .padding(.horizontal, 22).padding(.vertical, 12).frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }

    /// 在同一设置分区内分隔相邻配置行。
    private var rowDivider: some View {
        Rectangle().fill(VeilTheme.border).frame(height: 1).padding(.leading, 14)
    }

    // MARK: 用户配置

    /// 配置命令行提示详细程度；绑定 setter 创建异步保存任务，不等待持久化成功才返回。
    private var hintLevelRow: some View {
        SettingsRow("提示级别", detail: "命令行提示的详细程度。") {
            Picker("提示级别", selection: Binding(
                get: { store.hintLevel },
                set: { level in Task { await store.setHintLevel(level) } }
            )) {
                ForEach(HintLevel.allCases, id: \.self) { level in Text(level.title).tag(level) }
            }
            .labelsHidden().pickerStyle(.segmented).fixedSize()
        }
    }

    /// 选择文件浏览器使用网格卡片还是列表行。
    private var layoutRow: some View {
        SettingsRow("布局", detail: "文件以网格或列表展示。") {
            Picker("布局", selection: $store.layout) {
                Text("网格").tag(FileLayout.grid)
                Text("列表").tag(FileLayout.list)
            }
            .labelsHidden().pickerStyle(.segmented).fixedSize()
        }
    }

    /// 选择按文件类别聚合浏览或按目录层级浏览。
    private var browseModeRow: some View {
        SettingsRow("浏览方式", detail: "按类型查看，或按目录层级浏览。") {
            Picker("浏览方式", selection: $store.browseMode) {
                Text("按类型").tag(BrowseMode.type)
                Text("按路径").tag(BrowseMode.path)
            }
            .labelsHidden().pickerStyle(.segmented).fixedSize()
        }
    }

    /// 设置解锁密码在本机内存中复用的时长；0 表示每次都要求输入。
    private var unlockCacheRow: some View {
        SettingsRow("免密时长", detail: "解锁后重新打开同一容器免输密码的时长；导入/导出等长任务进行中会自动顺延。") {
            Picker("免密时长", selection: Binding(
                get: { store.unlockCacheSeconds },
                set: { seconds in Task { await store.setUnlockCache(seconds: seconds) } }
            )) {
                ForEach([0, 900, 1800, 3600, 7200, 14400], id: \.self) { seconds in
                    Text(AppStore.durationLabel(seconds)).tag(seconds)
                }
            }
            .labelsHidden().pickerStyle(.menu).frame(width: 120)
        }
    }

    /// 设置批量导入合并提交的时间阈值，界面范围为 1 至 60 秒；不表示每条记录一定在此时限内完成。
    private var importIntervalRow: some View {
        SettingsRow("导入提交间隔", detail: "批量导入时合并写入加密库的等待时间。") {
            Stepper(value: Binding(
                get: { store.importCommitIntervalSeconds },
                set: { seconds in Task { await store.setImportCommitInterval(seconds: seconds) } }
            ), in: 1...60) {
                Text("\(store.importCommitIntervalSeconds) 秒").monospacedDigit()
                    .frame(minWidth: 44, alignment: .trailing)
            }
        }
    }

    /// 设置累计多少条待导入文件记录后触发合并提交。
    private var importCountRow: some View {
        SettingsRow("导入提交数量", detail: "累计多少条文件记录后合并写入。") {
            Stepper(value: Binding(
                get: { store.importCommitCount },
                set: { count in Task { await store.setImportCommitCount(count) } }
            ), in: 1...100) {
                Text("\(store.importCommitCount) 条").monospacedDigit()
                    .frame(minWidth: 44, alignment: .trailing)
            }
        }
    }

    // MARK: 打开方式

    /// 展示按文件后缀登记的默认外部应用；空集合时提示使用内置预览。
    private var openWithRows: some View {
        VStack(spacing: 0) {
            if store.openWithExtensions.isEmpty {
                HStack {
                    Text("尚未指定后缀，点击文件会在应用内预览。")
                        .font(.system(size: 11.5)).foregroundStyle(VeilTheme.muted)
                    Spacer()
                }
                .padding(.horizontal, 14).padding(.vertical, 11)
            } else {
                // 每轮展示一个已排序后缀的应用关联；索引只用于首行不画分隔线，后缀作为稳定标识。
                ForEach(Array(store.openWithExtensions.enumerated()), id: \.element) { index, ext in
                    if index > 0 {
                        Rectangle().fill(VeilTheme.border).frame(height: 1).padding(.leading, 14)
                    }
                    openWithRow(ext)
                }
            }
        }
    }

    /// 展示一个后缀已登记的外部应用及更换、移除操作。
    /// - Parameters: ext: String，规则映射中的扩展名键，正常由登记流程规范化为不带点的小写后缀。
    /// - Returns: some View，显示后缀、当前可解析的应用图标和名称，或无法定位的 bundle ID。
    /// - Throws: 不向调用方抛出异常；保存操作的错误由 Store 处理。
    /// - 注意事项: 每次求值查询系统应用位置，不保证稍后能启动；按钮创建异步任务，本视图不对多次规则修改加互斥锁。
    private func openWithRow(_ ext: String) -> some View {
        HStack(spacing: 10) {
            Text(".\(ext)")
                .font(.system(size: 12, design: .monospaced))
                .frame(width: 62, alignment: .leading)
            if let application = ExternalApplication.resolve(
                bundleID: store.openWithDefaults[ext] ?? "")
            {
                Image(nsImage: application.icon).resizable().frame(width: 16, height: 16)
                Text(application.name).font(.system(size: 12)).lineLimit(1)
            } else {
                Text("应用未安装（\(store.openWithDefaults[ext] ?? "—")）")
                    .font(.system(size: 11.5)).foregroundStyle(VeilTheme.muted).lineLimit(1)
            }
            Spacer(minLength: 8)
            Button("更改") {
                Task { await store.changeDefaultOpenApplication(forExtension: ext) }
            }.buttonStyle(VeilPlainButtonStyle())
            Button("移除", role: .destructive) {
                Task { await store.removeDefaultOpenApplication(forExtension: ext) }
            }.buttonStyle(VeilPlainButtonStyle())
        }
        .padding(.horizontal, 14).padding(.vertical, 9)
    }

    /// 展示临时根目录及打开入口；说明文字描述清理时机，实际删除失败只记日志，不保证无残留。
    private var temporaryDirectoryRow: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("临时副本目录").font(.system(size: 12, weight: .medium))
                Spacer()
                Button("在访达中打开", systemImage: "folder") {
                    store.openTemporaryDirectoryInFinder()
                }.buttonStyle(VeilPlainButtonStyle())
            }
            Text(TemporaryFiles.directory.path)
                .font(.system(size: 10.5, design: .monospaced))
                .foregroundStyle(VeilTheme.secondary)
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
            Text("解密后的临时文件保存在这里；启动 Veil 时会清空该目录下的全部内容（目录本身保留），关闭对应应用、锁定容器或退出 Veil 时也会清理。")
                .font(.system(size: 10.5)).foregroundStyle(VeilTheme.muted)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(.horizontal, 14).padding(.vertical, 11)
    }

    /// 提示默认应用规则，并打开新增后缀与应用的选择流程。
    private var openWithAddRow: some View {
        HStack {
            Text("选择应用打开时也会提示是否设为默认；打开前会先解密到临时文件。")
                .font(.system(size: 11.5)).foregroundStyle(VeilTheme.muted)
            Spacer()
            Button("添加后缀…") { Task { await store.addDefaultOpenApplication() } }
                .buttonStyle(VeilPlainButtonStyle())
        }
        .padding(.horizontal, 14).padding(.vertical, 11)
    }

    // MARK: 系统配置

    /// 展示本机设备 ID，并允许复制供设备识别或诊断使用。
    private var deviceRow: some View {
        SettingsRow("设备 ID", detail: "本机设备身份，用于识别设备。") {
            HStack(spacing: 6) {
                Text(store.deviceID.isEmpty ? "—" : store.deviceID)
                    .font(.system(size: 11, design: .monospaced)).foregroundStyle(VeilTheme.secondary)
                    .lineLimit(1).truncationMode(.middle).frame(maxWidth: 200, alignment: .trailing)
                Button {
                    NSPasteboard.general.clearContents()
                    // 未检查写入剪贴板的 Bool 结果，因此后续提示只表示已尝试复制。
                    NSPasteboard.general.setString(store.deviceID, forType: .string)
                    store.settingsFeedback = "设备 ID 已拷贝"
                } label: { Image(systemName: "doc.on.doc") }
                    .buttonStyle(VeilIconButtonStyle()).disabled(store.deviceID.isEmpty)
                    .help("拷贝设备 ID").accessibilityLabel("拷贝设备 ID")
            }
        }
    }

    /// 选择新建容器默认放置的空间。
    private var defaultSpaceRow: some View {
        SettingsRow("默认空间", detail: "新容器默认存放的空间。") {
            Picker("默认空间", selection: defaultSpaceBinding) {
                // 每轮一个注册空间，离线空间也显示；这里没有按可用性过滤或禁用单个选项。
                ForEach(store.spaces) { space in
                    Text("\(space.name) · \(space.path)").tag(space.id)
                }
            }
            .labelsHidden().pickerStyle(.menu).frame(maxWidth: 230)
            .disabled(store.spaces.isEmpty)
        }
    }

    /// 读取列表中首个 isDefault 空间的 ID，无匹配返回空字符串；写入时异步请求 Store 保存，不直接修改列表。
    private var defaultSpaceBinding: Binding<String> {
        Binding(
            get: { store.spaces.first(where: \.isDefault)?.id ?? "" },
            set: { id in Task { await store.setDefaultSpace(id) } }
        )
    }
}

/// 设置行：左侧标题与说明，右侧控件。
private struct SettingsRow<Control: View>: View {
    /// 该设置项的业务名称。
    let title: String
    /// 帮助用户理解设置影响的可选说明。
    let detail: String?
    /// 调用方构建的 Picker、Stepper 或其他交互控件。
    private let control: Control

    /// 构建一个左侧说明、右侧交互控件的设置行。
    /// - Parameters: title: String 设置项标题；detail: String? 可选说明，默认 nil 不显示说明；control: () -> Control 构建交互视图的闭包，Control 符合 View。
    /// - Returns: SettingsRow 实例，保存标题、说明及闭包本次求值生成的控件。
    /// - Throws: 不抛出异常。
    /// - 注意事项: control 在初始化时调用，不保存为待执行闭包；该行不负责设置的校验或持久化。
    init(_ title: String, detail: String? = nil, @ViewBuilder control: () -> Control) {
        self.title = title
        self.detail = detail
        self.control = control()
    }

    /// 将设置标题与说明放在左侧，将实际交互控件放在右侧。
    var body: some View {
        HStack(spacing: 14) {
            VStack(alignment: .leading, spacing: 3) {
                Text(title).font(.system(size: 12, weight: .medium))
                if let detail {
                    Text(detail).font(.system(size: 10.5)).foregroundStyle(VeilTheme.muted)
                }
            }
            Spacer(minLength: 12)
            control
        }
        .padding(.horizontal, 14).padding(.vertical, 11)
    }
}

/// 设置分区：标题、说明与卡片容器。
private struct SettingsSection<Content: View>: View {
    /// 分区标题和用途说明。
    let title: String
    /// 分区标题下面显示的业务范围说明。
    let detail: String
    /// 按顺序排列在本分区中的设置行内容。
    private let content: Content

    /// 将同一业务范围的设置行组合为带标题说明的卡片分区。
    /// - Parameters: title: String 分区标题；detail: String 范围或存储说明；content: () -> Content 设置行构建闭包，Content 符合 View。
    /// - Returns: SettingsSection 实例，保存标题、说明和本次生成的内容视图。
    /// - Throws: 不抛出异常。
    /// - 注意事项: content 初始化时求值；只提供布局和外观，不隔离各控件的异步保存任务。
    init(title: String, detail: String, @ViewBuilder content: () -> Content) {
        self.title = title
        self.detail = detail
        self.content = content()
    }

    /// 按标题、说明和卡片内容组成一个视觉分组。
    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            Text(title).font(.system(size: 13, weight: .semibold))
            Text(detail).font(.system(size: 11)).foregroundStyle(VeilTheme.muted)
            VStack(spacing: 0) { content }
                .background(VeilTheme.card, in: RoundedRectangle(cornerRadius: 10))
                .overlay(RoundedRectangle(cornerRadius: 10).stroke(VeilTheme.border))
        }
    }
}
