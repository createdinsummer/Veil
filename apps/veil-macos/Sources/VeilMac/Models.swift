/// ============================================================================
/// Veil macOS 客户端 · 数据模型与浏览计算（Models.swift）
///
/// 文件作用：定义客户端与服务层 JSON 一一对应的数据模型（空间、容器、文件、
///   目录、冲突等），并集中放置浏览视图的纯计算函数（目录展开、隐藏文件规则、
///   面包屑等），这些计算只影响界面展示，不改动业务数据。
/// 所属模块：客户端模型层与浏览逻辑。
/// 文件内核心类型/函数清单：
///   - Space / ContainerSummary / RecentContainer   空间与容器登记信息
///   - ContainerSnapshot / FileEntry / DirectoryEntry 解锁后的容器视图
///   - FileCategory / BrowseMode / FileLayout / HintLevel  浏览与展示枚举
///   - BrowserEntry / Breadcrumb                   界面用条目与面包屑
///   - folderNameForCreation / pathContainsDotComponent   名称与隐藏规则校验
///   - browserEntries / directoryBreadcrumbs       浏览计算函数
///   - ContainerForm                               创建/解锁/导入三种表单状态
/// 依赖能力：Foundation（URL、日期格式化等）。
/// 整体业务目标：让服务层返回的 JSON 直接解码成 Swift 类型，并把「按类型 /
///   按路径浏览、搜索、分类、隐藏文件」等展示规则收敛为可单测的纯函数。
/// ============================================================================
import Foundation

/// 空间：存放多个加密容器的分组单元，默认空间位于应用数据目录，
/// 用户也可以为空间选择外置磁盘目录（isAvailable 表示该目录当前是否可访问）。
struct Space: Decodable, Identifiable, Sendable {
    /// 空间唯一标识（服务层生成并登记在 app.db）。
    let id: String
    /// 空间展示名称。
    let name: String
    /// 空间所在磁盘目录的绝对路径。
    let path: String
    /// 是否为默认空间：新容器默认建在这里，客户端启动时作为首选。
    let isDefault: Bool
    /// 登记路径当前is_dir观察值；不验证可写权限、容器文件完整性或未来可用性。
    let isAvailable: Bool
    /// 空间内的容器登记列表；每个元素是容器的一条登记信息。
    let containers: [ContainerSummary]
}

/// 容器登记信息：未解锁时的轻量描述，供空间侧栏与首页卡片展示。
struct ContainerSummary: Decodable, Identifiable, Hashable, Sendable {
    /// 容器唯一标识（veil_id），与展示名称、路径变化无关。
    let id: String
    /// 容器展示名称。
    let name: String
    /// 容器目录的绝对路径（其中包含 veil.db 与密文文件）。
    let path: String
}

/// 本机recent_div展示缓存，不保证目前仍有登记、在线或路径未变化。
struct RecentContainer: Decodable, Identifiable, Sendable {
    /// 容器唯一标识。
    let id: String
    /// 容器展示名称。
    let name: String
    /// 容器目录的绝对路径。
    let path: String
}

/// 解锁后的容器完整视图：文件与目录树的服务层快照，浏览界面据此渲染。
struct ContainerSnapshot: Decodable, Sendable {
    /// 容器唯一标识。
    let id: String
    /// 容器展示名称。
    let name: String
    /// 容器目录的绝对路径。
    let path: String
    /// 容器创建时间（ISO 8601 字符串）。
    let createdAt: String
    /// 容器内全部文件；每个元素是一条文件记录（含分类与版本）。
    let files: [FileEntry]
    /// 容器内全部目录；每个元素是一条目录记录（用于保持空目录）。
    let directories: [DirectoryEntry]
}

/// 目录记录：即使目录为空也会登记，保证按路径浏览时能看到空文件夹。
struct DirectoryEntry: Decodable, Sendable {
    /// 目录唯一标识。
    let id: String
    /// 目录在容器内的路径（以 / 分隔）。
    let path: String
}

/// 文件分类：服务层按扩展名归类，用于「按类型」浏览与分类筛选。
enum FileCategory: String, Decodable, CaseIterable, Identifiable, Sendable {
    case photo, video, document, archive, other
    /// 列表唯一标识，直接用原始值。
    var id: String { rawValue }
    /// 分类的界面文案。
    var title: String {
        switch self {
        case .photo: "照片"
        case .video: "视频"
        case .document: "文档"
        case .archive: "压缩包"
        case .other: "其他"
        }
    }
    /// 分类对应的 SF Symbols 图标名。
    var symbol: String {
        switch self {
        case .photo: "photo"
        case .video: "film"
        case .document: "doc.text"
        case .archive: "archivebox"
        case .other: "doc"
        }
    }
}

