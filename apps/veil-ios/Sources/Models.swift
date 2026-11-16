/// ============================================================================
/// Veil iOS 客户端 · 数据模型与展示计算（Models）
///
/// 文件作用：定义 iOS 客户端全部业务数据模型（空间、容器、文件条目、浏览条目等），
/// 以及把“容器快照 + 浏览条件”换算成界面列表的纯展示计算函数。
/// 所属模块：iOS 客户端数据层，模型与服务端 JSON 字段一一对应。
/// 文件内核心类型 / 函数清单：
///   - Space              空间（容器分组的磁盘目录与登记信息）
///   - ContainerSummary   未解锁容器的登记摘要
///   - RecentContainer    最近使用容器记录
///   - ContainerSnapshot  解锁后的容器完整快照（元数据 + 目录树 + 文件列表）
///   - DirectoryEntry     目录树节点
///   - FileCategory       文件分类枚举（照片 / 视频 / 文档 / 压缩包 / 其他）
///   - FileEntry          文件条目（路径、大小、版本、分类等元数据）
///   - folderNameForCreation 新建文件夹名称校验
///   - BrowseMode / FileLayout 浏览模式与列表布局枚举
///   - BrowserEntry       文件夹 / 文件统一浏览条目
///   - browserEntries     快照 → 浏览条目列表的核心展示计算
///   - Breadcrumb / directoryBreadcrumbs 目录面包屑
///   - ContainerForm      创建 / 解锁 / 导入加密备份三种表单场景
/// 依赖能力：Foundation（Decodable、DateFormatter 等）。
/// 整体业务目标：与 CLI / 桌面端共用容器格式；容器身份由稳定 veil_id 表示，
///   展示名称和磁盘路径变化不影响身份。目录树、内容版本与文件密钥保存在各容器的
///   veil.db（SQLCipher 加密数据库）里，本文件的模型只承载其展示视图。
/// ============================================================================
import Foundation

/// 空间：一组加密容器的存放目录与登记信息。
/// 空间具有稳定 id，实际磁盘路径由应用数据库登记；展示名称变化不影响身份。
struct Space: Decodable, Identifiable, Sendable {
    /// 空间稳定 id。
    let id: String
    /// 空间展示名称。
    let name: String
    /// 空间在磁盘上的目录路径。
    let path: String
    /// 是否匹配当前默认空间ID；读取默认空间的服务可在引用缺失时建立默认项，非本模型自动重建。
    let isDefault: Bool
    /// 登记路径当前是否为目录的观察结果，不证明可写权限或每个容器均存在。
    let isAvailable: Bool
    /// 当前空间展示摘要集合；目录不可用时服务可能置空，不能据此断定已无登记或数据。
    let containers: [ContainerSummary]
}

/// 容器摘要：本机登记表里的一条容器记录，未解锁时用于首页展示。
struct ContainerSummary: Decodable, Identifiable, Hashable, Sendable {
    /// 容器稳定 id（veil_id）。
    let id: String
    /// 容器展示名称。
    let name: String
    /// 容器目录在磁盘上的路径。
    let path: String
}

/// 最近使用的容器记录（字段与 ContainerSummary 一致，供最近列表复用）。
struct RecentContainer: Decodable, Identifiable, Sendable {
    /// 容器稳定 id。
    let id: String
    /// 容器展示名称。
    let name: String
    /// 容器目录路径。
    let path: String
}

/// 容器快照：解锁成功后服务返回的完整视图（元数据 + 目录树 + 文件列表）。
/// 注意：文件内容与密钥并不在快照里，只包含目录树、内容版本等展示元数据。
struct ContainerSnapshot: Decodable, Sendable {
    /// 容器稳定 id。
    let id: String
    /// 容器展示名称。
    let name: String
    /// 容器目录路径。
    let path: String
    /// 容器创建时间（ISO 8601 字符串）。
    let createdAt: String
    /// 容器内全部文件条目（含路径、版本、分类等）。
    let files: [FileEntry]
    /// 容器内全部目录条目（按路径浏览时用于合成子目录列表）。
    let directories: [DirectoryEntry]
}

