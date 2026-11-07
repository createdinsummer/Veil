import AppKit
import SwiftUI

@main
struct VeilMacApp: App {
    @StateObject private var store = AppStore()
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    var body: some Scene {
        Window("Veil", id: "main") {
            ContentView()
                .environmentObject(store)
                .frame(minWidth: 820, minHeight: 560)
                .tint(VeilTheme.accent)
                .task {
                    delegate.store = store
                    NSApp.setActivationPolicy(.regular)
                    await store.load()
                }
                .onReceive(NSWorkspace.shared.notificationCenter.publisher(for: NSWorkspace.willSleepNotification)) { _ in
                    Task { await store.lock() }
                }
                .onReceive(NSWorkspace.shared.notificationCenter.publisher(for: NSWorkspace.screensDidSleepNotification)) { _ in
                    Task { await store.lock() }
                }
                .onReceive(NSWorkspace.shared.notificationCenter.publisher(for: NSWorkspace.sessionDidResignActiveNotification)) { _ in
                    Task { await store.lock() }
                }
                .onReceive(NSWorkspace.shared.notificationCenter.publisher(for: NSWorkspace.didUnmountNotification)) { _ in
                    Task { await store.refreshSpaceAvailability() }
                }
                .onReceive(NSWorkspace.shared.notificationCenter.publisher(for: NSWorkspace.didMountNotification)) { _ in
                    Task { await store.refreshSpaceAvailability() }
                }
                .onDisappear { Task { await store.lock() } }
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 1160, height: 760)
        .windowResizability(.contentMinSize)
        .commands {
            CommandGroup(replacing: .newItem) {
                Button("新建容器…") { Task { await store.newContainer() } }
                    .keyboardShortcut("n").disabled(store.isBusy || store.activeSpace?.isAvailable != true)
                Button("打开容器…") { Task { await store.chooseContainerDirectory() } }
                    .keyboardShortcut("o").disabled(store.isBusy)
                Button("导入加密容器…") { Task { await store.choosePackage() } }
                    .keyboardShortcut("o", modifiers: [.command, .shift]).disabled(store.isBusy || store.activeSpace?.isAvailable != true)
                Divider()
                Button("新建空间…") { store.error = nil; store.showNewSpace = true }
                    .disabled(store.isBusy)
            }
            CommandMenu("容器") {
                Button("添加文件…") { Task { await store.chooseImport(directory: false) } }
                    .keyboardShortcut("i").disabled(store.isBusy || store.container == nil)
                Button("添加文件夹…") { Task { await store.chooseImport(directory: true) } }
                    .disabled(store.isBusy || store.container == nil)
                Divider()
                Button("导出加密备份…") { Task { await store.exportPackage() } }
                    .keyboardShortcut("e", modifiers: [.command, .shift]).disabled(store.isBusy || store.container == nil)
                Button("刷新") { Task { await store.refresh() } }
                    .keyboardShortcut("r").disabled(store.isBusy)
                Button("锁定容器") { Task { await store.lock(forgetPassword: true) } }
                    .keyboardShortcut("l", modifiers: [.command, .shift]).disabled(store.container == nil && !store.isBusy)
                Divider()
                Menu("免密时长") {
                    ForEach([0, 900, 1800, 3600, 7200, 14400], id: \.self) { seconds in
                        Button(store.unlockCacheSeconds == seconds
                               ? "\(AppStore.durationLabel(seconds))（当前）"
                               : AppStore.durationLabel(seconds)) {
                            Task { await store.setUnlockCache(seconds: seconds) }
                        }
                    }
                }.disabled(store.isBusy)
                Menu("导入提交间隔") {
                    ForEach(1...60, id: \.self) { seconds in
                        Button("\(seconds) 秒" + (store.importCommitIntervalSeconds == seconds ? "（当前）" : "")) {
                            Task { await store.setImportCommitInterval(seconds: seconds) }
                        }
                    }
                }.disabled(store.isBusy)
                Menu("导入提交数量") {
                    ForEach(0..<10, id: \.self) { group in
                        Menu("\(group * 10 + 1)～\(group * 10 + 10) 条") {
                            ForEach((group * 10 + 1)...(group * 10 + 10), id: \.self) { count in
                                Button("\(count) 条" + (store.importCommitCount == count ? "（当前）" : "")) {
                                    Task { await store.setImportCommitCount(count) }
                                }
                            }
                        }
                    }
                }.disabled(store.isBusy)
            }
        }
        Window("任务中心", id: VeilWindowID.taskCenter) {
            TaskCenterView()
                .environmentObject(store)
                .tint(VeilTheme.accent)
        }
        .defaultSize(width: 560, height: 360)
        .windowResizability(.contentSize)
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    weak var store: AppStore?

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard let store else { AppLog.flush(); return .terminateNow }
        Task {
            await store.lock()
            AppLog.flush()
            sender.reply(toApplicationShouldTerminate: true)
        }
        return .terminateLater
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if !flag { sender.windows.first(where: { $0.canBecomeMain })?.makeKeyAndOrderFront(nil) }
        return true
    }
}