/// 文件记录：容器内一条文件的元数据（不包含内容，内容按需解密读取）。
struct FileEntry: Decodable, Identifiable, Hashable, Sendable {
    /// 文件唯一标识。
    let id: String
    /// 文件在容器内的完整路径（含父目录，以 / 分隔）。
    let name: String
    /// 文件大小（字节）。
    let size: UInt64
    /// 内容版本链长度，内容变化才追加；同内容重写、改名或重加密不会自动加1。
    let version: Int
    /// 文件加密入库时间（ISO 8601 字符串）。
    let encryptedAt: String
    /// 服务层给出的分类。
    let category: FileCategory
    /// 可预览的 MIME 类型；nil 表示不支持内置预览（如未知格式）。
    let previewMime: String?

    /// 文件名（去掉父目录部分）。
    var leafName: String { name.components(separatedBy: "/").last ?? name }
    /// 父目录名；根目录下的文件显示「根目录」。
    var parentName: String {
        guard let slash = name.lastIndex(of: "/") else { return "根目录" }
        return String(name[..<slash])
    }
    /// 人类可读的大小文案（如「1.2 MB」）。
    var sizeLabel: String { ByteCountFormatter.string(fromByteCount: Int64(clamping: size), countStyle: .file) }
    /// 加密日期文案：优先按带毫秒的 ISO 8601 解析，失败再按秒级格式尝试。
    var dateLabel: String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        var date = formatter.date(from: encryptedAt)
        if date == nil {
            formatter.formatOptions = [.withInternetDateTime]
            date = formatter.date(from: encryptedAt)
        }
        return date?.formatted(date: .abbreviated, time: .omitted) ?? encryptedAt
    }
}

/// 为新建目录表单取得合法的单级名称。
/// 入参：rawName（String）为用户输入。
/// 返回值：String?，先trim空白/换行，非空且非点/双点、不含斜杠/反斜杠/NUL时返回名称，否则nil。
/// 异常：不抛错。
/// 注意事项：不校验长度、其他控制字符、同名或父目录状态；最终创建仍由Rust检查。
func folderNameForCreation(_ rawName: String) -> String? {
    let name = rawName.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !name.isEmpty, name != ".", name != "..",
          !name.contains("/"), !name.contains("\\"), !name.contains("\0")
    else { return nil }
    return name
}

/// 浏览方式：按类型（扁平分类）或按路径（目录层级）。
enum BrowseMode: String, CaseIterable, Sendable { case type, path }
/// 文件布局：网格（大图标）或列表。
enum FileLayout: String, Sendable { case grid, list }

/// 命令行提示的详细程度，与 CLI 共用 app.db 的 hint_level 设置项。
enum HintLevel: String, CaseIterable, Sendable {
    case full, brief, off
    /// 选项名称。
    var title: String {
        switch self {
        case .full: "完整"
        case .brief: "简洁"
        case .off: "关闭"
        }
    }
    /// 选项说明。
    var detail: String {
        switch self {
        case .full: "显示全部操作提示。"
        case .brief: "只显示关键提示。"
        case .off: "不显示提示。"
        }
    }
}

/// 浏览条目：文件夹（携带路径）或文件（携带完整记录），浏览视图统一遍历它。
enum BrowserEntry: Identifiable, Hashable, Sendable {
    case folder(String)
    case file(FileEntry)
    /// 列表唯一标识：文件夹用路径加前缀，文件用文件 id。
    var id: String {
        switch self { case .folder(let path): "folder:\(path)"; case .file(let file): file.id }
    }
    /// 条目的容器内路径。
    var path: String {
        switch self { case .folder(let path): path; case .file(let file): file.name }
    }
    /// 条目显示名（路径最后一段）。
    var name: String { path.components(separatedBy: "/").last ?? path }
    /// 条目图标：文件夹图标或文件分类图标。
    var symbol: String {
        switch self { case .folder: "folder.fill"; case .file(let file): file.category.symbol }
    }
}

/// 判断完整路径是否位于点文件或点目录下，供类型模式隐藏。
/// 入参：path（String）为内部逻辑路径。
/// 返回值：Bool，任一非空斜杠组件以“.”起始为true。
/// 异常：不抛错。
/// 注意事项：不规范化，不查询系统隐藏属性；“..”也会匹配，合法性另由Rust保证。
func pathContainsDotComponent(_ path: String) -> Bool {
    path.split(separator: "/").contains { $0.hasPrefix(".") }
}