/// 目录条目：容器目录树中的一个目录节点。
struct DirectoryEntry: Decodable, Sendable {
    /// 目录稳定 id。
    let id: String
    /// 目录在容器内的路径。
    let path: String
}

/// 文件分类：用于按类型浏览、图标选择与缩略图策略。
enum FileCategory: String, Decodable, CaseIterable, Identifiable, Sendable {
    /// 五个分类：照片、视频、文档、压缩包、其他。
    case photo, video, document, archive, other
    /// 分类的唯一标识，直接使用原始字符串。
    var id: String { rawValue }
    /// 分类中文名（筛选条与详情展示用）。
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

/// 文件条目：容器目录树里的一个加密文件（含路径、大小、版本与分类等元数据）。
struct FileEntry: Decodable, Identifiable, Hashable, Sendable {
    /// 文件稳定 id。
    let id: String
    /// 文件在容器内的完整路径（例如 "照片/旅行/a.jpg"）。
    let name: String
    /// 文件大小（字节）。
    let size: UInt64
    /// 内容版本链长度，内容变化追加；不等于可证明内容相同的版本UUID，重加密可不改变它。
    let version: Int
    /// 最近一次加密 / 修改时间（ISO 8601 字符串）。
    let encryptedAt: String
    /// 服务端判定好的文件分类。
    let category: FileCategory
    /// 预览 MIME 类型（可能为空）。
    let previewMime: String?

