import AppKit
import Foundation
import UniformTypeIdentifiers

/// 按扩展名登记的默认打开应用（扩展名 → 应用 bundle identifier）。
/// 规则保存在 app.db 的 user_setting 表；未登记的扩展名使用系统默认应用。
enum OpenWithRules {
    static let maximumExtensionLength = 16

    /// 从文件名提取可登记的扩展名：小写、不含点号；没有扩展名时返回 nil。
    static func normalizeExtension(forFileName name: String) -> String? {
        normalize(extension: (name as NSString).pathExtension)
    }

    /// 归一化用户输入（允许 `MP4`、`.mp4`、` mp4 ` 等写法）。
    static func normalizeUserInput(_ raw: String) -> String? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        let stripped = String(trimmed.drop(while: { $0 == "." }))
        return normalize(extension: stripped)
    }

    /// 设置页按后缀排序展示。
    static func sortedExtensions(_ rules: [String: String]) -> [String] {
        rules.keys.sorted()
    }

    private static func normalize(extension ext: String) -> String? {
        let lower = ext.lowercased()
        guard (1...maximumExtensionLength).contains(lower.count),
            lower.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber) })
        else { return nil }
        return lower
    }
}

/// 已安装的系统应用：按 bundle identifier 解析，应用被卸载或移动后解析失败。
struct ExternalApplication {
    let bundleID: String
    let url: URL

    var name: String { Self.displayName(for: url) }
    var icon: NSImage { NSWorkspace.shared.icon(forFile: url.path) }

    static func resolve(bundleID: String) -> ExternalApplication? {
        guard !bundleID.isEmpty,
            let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID)
        else { return nil }
        return ExternalApplication(bundleID: bundleID, url: url)
    }

    static func application(at url: URL) -> ExternalApplication? {
        guard let bundle = Bundle(url: url), let bundleID = bundle.bundleIdentifier else {
            return nil
        }
        return ExternalApplication(bundleID: bundleID, url: url)
    }

    static func displayName(for url: URL) -> String {
        let display = FileManager.default.displayName(atPath: url.path)
        return display.hasSuffix(".app") ? String(display.dropLast(4)) : display
    }

    /// 应用选择面板：允许选择系统应用或第三方应用，只返回带 bundle identifier 的应用。
    @MainActor
    static func choose(title: String, message: String) -> ExternalApplication? {
        let panel = NSOpenPanel()
        panel.title = title
        panel.message = message
        panel.prompt = "选择"
        panel.allowedContentTypes = [.application]
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.directoryURL = URL(fileURLWithPath: "/System/Applications", isDirectory: true)
        guard panel.runModal() == .OK, let url = panel.url else { return nil }
        return application(at: url)
    }
}

/// 预览与外开都会在应用临时目录生成明文副本；关闭、锁定或退出时清理，启动时兜底清扫残留。
enum TemporaryFiles {
    static let previewPrefix = "veil-native-preview-"
    static let openPrefix = "veil-open-"
    /// 残留超过该时长才在启动时清理，避免误删正在使用的副本。
    static let staleAge: TimeInterval = 48 * 3600

    /// 预览与外开共用的临时根目录：`$TMPDIR/Veil`，设置页会展示并支持在访达中打开。
    static var directory: URL {
        FileManager.default.temporaryDirectory.appendingPathComponent("Veil", isDirectory: true)
    }

