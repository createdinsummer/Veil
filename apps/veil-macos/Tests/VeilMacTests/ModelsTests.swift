import Foundation
import XCTest
import ImageIO
import UniformTypeIdentifiers
@testable import VeilMac

final class ModelsTests: XCTestCase {
    private func file(_ path: String, category: FileCategory = .document) -> FileEntry {
        FileEntry(id: path, name: path, size: 32, version: 1, encryptedAt: "2026-11-04T12:00:00Z", category: category, previewMime: "text/plain")
    }

    func testPathBrowsingKeepsEmptyFoldersAndDirectoryBoundaries() {
        let container = ContainerSnapshot(id: "one", name: "测试", path: "/tmp/one", createdAt: "", files: [
            file("work/note.txt"), file("work/sub/child.txt"), file("workspace/hidden.txt"), file("root.txt")
        ], directories: [DirectoryEntry(id: "empty", path: "work/empty")])
        let rows = browserEntries(container: container, mode: .path, directory: "work", categories: [], search: "")
        XCTAssertEqual(rows.map(\.path), ["work/empty", "work/sub", "work/note.txt"])
        XCTAssertEqual(rows.map(\.name), ["empty", "sub", "note.txt"])
    }

    func testCategorySearchAndBreadcrumbs() {
        let container = ContainerSnapshot(id: "one", name: "测试", path: "/tmp/one", createdAt: "", files: [
            file("旅行/照片.jpg", category: .photo), file("旅行/说明.txt"), file("工作/记录.txt")
        ], directories: [])
        let rows = browserEntries(container: container, mode: .type, directory: "", categories: [.document], search: "旅行")
        XCTAssertEqual(rows.map(\.path), ["旅行/说明.txt"])
        XCTAssertEqual(directoryBreadcrumbs("旅行/去年").map(\.path), ["", "旅行", "旅行/去年"])
        XCTAssertEqual(file("旅行/说明.txt").parentName, "旅行")
        XCTAssertEqual(file("说明.txt").parentName, "根目录")
    }
}

@MainActor
final class NativeClientTests: XCTestCase {
    func testFormNavigationLocksThePreviousContainerAndClearsCancelledInputState() async throws {
        guard ProcessInfo.processInfo.environment["VEIL_APP_DATA_DIR"] != nil,
              ProcessInfo.processInfo.environment["VEIL_NATIVE_TESTS"] == "1" else {
            throw XCTSkip("导航集成测试需要独立的测试目录")
        }
        let store = AppStore()
        await store.load()
        let originalSpace = try XCTUnwrap(store.activeSpace)
        await store.submit(.create(space: originalSpace.id), name: "导航测试 " + UUID().uuidString,
                           password: "navigation-test-password", confirmation: "navigation-test-password")
        let previous = try XCTUnwrap(store.container)
        await store.createSpace(name: "目标空间 " + UUID().uuidString)
        let target = try XCTUnwrap(store.spaces.first { $0.id != originalSpace.id })
        await store.newContainer(in: target.id)
        XCTAssertNil(store.container)
        XCTAssertEqual(store.activeSpace?.id, target.id)
        XCTAssertEqual(store.form?.id, ContainerForm.create(space: target.id).id)
        store.error = "测试错误"
        store.cancelForm()
        XCTAssertNil(store.form)
        XCTAssertNil(store.error)
        await store.chooseContainer(ContainerSummary(id: previous.id, name: previous.name, path: previous.path))
        XCTAssertEqual(store.form?.id, ContainerForm.open(path: previous.path, name: previous.name).id)
        await store.selectSpace(target)
        XCTAssertNil(store.form)
        XCTAssertNil(store.container)
        await store.newContainer(in: target.id)
        let newSpaceName = "新增空间 " + UUID().uuidString
        await store.createSpace(name: newSpaceName)
        XCTAssertNil(store.form)
        XCTAssertEqual(store.activeSpace?.name, newSpaceName)
        await store.lock()
    }

