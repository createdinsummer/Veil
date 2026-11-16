/// ============================================================================
/// Veil macOS 客户端 · 应用入口（VeilMacApp）
///
/// 文件作用：SwiftUI 应用的总入口，声明主窗口、设置窗口与任务中心窗口，
///   注册菜单命令，并负责监听系统休眠、会话失活（锁屏/切换用户）、磁盘挂载等
///   系统事件，把「锁定容器」「刷新空间可用性」等安全动作接到对应时机上。
/// 所属模块：客户端应用层（窗口与生命周期编排）。
/// 文件内核心类型清单：
///   - VeilMacApp  应用主体（@main），装配窗口、菜单与系统事件订阅
///   - AppDelegate 应用生命周期代理，处理退出与窗口重新打开
/// 依赖能力：SwiftUI（窗口/场景）、AppKit（NSWorkspace 事件通知）、
///   AppStore（全局状态中枢）、VeilTheme（视觉参数）。
/// 整体业务目标：把「系统休眠、锁屏、退出即锁定容器」的安全边界落到
///   应用生命周期上，并让用户通过菜单/快捷键完成新建、打开、导入容器等操作。
/// ============================================================================
import AppKit
import SwiftUI

/// 应用入口：装配全局状态、窗口场景、菜单命令与系统事件监听。
/// @main 标记让 SwiftUI 用它作为进程入口，不再需要 main.swift。
@main
struct VeilMacApp: App {
    /// 全局状态中枢：空间/容器/浏览/任务等全部界面数据都集中在这里。
    @StateObject private var store = AppStore()
    /// 应用生命周期代理：负责「退出前先锁定容器」等收尾逻辑。
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    var body: some Scene {
        // 主窗口：容器创建/解锁/浏览/预览等一切主要操作都在这里发生。
        Window("Veil", id: "main") {
            ContentView()
                .environmentObject(store)
                .frame(minWidth: 820, minHeight: 560)
                .tint(VeilTheme.accent)
                .task {
                    // 首次出现时把状态中枢交给代理，并恢复注册策略为普通前台应用。
                    delegate.store = store
                    NSApp.setActivationPolicy(.regular)
                    await store.load()
                }
                // 系统即将休眠时提交异步锁定；通知回调不等待完成，默认锁定也不会主动清除免密缓存。
                .onReceive(NSWorkspace.shared.notificationCenter.publisher(for: NSWorkspace.willSleepNotification)) { _ in
                    Task { await store.lock() }
                }
                // 用户会话失活时提交异步锁定，关闭当前浏览状态；不等同于所有密码副本已从内存清零。
                .onReceive(NSWorkspace.shared.notificationCenter.publisher(for: NSWorkspace.sessionDidResignActiveNotification)) { _ in
                    Task { await store.lock() }
                }
                // 磁盘被卸载：空间可能因此不可用，刷新可用性标记。
                .onReceive(NSWorkspace.shared.notificationCenter.publisher(for: NSWorkspace.didUnmountNotification)) { _ in
                    Task { await store.refreshSpaceAvailability() }
                }
                // 磁盘重新挂载：外置空间可能恢复可用，刷新可用性标记。
                .onReceive(NSWorkspace.shared.notificationCenter.publisher(for: NSWorkspace.didMountNotification)) { _ in
                    Task { await store.refreshSpaceAvailability() }
                }
                // 主窗口消失时提交锁定以清理当前浏览和预览状态；异步完成前回调已返回。
                .onDisappear { Task { await store.lock() } }
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 1160, height: 760)
        .windowResizability(.contentMinSize)
        .commands {
            // 替换系统默认「新建」命令组，放入 Veil 自己的新建/打开/导入入口。
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
            // 「容器」菜单：文件/文件夹导入、备份导出、刷新与主动锁定。
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
            }
        }
        // 独立设置窗口（⌘,）：调整提示级别、布局、浏览方式、免密时长等。
        Settings {
            SettingsView()
                .environmentObject(store)
                .tint(VeilTheme.accent)
        }
        // 任务中心窗口：展示导入/导出进度与最近一次失败详情。
        Window("任务中心", id: VeilWindowID.taskCenter) {
            TaskCenterView()
                .environmentObject(store)
                .tint(VeilTheme.accent)
        }
        .defaultSize(width: 560, height: 360)
        .windowResizability(.contentSize)
    }
}

/// 退出与 Dock 重开窗口的生命周期代理；弱引用共享 Store，不保存文件或密码。
/// 退出前等待锁定流程并请求刷新日志，但不根据锁定或日志的实际成功状态否决退出。
/// @MainActor（保证在 UI 主线程执行的隔离注解）：代理回调全部发生在主线程。
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    /// 对全局状态中枢的弱引用，避免代理与 App 互相持有形成循环引用。
    weak var store: AppStore?

    /// 在应用退出前等待容器锁定流程，减少明文预览和外部副本残留。
    /// - Parameters: sender: NSApplication 发起退出请求的应用实例，用于稍后回复系统。
    /// - Returns: NSApplication.TerminateReply；没有 Store 时刷新日志后返回 .terminateNow，否则返回 .terminateLater，并在任务结束后回复允许退出。
    /// - Throws: 不向系统抛出异常；Store 自行处理锁定错误，日志刷新接口不返回失败状态。
    /// - 注意事项: 主 Actor 调用；等待流程结束不保证资源清理全部成功，默认 lock 不主动忘记免密缓存。
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        // 状态中枢尚未就绪时仅请求刷新日志并立即允许退出。
        guard let store else { AppLog.flush(); return .terminateNow }
        Task {
            await store.lock()
            AppLog.flush()
            sender.reply(toApplicationShouldTerminate: true)
        }
        // 返回「稍后终止」：等上面的锁定与日志任务完成后应用才会真正退出。
        return .terminateLater
    }

    /// 用户点击 Dock 图标时尝试恢复可成为主窗口的窗口。
    /// - Parameters: sender: NSApplication 当前应用；flag: Bool 当前是否已有可见窗口，false 时才查找可恢复窗口。
    /// - Returns: Bool，始终为 true，让系统继续处理重开行为。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 主 Actor 调用；找不到 canBecomeMain 的现有窗口时不创建新窗口，也不保证被选中的是主业务窗口。
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if !flag { sender.windows.first(where: { $0.canBecomeMain })?.makeKeyAndOrderFront(nil) }
        return true
    }
}
