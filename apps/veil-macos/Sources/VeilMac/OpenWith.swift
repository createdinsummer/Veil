/// 外部应用关联和临时明文文件生命周期管理。
/// 所属模块：macOS 预览 / 设置功能；按扩展名选择打开应用，并跟踪交给其他应用的临时解密副本。
/// 核心类型：OpenWithRules 规范化扩展名；ExternalApplication 表示可启动应用；TemporaryFiles 清扫预览目录；ExternalOpenTracker 依据进程退出回收副本。
/// 依赖 AppKit、Foundation、UniformTypeIdentifiers、系统工作区和应用设置存储。整体目标是让用户选择的打开规则稳定可保存，并尽量缩短明文副本在磁盘上的留存时间。
import AppKit
import Foundation
import UniformTypeIdentifiers

/// 按扩展名登记的默认打开应用（扩展名 → 应用 bundle identifier）。
/// 规则保存在 app.db 的 user_setting 表；未登记的扩展名使用系统默认应用。
enum OpenWithRules {
    /// 扩展名允许的最大字符数，限制异常或误粘贴输入。
    static let maximumExtensionLength = 16

    /// 根据文件后缀查找可保存的默认打开规则键。
    /// - Parameters: name: String 文件名或路径文本；使用 NSString 提取最后一个扩展名。
    /// - Returns: String?；合法时为小写 ASCII 字母数字后缀，无后缀或不合法时为 nil。
    /// - Throws: 不向调用方抛出异常；失败按返回值或注意事项处理。
    /// - 注意事项: 仅处理后缀，不检查文件是否存在。
    static func normalizeExtension(forFileName name: String) -> String? {
        normalize(extension: (name as NSString).pathExtension)
    }

    /// 把设置页输入转换为统一的扩展名规则键。
    /// - Parameters: raw: String 用户输入；允许首尾空白和一个或多个前导点。
    /// - Returns: String?；清理后通过长度和字符校验时返回后缀，否则 nil。
    /// - Throws: 不向调用方抛出异常；失败按返回值或注意事项处理。
    /// - 注意事项: 不会删除后缀内部的点或空白。
    static func normalizeUserInput(_ raw: String) -> String? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        let stripped = String(trimmed.drop(while: { $0 == "." }))
        return normalize(extension: stripped)
    }

    /// 为设置页提供稳定的后缀展示顺序。
    /// - Parameters: rules: [String: String]，键是后缀，值是应用 bundle ID；只读取键。
    /// - Returns: [String]，按字符串默认比较顺序排序的后缀列表；空映射返回空数组。
    /// - Throws: 不向调用方抛出异常；失败按返回值或注意事项处理。
    /// - 注意事项: 不验证已有键或应用安装状态。
    static func sortedExtensions(_ rules: [String: String]) -> [String] {
        rules.keys.sorted()
    }

    /// 限制规则键格式，避免异常输入成为应用关联配置。
    /// - Parameters: ext: String 待校验后缀；本方法不会去除点和空白。
    /// - Returns: String?；转小写后长度为 1 至 16 且每个字符为 ASCII 字母或数字时返回，否则 nil。
    /// - Throws: 不向调用方抛出异常；失败按返回值或注意事项处理。
    /// - 注意事项: 纯字符串计算，可独立调用，不依赖文件系统。
    private static func normalize(extension ext: String) -> String? {
        let lower = ext.lowercased()
        guard (1...maximumExtensionLength).contains(lower.count),
            lower.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber) })
        else { return nil }
        return lower
    }
}

/// 外部打开应用的定位信息：保存系统标识和当前应用包 URL，供菜单展示和启动使用。
/// 记录本身不跟踪安装变化；后续解析可能因卸载失败，已保存 URL 也可能因移动而失效。
struct ExternalApplication {
    /// 系统用于唯一标识应用的 bundle identifier。
    let bundleID: String
    /// 当前应用包的位置；应用移动或卸载后可能不再有效。
    let url: URL

