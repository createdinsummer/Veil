import Foundation

struct Space: Decodable, Identifiable, Sendable {
    let id: String
    let name: String
    let path: String
    let isDefault: Bool
    let containers: [ContainerSummary]
}

struct ContainerSummary: Decodable, Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let path: String
}

struct RecentContainer: Decodable, Identifiable, Sendable {
    let id: String
    let name: String
    let path: String
}

struct ContainerSnapshot: Decodable, Sendable {
    let id: String
    let name: String
    let path: String
    let createdAt: String
    let files: [FileEntry]
    let directories: [DirectoryEntry]
}

struct DirectoryEntry: Decodable, Sendable {
    let id: String
    let path: String
}

enum FileCategory: String, Decodable, CaseIterable, Identifiable, Sendable {
    case photo, video, document, archive, other
    var id: String { rawValue }
    var title: String {
        switch self {
        case .photo: "照片"
        case .video: "视频"
        case .document: "文档"
        case .archive: "压缩包"
        case .other: "其他"
        }
    }
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

struct FileEntry: Decodable, Identifiable, Hashable, Sendable {
    let id: String
    let name: String
    let size: UInt64
    let version: Int
    let encryptedAt: String
    let category: FileCategory
    let previewMime: String?

    var leafName: String { name.components(separatedBy: "/").last ?? name }
    var parentName: String {
        guard let slash = name.lastIndex(of: "/") else { return "根目录" }
        return String(name[..<slash])
    }
    var sizeLabel: String { ByteCountFormatter.string(fromByteCount: Int64(clamping: size), countStyle: .file) }
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

enum BrowseMode: String, CaseIterable, Sendable { case type, path }
enum FileLayout: String, Sendable { case grid, list }

enum BrowserEntry: Identifiable, Hashable, Sendable {
    case folder(String)
    case file(FileEntry)
    var id: String {
        switch self { case .folder(let path): "folder:\(path)"; case .file(let file): file.id }
    }
    var path: String {
        switch self { case .folder(let path): path; case .file(let file): file.name }
    }
    var name: String { path.components(separatedBy: "/").last ?? path }
    var symbol: String {
        switch self { case .folder: "folder.fill"; case .file(let file): file.category.symbol }
    }
}

/// 这是 macOS 界面的展示计算；业务路径的合法性仍由 Rust 校验。
func browserEntries(container: ContainerSnapshot, mode: BrowseMode, directory: String,
                    categories: Set<FileCategory>, search: String) -> [BrowserEntry] {
    var result: [BrowserEntry]
    if mode == .type {
        result = container.files.filter { categories.isEmpty || categories.contains($0.category) }.map(BrowserEntry.file)
    } else {
        let prefix = directory.isEmpty ? "" : directory + "/"
        var folders = Set<String>()
        var files: [BrowserEntry] = []
        for folder in container.directories where folder.path.hasPrefix(prefix) {
            let tail = folder.path.dropFirst(prefix.count)
            if let child = tail.split(separator: "/").first { folders.insert(prefix + child) }
        }
        for file in container.files where file.name.hasPrefix(prefix) {
            let tail = file.name.dropFirst(prefix.count)
            if tail.contains("/") {
                if let child = tail.split(separator: "/").first { folders.insert(prefix + child) }
            } else if !tail.isEmpty { files.append(.file(file)) }
        }
        result = folders.sorted { $0.localizedStandardCompare($1) == .orderedAscending }.map(BrowserEntry.folder)
            + files.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }
    if !search.isEmpty { result = result.filter { $0.path.localizedStandardContains(search) } }
    return result
}

struct Breadcrumb: Identifiable {
    let title: String
    let path: String
    var id: String { path }
}

func directoryBreadcrumbs(_ directory: String) -> [Breadcrumb] {
    var result = [Breadcrumb(title: "全部文件", path: "")]
    var path = ""
    for component in directory.split(separator: "/") {
        path = path.isEmpty ? String(component) : path + "/" + component
        result.append(Breadcrumb(title: String(component), path: path))
    }
    return result
}

enum ContainerForm: Identifiable {
    case create(space: String)
    case open(path: String, name: String)
    case importPackage(path: String, space: String)
    var id: String {
        switch self {
        case .create(let space): "create:\(space)"
        case .open(let path, _): "open:\(path)"
        case .importPackage(let path, _): "import:\(path)"
        }
    }
    var title: String {
        switch self { case .create: "创建容器"; case .open: "解锁容器"; case .importPackage: "导入加密容器" }
    }
    var isCreate: Bool { if case .create = self { true } else { false } }
    var detail: String {
        switch self {
        case .create: "设置密码，为重要文件创建一个私密空间。"
        case .open(_, let name): name
        case .importPackage(let path, _): URL(fileURLWithPath: path).lastPathComponent
        }
    }
}