/// 根据本地浏览条件从容器快照计算当前可见文件和目录项。
/// 入参：container（ContainerSnapshot）含完整文件/目录列表；mode为type/path；directory（String）为规范化内部路径，根为空；categories（Set<FileCategory>）空为全部；search为本地化包含搜索词；showHidden（Bool）默认false控制点组件可见性。
/// 返回值：[BrowserEntry]：类型模式只含筛选文件且保留输入顺序；路径模式聚合直接子目录去重并按自然顺序目录在前、文件在后，最后按完整路径搜索。
/// 异常：不抛错，不访问服务。
/// 注意事项：分类筛选仅类型模式生效；搜索不是递归查库。类型模式按任意点组件隐藏，路径模式只判断当前层子名，已进入隐藏父目录时不再整体过滤。
func browserEntries(container: ContainerSnapshot, mode: BrowseMode, directory: String,
                    categories: Set<FileCategory>, search: String,
                    showHidden: Bool = false) -> [BrowserEntry] {
    var result: [BrowserEntry]
    if mode == .type {
        // 按类型：扁平列出全部文件，按分类集合过滤，默认隐藏含「.」组件的路径。
        result = container.files.filter {
            (categories.isEmpty || categories.contains($0.category))
                && (showHidden || !pathContainsDotComponent($0.name))
        }.map(BrowserEntry.file)
    } else {
        // 按路径：先算当前目录前缀（根目录为空串），再聚合「直接子目录」与「直接子文件」。
        let prefix = directory.isEmpty ? "" : directory + "/"
        // 直接子目录集合：由目录记录与含子路径的文件共同推导，去重。
        var folders = Set<String>()
        // 直接子文件列表：仅当文件恰好位于当前目录时出现。
        var files: [BrowserEntry] = []
        // 遍历目录记录：位于当前目录下的，取第一段子路径作为直接子目录。
        for folder in container.directories where folder.path.hasPrefix(prefix) {
            let tail = folder.path.dropFirst(prefix.count)
            if let child = tail.split(separator: "/").first,
               showHidden || !child.hasPrefix(".") {
                folders.insert(prefix + child)
            }
        }
        // 遍历文件记录：位于当前目录下的直接列出，位于更深层的只暴露其直接子目录。
        for file in container.files where file.name.hasPrefix(prefix) {
            let tail = file.name.dropFirst(prefix.count)
            if tail.contains("/") {
                // 此文件在更深子目录，登记其第一层目录；真正空目录由上面的目录记录提供。
                if let child = tail.split(separator: "/").first,
                   showHidden || !child.hasPrefix(".") {
                    folders.insert(prefix + child)
                }
            } else if !tail.isEmpty, showHidden || !tail.hasPrefix(".") {
                // 文件就在当前目录：直接进入文件列表。
                files.append(.file(file))
            }
        }
        // 目录在前、文件在后，都按本地化自然顺序排序。
        result = folders.sorted { $0.localizedStandardCompare($1) == .orderedAscending }.map(BrowserEntry.folder)
            + files.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }
    // 搜索词非空时对所有条目做包含匹配（作用在完整路径上）。
    if !search.isEmpty { result = result.filter { $0.path.localizedStandardContains(search) } }
    return result
}

/// 面包屑：路径层级导航的一级（标题 + 点击后进入的路径）。
struct Breadcrumb: Identifiable {
    /// 当前层的显示名称，根为“全部文件”。
    let title: String
    /// 点击后进入的累计内部目录路径，根为空。
    let path: String
    /// 列表唯一标识，直接复用路径。
    var id: String { path }
}

/// 把当前内部目录拆成从根出发的导航层级。
/// 入参：directory（String）为内部路径，空表示根。
/// 返回值：[Breadcrumb]，首项“全部文件”路径为空，每个非空组件追加一级累计路径。
/// 异常：不抛错。
/// 注意事项：split忽略重复/首尾斜杠造成的空段，不验证“..”等业务合法性；只生成导航，不确认目录存在。
func directoryBreadcrumbs(_ directory: String) -> [Breadcrumb] {
    var result = [Breadcrumb(title: "全部文件", path: "")]
    var path = ""
    // 逐段拼接路径，每段生成一个可点击的层级节点。
    for component in directory.split(separator: "/") {
        path = path.isEmpty ? String(component) : path + "/" + component
        result.append(Breadcrumb(title: String(component), path: path))
    }
    return result
}

/// 容器表单状态：创建 / 解锁 / 导入加密备份三种流程共用一套提交入口。
enum ContainerForm: Identifiable {
    /// 在指定空间内创建新容器。
    case create(space: String)
    /// 用密码解锁已存在的容器目录。
    case open(path: String, name: String)
    /// 把 .veil 加密备份导入指定空间。
    case importPackage(path: String, space: String)
    /// 表单身份：create按空间、open按路径、import按包路径；后两者不包含名称/目标空间变化。
    var id: String {
        switch self {
        case .create(let space): "create:\(space)"
        case .open(let path, _): "open:\(path)"
        case .importPackage(let path, _): "import:\(path)"
        }
    }
    /// 表单标题（提交按钮文案也复用）。
    var title: String {
        switch self { case .create: "创建容器"; case .open: "解锁容器"; case .importPackage: "导入加密容器" }
    }
    /// 是否创建流程（需要容器名称与确认密码）。
    var isCreate: Bool { if case .create = self { true } else { false } }
    /// 表单说明文案：创建时展示卖点，解锁时展示容器名，导入时展示备份文件名。
    var detail: String {
        switch self {
        case .create: "设置密码，为重要文件创建一个私密空间。"
        case .open(_, let name): name
        case .importPackage(let path, _): URL(fileURLWithPath: path).lastPathComponent
        }
    }
}