    func testThumbnailsAreDownsampledAndClearedOnLock() async throws {
        guard ProcessInfo.processInfo.environment["VEIL_APP_DATA_DIR"] != nil,
              ProcessInfo.processInfo.environment["VEIL_NATIVE_TESTS"] == "1" else {
            throw XCTSkip("缩略图集成测试需要独立的测试目录")
        }
        let store = AppStore()
        await store.load()
        let space = try XCTUnwrap(store.activeSpace)
        await store.submit(.create(space: space.id), name: "缩略图测试 " + UUID().uuidString,
                           password: "thumbnail-test-password", confirmation: "thumbnail-test-password")
        XCTAssertNotNil(store.container)
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let url = folder.appendingPathComponent("照片.png")
        let context = try XCTUnwrap(CGContext(data: nil, width: 1200, height: 800, bitsPerComponent: 8,
                                             bytesPerRow: 0, space: CGColorSpaceCreateDeviceRGB(),
                                             bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue))
        context.setFillColor(CGColor(red: 0.4, green: 0.6, blue: 0.5, alpha: 1))
        context.fill(CGRect(x: 0, y: 0, width: 1200, height: 800))
        let source = try XCTUnwrap(context.makeImage())
        let destination = try XCTUnwrap(CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil))
        CGImageDestinationAddImage(destination, source, nil)
        XCTAssertTrue(CGImageDestinationFinalize(destination))
        await store.importFiles([url])
        XCTAssertNil(store.error)
        await store.loadThumbnails()
        let file = try XCTUnwrap(store.container?.files.first)
        let bytes = try XCTUnwrap(store.thumbnails[file.id])
        let imageSource = try XCTUnwrap(CGImageSourceCreateWithData(bytes as CFData, nil))
        let thumbnail = try XCTUnwrap(CGImageSourceCreateImageAtIndex(imageSource, 0, nil))
        XCTAssertLessThanOrEqual(thumbnail.width, 320)
        XCTAssertLessThanOrEqual(thumbnail.height, 320)
        XCTAssertEqual(store.thumbnails.count, 1)
        await store.lock()
        XCTAssertTrue(store.thumbnails.isEmpty)
        XCTAssertNil(store.container)
        await store.loadThumbnails()
        XCTAssertTrue(store.thumbnails.isEmpty)
    }

    func testVideoPreviewRemovesItsPrivateTemporaryFile() throws {
        let file = FileEntry(id: "video", name: "旅行/短片.mp4", size: 8, version: 1,
                             encryptedAt: "", category: .video, previewMime: "video/mp4")
        let asset = PreviewAsset(file: file)
        try asset.install(Data("test-only".utf8))
        let url = try XCTUnwrap(asset.videoURL)
        XCTAssertTrue(FileManager.default.fileExists(atPath: url.path))
        let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
        XCTAssertEqual((attributes[.posixPermissions] as? NSNumber)?.intValue, 0o600)
        asset.close()
        XCTAssertFalse(FileManager.default.fileExists(atPath: url.deletingLastPathComponent().path))
        XCTAssertNil(asset.videoURL)
        XCTAssertNil(asset.data)
    }

    func testSwiftCallsRealRustService() async throws {
        guard ProcessInfo.processInfo.environment["VEIL_APP_DATA_DIR"] != nil,
              ProcessInfo.processInfo.environment["VEIL_NATIVE_TESTS"] == "1" else {
            throw XCTSkip("集成测试需要独立的 VEIL_APP_DATA_DIR 和 VEIL_NATIVE_TESTS=1")
        }
        let client = ServiceClient()
        let spaces: [Space] = try await client.json(NativeRequest(operation: "list_spaces"))
        let space = try XCTUnwrap(spaces.first(where: \.isDefault))
        let container: ContainerSnapshot = try await client.json(NativeRequest(operation: "create_container", space: space.id, name: "Swift 测试 " + UUID().uuidString, password: "native-test-password"))
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appendingPathComponent("说明.txt")
        let content = Data("Swift 与 Rust 的真实调用。\n".utf8)
        try content.write(to: source)
        let updated: ContainerSnapshot = try await client.json(NativeRequest(operation: "import_files", sources: [source.path], directory: ""))
        XCTAssertEqual(updated.files.count, 1)
        XCTAssertEqual(updated.files.first?.category, .document)
        let preview = try await client.preview(name: "说明.txt")
        XCTAssertEqual(preview, content)
        try await client.execute(NativeRequest(operation: "lock"))
        do { _ = try await client.preview(name: "说明.txt"); XCTFail("锁定后不能预览") }
        catch { XCTAssertTrue(error.localizedDescription.contains("打开容器")) }
        let reopened: ContainerSnapshot = try await client.json(NativeRequest(operation: "open_container", password: "native-test-password", path: container.path))
        XCTAssertEqual(reopened.id, container.id)
        try await client.execute(NativeRequest(operation: "lock"))
    }
}