    /// 确保临时根目录存在且仅当前用户可访问。
    static func ensureDirectory() throws {
        let manager = FileManager.default
        var isDirectory: ObjCBool = false
        if manager.fileExists(atPath: directory.path, isDirectory: &isDirectory), isDirectory.boolValue {
            try? manager.setAttributes([.posixPermissions: 0o700], ofItemAtPath: directory.path)
            return
        }
        try manager.createDirectory(
            at: directory, withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700])
    }

    /// 启动即清空明文副本：删除 `root` 下的全部内容（Veil 专用临时目录），
    /// 并清理旧版本直接写在系统临时目录根下的 `veil-*` 残留。
    @discardableResult
    static func purge(root: URL, legacyRoot: URL? = nil, now: Date = Date()) -> Int {
        var removed = 0
        let manager = FileManager.default
        if let entries = try? manager.contentsOfDirectory(
            at: root, includingPropertiesForKeys: nil, options: [])
        {
            for url in entries where (try? manager.removeItem(at: url)) != nil {
                removed += 1
            }
        }
        if let legacyRoot {
            removed += sweep(root: legacyRoot, olderThan: 0, now: now).count
        }
        return removed
    }

    /// 清理 `root` 下过期的预览/外开临时目录，返回被清理的目录名。
    @discardableResult
    static func sweep(root: URL, olderThan age: TimeInterval = staleAge, now: Date = Date())
        -> [String]
    {
        let manager = FileManager.default
        guard
            let entries = try? manager.contentsOfDirectory(
                at: root, includingPropertiesForKeys: [.isDirectoryKey, .contentModificationDateKey],
                options: [.skipsHiddenFiles])
        else { return [] }
        let cutoff = now.addingTimeInterval(-age)
        var removed: [String] = []
        for url in entries {
            let name = url.lastPathComponent
            guard name.hasPrefix(previewPrefix) || name.hasPrefix(openPrefix) else { continue }
            let isDirectory = (try? url.resourceValues(forKeys: [.isDirectoryKey]))?.isDirectory
            guard isDirectory == true else { continue }
            let modified =
                (try? url.resourceValues(forKeys: [.contentModificationDateKey]))?
                .contentModificationDate ?? .distantPast
            guard modified < cutoff else { continue }
            guard (try? manager.removeItem(at: url)) != nil else { continue }
            removed.append(name)
        }
        return removed
    }
}

/// 交给系统应用的明文副本登记表。
///
/// 与具体应用无关：启动完成后按 `NSRunningApplication` 的进程号登记目录，
/// 该应用退出时回收对应目录；锁定容器或退出 Veil 时全部回收。
@MainActor
final class ExternalOpenTracker {
    static let shared = ExternalOpenTracker()

    /// 已创建、尚未绑定到具体进程（启动回调还没回来）的目录。
    private var pending: Set<URL> = []
    /// 进程号 → 该应用当前打开的明文副本目录。
    private var byProcess: [pid_t: Set<URL>] = [:]
    private var observer: NSObjectProtocol?

    private init() {
        observer = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didTerminateApplicationNotification,
            object: nil,
            queue: .main
        ) { notification in
            guard
                let application = notification.userInfo?[NSWorkspace.applicationUserInfoKey]
                    as? NSRunningApplication
            else { return }
            let process = application.processIdentifier
            Task { @MainActor in ExternalOpenTracker.shared.cleanup(process: process) }
        }
    }

    /// 先登记目录，等系统应用启动回调拿到进程号后再绑定。
    func register(_ folder: URL) {
        pending.insert(folder)
    }

    /// 绑定到实际打开文件的进程；应用退出时据此回收。
    func associate(folder: URL?, process: pid_t?) {
        guard let folder else { return }
        pending.remove(folder)
        guard let process else { return }
        byProcess[process, default: []].insert(folder)
    }

    /// 打开失败时立即丢弃该副本。
    func discard(_ folder: URL?) {
        guard let folder else { return }
        pending.remove(folder)
        for process in byProcess.keys where byProcess[process]?.contains(folder) == true {
            byProcess[process]?.remove(folder)
        }
        remove(folder)
    }

    /// 对应应用退出后回收它的明文副本。
    func cleanup(process: pid_t) {
        guard let folders = byProcess.removeValue(forKey: process) else { return }
        for folder in folders { remove(folder) }
    }

    /// 锁定容器、退出应用或切换容器时回收全部明文副本。
    func cleanupAll() {
        var folders = pending
        for group in byProcess.values { folders.formUnion(group) }
        pending.removeAll()
        byProcess.removeAll()
        for folder in folders { remove(folder) }
    }

    private func remove(_ folder: URL) {
        AppLog.attempt({ try FileManager.default.removeItem(at: folder) })
    }
}