    /// 文件名（去掉路径前缀后的最后一段）。
    var leafName: String { name.components(separatedBy: "/").last ?? name }
    /// 上级目录路径（根目录下的文件显示“根目录”）。
    var parentName: String {
        guard let slash = name.lastIndex(of: "/") else { return "根目录" }
        return String(name[..<slash])
    }
    /// 人类可读的大小文案（例如 "1.2 MB"）。
    var sizeLabel: String { ByteCountFormatter.string(fromByteCount: Int64(clamping: size), countStyle: .file) }
    /// 人类可读的修改日期文案；解析失败时原样展示原始字符串。
    var dateLabel: String {
        let formatter = ISO8601DateFormatter()
        // 先按带毫秒的格式解析，失败再退回不带毫秒的格式。
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

/// 浏览模式：按类型分类浏览，或按目录路径逐级浏览。
enum BrowseMode: String, CaseIterable, Sendable { case type, path }
/// 文件列表布局：网格或列表。
enum FileLayout: String, Sendable { case grid, list }

/// 浏览条目：文件夹与文件的统一包装，供列表 / 网格一起渲染。
enum BrowserEntry: Identifiable, Hashable, Sendable {
    /// 文件夹（携带容器内路径）。
    case folder(String)
    /// 文件（携带完整文件条目）。
    case file(FileEntry)
    /// 列表唯一标识：文件夹加 "folder:" 前缀，避免与文件 id 冲突。
    var id: String {
        switch self { case .folder(let path): "folder:\(path)"; case .file(let file): file.id }
    }
    /// 条目在容器内的路径（文件夹路径或文件路径）。
    var path: String {
        switch self { case .folder(let path): path; case .file(let file): file.name }
    }
    /// 条目显示名（路径最后一段）。
    var name: String { path.components(separatedBy: "/").last ?? path }
    /// 条目图标：文件夹固定图标，文件用分类图标。
    var symbol: String {
        switch self { case .folder: "folder.fill"; case .file(let file): file.category.symbol }
    }
}

/// 根据本地浏览条件从容器快照计算当前可见文件和目录项。
/// 入参：container（ContainerSnapshot）含完整文件/目录列表；mode为type/path；directory（String）为规范化内部路径，根为空；categories（Set<FileCategory>）空为全部；search为本地化包含搜索词。
/// 返回值：[BrowserEntry]：类型模式只含筛选文件且保留输入顺序；路径模式聚合直接子目录去重并按自然顺序目录在前、文件在后，最后按完整路径搜索。
/// 异常：不抛错，不访问服务。
/// 注意事项：分类筛选仅类型模式生效；搜索不是递归查库。本iOS版本不提供隐藏点文件筛选，和macOS不同。
func browserEntries(container: ContainerSnapshot, mode: BrowseMode, directory: String,
                    categories: Set<FileCategory>, search: String) -> [BrowserEntry] {
    var result: [BrowserEntry]
    if mode == .type {
        // 按类型：所有文件按分类过滤（空分类集合 = 全部），此模式不展示文件夹。
        result = container.files.filter { categories.isEmpty || categories.contains($0.category) }.map(BrowserEntry.file)
    } else {
        // 按路径：先算出当前目录的“目录 + /”前缀。
        let prefix = directory.isEmpty ? "" : directory + "/"
        // folders 收集当前目录下的直接子目录（Set 去重），files 收集当前目录下的直接文件。
        var folders = Set<String>()
        var files: [BrowserEntry] = []
        // 遍历目录树：目录路径以当前目录前缀开头，取剩余路径的第一段即直接子目录。
        for folder in container.directories where folder.path.hasPrefix(prefix) {
            let tail = folder.path.dropFirst(prefix.count)
            if let child = tail.split(separator: "/").first { folders.insert(prefix + child) }
        }
        // 遍历文件：路径以当前目录前缀开头；剩余路径带 "/" 说明在更深目录，只记其直接子目录。
        for file in container.files where file.name.hasPrefix(prefix) {
            let tail = file.name.dropFirst(prefix.count)
            if tail.contains("/") {
                if let child = tail.split(separator: "/").first { folders.insert(prefix + child) }
            } else if !tail.isEmpty { files.append(.file(file)) }
        }
        // 文件夹排在前面，两组各自按本地化规则排序。
        result = folders.sorted { $0.localizedStandardCompare($1) == .orderedAscending }.map(BrowserEntry.folder)
            + files.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }
    // 搜索关键字存在时，对最终结果按路径做本地化模糊过滤。
    if !search.isEmpty { result = result.filter { $0.path.localizedStandardContains(search) } }
    return result
}

/// 面包屑节点：标题 + 对应目录路径，用于展示浏览层级。
struct Breadcrumb: Identifiable {
    /// 显示标题（目录名）。
    let title: String
    /// 该节点对应的容器内目录路径。
    let path: String
    /// 列表唯一标识，直接用路径。
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
    // 逐段拼接路径：每拼一级就追加一个面包屑节点。
    for component in directory.split(separator: "/") {
        path = path.isEmpty ? String(component) : path + "/" + component
        result.append(Breadcrumb(title: String(component), path: path))
    }
    return result
}

/// 容器表单场景：创建容器、解锁已有容器、导入加密备份（.veil）三种。
enum ContainerForm: Identifiable {
    /// 在指定空间新建容器（携带空间 id）。
    case create(space: String)
    /// 解锁已有容器（携带磁盘路径与展示名称）。
    case open(path: String, name: String)
    /// 把 .veil 加密备份导入指定空间（携带备份文件路径与空间 id）。
    case importPackage(path: String, space: String)
    /// sheet身份：create按空间、open按路径、import按包路径；同包换目标空间不会改变此id。
    var id: String {
        switch self {
        case .create(let space): "create:\(space)"
        case .open(let path, _): "open:\(path)"
        case .importPackage(let path, _): "import:\(path)"
        }
    }
    /// 表单标题。
    var title: String {
        switch self { case .create: "创建容器"; case .open: "解锁容器"; case .importPackage: "导入加密容器" }
    }
    /// 是否为“创建容器”场景（决定表单字段布局与文案）。
    var isCreate: Bool { if case .create = self { true } else { false } }
    /// 解锁 / 导入场景下展示的目标说明（容器名或备份文件名）。
    var detail: String {
        switch self {
        case .create: "设置密码，为重要文件创建一个私密空间。"
        case .open(_, let name): name
        case .importPackage(let path, _): URL(fileURLWithPath: path).lastPathComponent
        }
    }
}