    /// 去掉 .app 后缀的用户可读应用名。
    var name: String { Self.displayName(for: url) }
    /// 从系统工作区取得应用图标。
    var icon: NSImage { NSWorkspace.shared.icon(forFile: url.path) }

    /// 按系统应用标识定位可用于外部打开的应用。
    /// - Parameters: bundleID: String 应用包标识；空字符串无效。
    /// - Returns: ExternalApplication?；系统能定位时包含标识和 URL，否则 nil。
    /// - Throws: 不向调用方抛出异常；失败按返回值或注意事项处理。
    /// - 注意事项: 查询 NSWorkspace，结果只是当前定位状态，不保证稍后启动成功。
    static func resolve(bundleID: String) -> ExternalApplication? {
        guard !bundleID.isEmpty,
            let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID)
        else { return nil }
        return ExternalApplication(bundleID: bundleID, url: url)
    }

    /// 从用户选择的应用包读取可保存的系统标识。
    /// - Parameters: url: URL 候选应用包位置。
    /// - Returns: ExternalApplication?；Bundle 能解析且有 bundleIdentifier 时返回记录，否则 nil。
    /// - Throws: 不向调用方抛出异常；失败按返回值或注意事项处理。
    /// - 注意事项: 此方法没有额外验证 .app 后缀或应用能否启动。
    static func application(at url: URL) -> ExternalApplication? {
        guard let bundle = Bundle(url: url), let bundleID = bundle.bundleIdentifier else {
            return nil
        }
        return ExternalApplication(bundleID: bundleID, url: url)
    }

    /// 生成菜单中展示的应用名称。
    /// - Parameters: url: URL 应用包位置。
    /// - Returns: String，文件管理器提供的显示名，末尾为 .app 时移除该后缀。
    /// - Throws: 不向调用方抛出异常；失败按返回值或注意事项处理。
    /// - 注意事项: 名称来自文件系统显示规则，不用于唯一标识应用。
    static func displayName(for url: URL) -> String {
        let display = FileManager.default.displayName(atPath: url.path)
        return display.hasSuffix(".app") ? String(display.dropLast(4)) : display
    }

    @MainActor
    /// 让用户为文件选择外部打开应用。
    /// - Parameters: title: String 面板标题；message: String 面板说明。
    /// - Returns: ExternalApplication?；用户确认且选项可解析为应用记录时返回，取消或解析失败返回 nil。
    /// - Throws: 不向调用方抛出异常；失败按返回值或注意事项处理。
    /// - 注意事项: 必须在主 Actor 调用；模态面板会等待用户选择，默认浏览系统 Applications 目录。
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
    /// 临时预览目录名固定前缀，用于启动时识别和清理。
    static let previewPrefix = "veil-native-preview-"
    /// 外部应用打开副本目录名固定前缀。
    static let openPrefix = "veil-open-"
    /// sweep 默认保留 48 小时内的旧版目录；purge 清理旧版目录时显式传入零，不使用此门槛。
    static let staleAge: TimeInterval = 48 * 3600

    /// 预览与外开共用的临时根目录：`$TMPDIR/Veil`，设置页会展示并支持在访达中打开。
    /// 返回应用专用临时目录 URL，不负责创建目录。
    static var directory: URL {
        FileManager.default.temporaryDirectory.appendingPathComponent("Veil", isDirectory: true)
    }

    /// 为预览和外部打开准备应用专用临时根目录。
    /// - Parameters: 无。
    /// - Returns: Void；目录存在或创建成功后结束。
    /// - Throws: 无法创建目录时抛出 Foundation 文件系统错误；已有目录权限更新错误被忽略。
    /// - 注意事项: 依赖可写的系统临时目录；已有目录权限修改失败被忽略，因此成功返回不保证权限一定为 0700。
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

    @discardableResult
    /// 清理启动时遗留的明文副本，减少上次运行退出异常造成的残留。
    /// - Parameters: root: URL 专用目录，删除其中所有可枚举项；legacyRoot: URL? 旧版根目录，nil 跳过；now: Date 旧版目录年龄比较基准，默认当前时间。
    /// - Returns: Int，实际删除成功的顶层条目数，子目录内部文件不逐个计数。
    /// - Throws: 不向调用方抛出异常；失败按返回值或注意事项处理。
    /// - 注意事项: 调用方必须传入专用目录；不检查活动进程，可能删除仍被外部应用使用的副本；目录不可读和删除失败都忽略。
    static func purge(root: URL, legacyRoot: URL? = nil, now: Date = Date()) -> Int {
        var removed = 0
        let manager = FileManager.default
        if let entries = try? manager.contentsOfDirectory(
            at: root, includingPropertiesForKeys: nil, options: [])
        {
            // 每轮删除专用目录中的一项明文或残留子目录，只统计实际删除成功的条目。
            for url in entries where (try? manager.removeItem(at: url)) != nil {
                removed += 1
            }
        }
        if let legacyRoot {
            removed += sweep(root: legacyRoot, olderThan: 0, now: now).count
        }
        return removed
    }

    @discardableResult
    /// 按命名和修改时间回收旧版临时根目录中的副本。
    /// - Parameters: root: URL 扫描目录；age: TimeInterval 保留秒数，默认 48 小时，未限制负值；now: Date 比较基准。
    /// - Returns: [String]，成功删除的目录名；无法枚举时为空；隐藏项、普通文件、非指定前缀项和未过期项保留。
    /// - Throws: 不向调用方抛出异常；失败按返回值或注意事项处理。
    /// - 注意事项: 不检查使用中的进程；修改时间读取失败视为很久以前，可能进入删除流程。
    static func sweep(root: URL, olderThan age: TimeInterval = staleAge, now: Date = Date())
        -> [String]
    {
        let manager = FileManager.default
        guard
            let entries = try? manager.contentsOfDirectory(
                at: root, includingPropertiesForKeys: [.isDirectoryKey, .contentModificationDateKey],
                options: [.skipsHiddenFiles])
        else { return [] }
        // 修改时间必须严格早于截止时间；刚好等于截止时间的目录保留。
        let cutoff = now.addingTimeInterval(-age)
        // 保存成功删除的目录名，供调用方统计；不是所有候选目录的名单。
        var removed: [String] = []
        // 逐项只检查 Veil 自己创建的目录，再按目录属性和修改时间判断是否已经过期。
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
/// 该应用退出时尝试删除对应目录；锁定容器或退出 Veil 时尝试清理全部登记。
/// 所有登记操作在主 Actor 上串行执行；删除失败只记日志，不保留重试登记。
@MainActor
final class ExternalOpenTracker {
    /// 全应用共用的主线程追踪器。
    static let shared = ExternalOpenTracker()

    /// 已创建、尚未绑定到具体进程（启动回调还没回来）的目录。
    private var pending: Set<URL> = []
    /// 外部应用进程号到其明文副本目录集合的映射；同一进程可打开多份文件。
    private var byProcess: [pid_t: Set<URL>] = [:]
    /// Workspace 应用退出通知观察者，持有期间负责接收系统终止事件。
    private var observer: NSObjectProtocol?

    /// 为全局副本登记表订阅外部应用退出通知。
    /// - Parameters: 无。
    /// - Returns: 新的 ExternalOpenTracker 实例，保存通知观察者以持续接收事件。
    /// - Throws: 不向调用方抛出异常；失败按返回值或注意事项处理。
    /// - 注意事项: 主 Actor 初始化；通知仅提取进程号，再创建主 Actor 任务清理，收到通知不代表已同步完成删除。
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

    /// 在应用启动回调返回前保留明文目录的清理责任。
    /// - Parameters: folder: URL 刚生成的临时副本目录。
    /// - Returns: Void；将目录放入 pending 集合，同一 URL 重复登记合并。
    /// - Throws: 不向调用方抛出异常；失败按返回值或注意事项处理。
    /// - 注意事项: 主 Actor 调用；不创建目录，也不验证 URL 是否属于 Veil 临时根目录。
    func register(_ folder: URL) {
        pending.insert(folder)
    }

    /// 把已导出的明文目录交由外部应用进程生命周期管理。
    /// - Parameters: folder: URL? 待关联目录，nil 时无操作；process: pid_t? 外部应用进程号。
    /// - Returns: Void；先移除待定登记，有进程号时加入该进程的目录集合。
    /// - Throws: 不向调用方抛出异常；失败按返回值或注意事项处理。
    /// - 注意事项: 主 Actor 调用；process 为 nil 时目录会失去待定登记且不会被本方法删除，调用方需处理此情形。
    func associate(folder: URL?, process: pid_t?) {
        guard let folder else { return }
        pending.remove(folder)
        guard let process else { return }
        byProcess[process, default: []].insert(folder)
    }

    /// 外部打开失败时解除该目录登记并尝试删除明文副本。
    /// - Parameters: folder: URL? 要丢弃的目录；nil 时无操作。
    /// - Returns: Void；移除 pending 和所有进程集合中的该目录，然后尝试删除。
    /// - Throws: 不向调用方抛出异常；失败按返回值或注意事项处理。
    /// - 注意事项: 主 Actor 调用；删除失败只记日志，目录不再保留在登记表中等待重试。
    func discard(_ folder: URL?) {
        guard let folder else { return }
        pending.remove(folder)
        // 目录可能已经被关联到进程；遍历映射清除所有包含它的登记。
        for process in byProcess.keys where byProcess[process]?.contains(folder) == true {
            byProcess[process]?.remove(folder)
        }
        remove(folder)
    }

    /// 在某个外部应用退出后回收该进程打开的全部副本。
    /// - Parameters: process: pid_t 已退出应用的系统进程号。
    /// - Returns: Void；没有登记时无操作，有登记时先移除整组再逐个尝试删除目录。
    /// - Throws: 不向调用方抛出异常；失败按返回值或注意事项处理。
    /// - 注意事项: 主 Actor 调用；同一应用进程的多个文件一起清理，不以单个窗口关闭为触发条件。
    func cleanup(process: pid_t) {
        guard let folders = byProcess.removeValue(forKey: process) else { return }
        // 逐个删除该进程打开的文件副本目录。
        for folder in folders { remove(folder) }
    }

    /// 锁定或退出时回收全部已登记的外部打开副本。
    /// - Parameters: 无。
    /// - Returns: Void；合并 pending 和各进程目录、清空登记，再逐项尝试删除。
    /// - Throws: 不向调用方抛出异常；失败按返回值或注意事项处理。
    /// - 注意事项: 主 Actor 调用；不等待外部应用退出，Set 按 URL 去重，删除失败不会恢复登记。
    func cleanupAll() {
        // 汇总尚未绑定和已绑定进程的目录；每个 URL 对应一份导出文件所在目录。
        var folders = pending
        // 合并各进程目录集合，Set 确保同一目录不会被重复删除。
        for group in byProcess.values { folders.formUnion(group) }
        pending.removeAll()
        byProcess.removeAll()
        for folder in folders { remove(folder) }
    }

    /// 执行一个已登记明文目录的尽力清理。
    /// - Parameters: folder: URL 要删除的目录及其内容。
    /// - Returns: Void；删除成功或错误记录完成后结束。
    /// - Throws: 不向调用方抛出异常；文件系统删除错误由 AppLog 记录。
    /// - 注意事项: 不验证路径边界；只应传入调用方创建的临时目录，不能保证已被外部程序复制的数据也被清除。
    private func remove(_ folder: URL) {
        AppLog.attempt({ try FileManager.default.removeItem(at: folder) })
    }
}
