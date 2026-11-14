import AppKit
import SwiftUI

/// 独立设置窗口：用户配置与系统配置分区，变更即时写入 app.db，与 CLI 共用设置项。
struct SettingsView: View {
    @EnvironmentObject private var store: AppStore

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
        .task(id: store.settingsFeedback) {
            guard let feedback = store.settingsFeedback else { return }
            do { try await Task.sleep(for: .seconds(3)) } catch { return }
            if store.settingsFeedback == feedback { store.settingsFeedback = nil }
        }
        .onChange(of: store.layout) { _, _ in Task { await store.savePreferences() } }
        .onChange(of: store.browseMode) { _, _ in Task { await store.savePreferences() } }
    }

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

    private var rowDivider: some View {
        Rectangle().fill(VeilTheme.border).frame(height: 1).padding(.leading, 14)
    }

    // MARK: 用户配置

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

    private var layoutRow: some View {
        SettingsRow("布局", detail: "文件以网格或列表展示。") {
            Picker("布局", selection: $store.layout) {
                Text("网格").tag(FileLayout.grid)
                Text("列表").tag(FileLayout.list)
            }
            .labelsHidden().pickerStyle(.segmented).fixedSize()
        }
    }

    private var browseModeRow: some View {
        SettingsRow("浏览方式", detail: "按类型查看，或按目录层级浏览。") {
            Picker("浏览方式", selection: $store.browseMode) {
                Text("按类型").tag(BrowseMode.type)
                Text("按路径").tag(BrowseMode.path)
            }
            .labelsHidden().pickerStyle(.segmented).fixedSize()
        }
    }

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
                ForEach(Array(store.openWithExtensions.enumerated()), id: \.element) { index, ext in
                    if index > 0 {
                        Rectangle().fill(VeilTheme.border).frame(height: 1).padding(.leading, 14)
                    }
                    openWithRow(ext)
                }
            }
        }
    }

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

    /// 临时副本目录：展示路径并支持在访达中打开，说明清理时机。
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

    private var deviceRow: some View {
        SettingsRow("设备 ID", detail: "本机设备身份，用于识别设备。") {
            HStack(spacing: 6) {
                Text(store.deviceID.isEmpty ? "—" : store.deviceID)
                    .font(.system(size: 11, design: .monospaced)).foregroundStyle(VeilTheme.secondary)
                    .lineLimit(1).truncationMode(.middle).frame(maxWidth: 200, alignment: .trailing)
                Button {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(store.deviceID, forType: .string)
                    store.settingsFeedback = "设备 ID 已拷贝"
                } label: { Image(systemName: "doc.on.doc") }
                    .buttonStyle(VeilIconButtonStyle()).disabled(store.deviceID.isEmpty)
                    .help("拷贝设备 ID").accessibilityLabel("拷贝设备 ID")
            }
        }
    }

    private var defaultSpaceRow: some View {
        SettingsRow("默认空间", detail: "新容器默认存放的空间。") {
            Picker("默认空间", selection: defaultSpaceBinding) {
                ForEach(store.spaces) { space in
                    Text("\(space.name) · \(space.path)").tag(space.id)
                }
            }
            .labelsHidden().pickerStyle(.menu).frame(maxWidth: 230)
            .disabled(store.spaces.isEmpty)
        }
    }

    private var defaultSpaceBinding: Binding<String> {
        Binding(
            get: { store.spaces.first(where: \.isDefault)?.id ?? "" },
            set: { id in Task { await store.setDefaultSpace(id) } }
        )
    }
}

/// 设置行：左侧标题与说明，右侧控件。
private struct SettingsRow<Control: View>: View {
    let title: String
    let detail: String?
    private let control: Control

    init(_ title: String, detail: String? = nil, @ViewBuilder control: () -> Control) {
        self.title = title
        self.detail = detail
        self.control = control()
    }

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
    let title: String
    let detail: String
    private let content: Content

    init(title: String, detail: String, @ViewBuilder content: () -> Content) {
        self.title = title
        self.detail = detail
        self.content = content()
    }

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
