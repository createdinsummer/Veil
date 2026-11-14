import Foundation
import XCTest
import ImageIO
import UniformTypeIdentifiers
@testable import VeilMac

final class ModelsTests: XCTestCase {
    func testClientErrorsKeepCodesWithoutPersistingUserText() throws {
        guard ProcessInfo.processInfo.environment["VEIL_APP_DATA_DIR"] != nil else {
            throw XCTSkip("日志集成测试需要独立的测试目录")
        }
        func records() throws -> [[String: Any]] {
            try XCTUnwrap(JSONSerialization.jsonObject(with: AppLog.export()) as? [[String: Any]])
        }
        let initialCount = try records().count
        let secret = "文件读取失败 password=secret-password /Users/private/confidential.txt"
        let underlying = NSError(domain: NSPOSIXErrorDomain, code: 13,
                                 userInfo: [NSLocalizedDescriptionKey: "原始文档内容及私人账号"])
        AppLog.capture(NSError(domain: NSCocoaErrorDomain, code: NSFileReadNoSuchFileError,
                               userInfo: [NSLocalizedDescriptionKey: secret,
                                          NSUnderlyingErrorKey: underlying]))
        XCTAssertEqual(AppLog.validation(secret), secret)
        let captured = AppLog.recorded(ClientError.local(secret))
        AppLog.capture(captured)
        AppLog.capture(CancellationError())
        AppLog.capture(NSError(domain: NSCocoaErrorDomain, code: NSUserCancelledError))
        AppLog.capture(NSError(domain: NSURLErrorDomain, code: NSURLErrorCancelled))
        AppLog.capture(ClientError.service(secret))
        AppLog.capture(ClientError.unsupportedContent(secret))
        let result = try records()
        XCTAssertEqual(result.count - initialCount, 2)
        XCTAssertEqual(result.last?["source_file"] as? String, "ModelsTests.swift")
        let text = String(decoding: try AppLog.export(), as: UTF8.self)
        XCTAssertFalse(text.contains("secret-password"))
        XCTAssertFalse(text.contains("/Users/private"))
        XCTAssertFalse(text.contains("文件读取失败"))
        XCTAssertFalse(text.contains("原始文档内容及私人账号"))
        XCTAssertTrue(text.contains("Cocoa code=260"))
        XCTAssertTrue(text.contains("POSIX code=13"))
        XCTAssertTrue(result.contains { ($0["native_code"] as? Int) == NSFileReadNoSuchFileError })
    }

    func testDecodingDiagnosticsOmitPayloadAndFieldNames() throws {
        guard ProcessInfo.processInfo.environment["VEIL_APP_DATA_DIR"] != nil else {
            throw XCTSkip("日志集成测试需要独立的测试目录")
        }
        let secret = "客户完整私人数据无扩展名"
        let error = DecodingError.dataCorrupted(.init(codingPath: [], debugDescription: secret))
        AppLog.capture(error)
        let data = try AppLog.export()
        let text = String(decoding: data, as: UTF8.self)
        XCTAssertFalse(text.contains(secret))
        XCTAssertTrue(text.contains("corrupted_data"))
    }

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

    func testDotEntriesAreHiddenByDefaultAndCanBeRevealed() {
        let container = ContainerSnapshot(id: "one", name: "测试", path: "/tmp/one", createdAt: "", files: [
            file(".DS_Store"), file("可见.txt"), file("资料/.hidden.txt"), file("资料/说明.txt"), file(".配置/内容.txt")
        ], directories: [
            DirectoryEntry(id: "hidden", path: ".配置"), DirectoryEntry(id: "visible", path: "资料")
        ])
        // 默认隐藏：根目录只显示「可见.txt」与「资料」，隐藏文件与隐藏目录不出现。
        let rootRows = browserEntries(container: container, mode: .path, directory: "", categories: [], search: "")
        XCTAssertEqual(rootRows.map(\.path), ["资料", "可见.txt"])
        // 进入普通目录后，其中的隐藏文件同样默认不显示。
        let insideRows = browserEntries(container: container, mode: .path, directory: "资料", categories: [], search: "")
        XCTAssertEqual(insideRows.map(\.path), ["资料/说明.txt"])
        // 按类型扁平浏览时，隐藏文件及位于隐藏目录内的文件都不显示。
        let typeRows = browserEntries(container: container, mode: .type, directory: "", categories: [], search: "")
        XCTAssertEqual(typeRows.map(\.path), ["可见.txt", "资料/说明.txt"])
        // 打开开关后，隐藏文件与隐藏目录恢复显示；隐藏目录内容仍聚合在自身路径下。
        let revealed = browserEntries(container: container, mode: .path, directory: "", categories: [], search: "", showHidden: true)
        XCTAssertEqual(Set(revealed.map(\.name)), [".DS_Store", "可见.txt", "资料", ".配置"])
        let revealedType = browserEntries(container: container, mode: .type, directory: "", categories: [], search: "", showHidden: true)
        XCTAssertEqual(Set(revealedType.map(\.name)), [".DS_Store", "可见.txt", ".hidden.txt", "说明.txt", "内容.txt"])
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
    func testDeleteContainerClearsDirectoryRegistrationAndSession() async throws {
        guard ProcessInfo.processInfo.environment["VEIL_APP_DATA_DIR"] != nil,
              ProcessInfo.processInfo.environment["VEIL_NATIVE_TESTS"] == "1" else {
            throw XCTSkip("删除集成测试需要独立的测试目录")
        }
        let store = AppStore()
        await store.load()
        let space = try XCTUnwrap(store.activeSpace)
        let password = "delete-test-password"
        await store.submit(.create(space: space.id), name: "删除兼容性 " + UUID().uuidString,
                           password: password, confirmation: password)
        let container = try XCTUnwrap(store.container)
        let item = ContainerSummary(id: container.id, name: container.name, path: container.path)
        let deleted = await store.deleteContainer(item)
        XCTAssertTrue(deleted)
        XCTAssertNil(store.error)
        XCTAssertNil(store.container)
        XCTAssertFalse(FileManager.default.fileExists(atPath: container.path))
        XCTAssertFalse(store.spaces.flatMap(\.containers).contains { $0.id == container.id })
        XCTAssertFalse(store.recents.contains { $0.id == container.id })
    }

    func testCustomSpaceDirectoryIsPersistedAndUsedForNewContainers() async throws {
        guard ProcessInfo.processInfo.environment["VEIL_APP_DATA_DIR"] != nil,
              ProcessInfo.processInfo.environment["VEIL_NATIVE_TESTS"] == "1" else {
            throw XCTSkip("目录集成测试需要独立的测试目录")
        }
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let existingFile = root.appendingPathComponent("保留.txt")
        try Data("已有文件".utf8).write(to: existingFile)
        let name = "自选目录 " + UUID().uuidString
        let store = AppStore()
        await store.load()
        await store.createSpace(name: name, directory: root)
        XCTAssertNil(store.error)
        let space = try XCTUnwrap(store.activeSpace)
        XCTAssertEqual(space.name, name)
        let actualDirectory = try FileManager.default.attributesOfItem(atPath: space.path)
        let selectedDirectory = try FileManager.default.attributesOfItem(atPath: root.path)
        XCTAssertEqual(try XCTUnwrap(actualDirectory[.systemFileNumber] as? NSNumber),
                       try XCTUnwrap(selectedDirectory[.systemFileNumber] as? NSNumber))
        XCTAssertEqual(try XCTUnwrap(actualDirectory[.systemNumber] as? NSNumber),
                       try XCTUnwrap(selectedDirectory[.systemNumber] as? NSNumber))
        await store.submit(.create(space: space.id), name: "新容器", password: "directory-test-password", confirmation: "directory-test-password")
        XCTAssertNil(store.error)
        let container = try XCTUnwrap(store.container)
        XCTAssertEqual(URL(fileURLWithPath: container.path).deletingLastPathComponent().path, space.path)
        XCTAssertTrue(FileManager.default.fileExists(atPath: container.path + "/veil.db"))
        await store.lock()
        let reopened = AppStore()
        await reopened.load()
        XCTAssertEqual(reopened.spaces.first { $0.id == space.id }?.path, space.path)
        await reopened.createSpace(name: "重复目录 " + UUID().uuidString, directory: root)
        XCTAssertTrue(reopened.error?.contains("此目录已用于其他空间") == true)
        XCTAssertEqual(try Data(contentsOf: existingFile), Data("已有文件".utf8))
    }

    func testUnlockCacheSkipsPasswordWithinWindowAndExpires() async throws {
        guard ProcessInfo.processInfo.environment["VEIL_APP_DATA_DIR"] != nil,
              ProcessInfo.processInfo.environment["VEIL_NATIVE_TESTS"] == "1" else {
            throw XCTSkip("免密集成测试需要独立的测试目录")
        }
        let store = AppStore()
        await store.load()
        await store.setUnlockCache(seconds: 3600)
        let space = try XCTUnwrap(store.activeSpace)
        let password = "unlock-cache-password"
        await store.submit(.create(space: space.id), name: "免密测试 " + UUID().uuidString,
                           password: password, confirmation: password)
        let created = try XCTUnwrap(store.container)
        await store.lock()
        XCTAssertNil(store.container)
        XCTAssertNil(store.form)
        // 自动锁定后，同一容器在 1 小时内直接用内存密码重开，不出现密码表单。
        await store.chooseContainer(ContainerSummary(id: created.id, name: created.name, path: created.path))
        XCTAssertNil(store.error)
        XCTAssertNil(store.form)
        XCTAssertEqual(store.container?.id, created.id)

        // 主动锁定会清除内存密码，必须重新输入。
        await store.lock(forgetPassword: true)
        await store.chooseContainer(ContainerSummary(id: created.id, name: created.name, path: created.path))
        XCTAssertEqual(store.form?.id, ContainerForm.open(path: created.path, name: created.name).id)
        store.cancelForm()

        // 超过配置时长后不再免密。
        await store.setUnlockCache(seconds: 1)
        await store.submit(.open(path: created.path, name: created.name), name: "",
                           password: password, confirmation: "")
        XCTAssertNotNil(store.container)
        await store.lock()
        try await Task.sleep(for: .milliseconds(1200))
        await store.chooseContainer(ContainerSummary(id: created.id, name: created.name, path: created.path))
        XCTAssertEqual(store.form?.id, ContainerForm.open(path: created.path, name: created.name).id)
        store.cancelForm()
        await store.setUnlockCache(seconds: 3600)
        await store.lock(forgetPassword: true)
    }

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
        let targetName = "目标空间 " + UUID().uuidString
        await store.createSpace(name: targetName)
        let target = try XCTUnwrap(store.spaces.first { $0.name == targetName })
        await store.newContainer(in: target.id)
        XCTAssertNil(store.container)
        XCTAssertEqual(store.activeSpace?.id, target.id)
        XCTAssertEqual(store.form?.id, ContainerForm.create(space: target.id).id)
        store.error = "测试错误"
        store.cancelForm()
        XCTAssertNil(store.form)
        XCTAssertNil(store.error)
        // 刚创建的容器仍在免密时长内，重开时直接解锁而不显示密码表单。
        await store.chooseContainer(ContainerSummary(id: previous.id, name: previous.name, path: previous.path))
        XCTAssertNil(store.form)
        XCTAssertEqual(store.container?.id, previous.id)
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

    func testPreviewRemovesItsPrivateTemporaryFile() throws {
        let file = FileEntry(id: "video", name: "旅行/短片.mp4", size: 8, version: 1,
                             encryptedAt: "", category: .video, previewMime: "video/mp4")
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("veil-native-preview-" + UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false,
                                                attributes: [.posixPermissions: 0o700])
        let url = folder.appendingPathComponent(file.leafName)
        try Data("test-only".utf8).write(to: url)
        let asset = PreviewAsset(file: file)
        try asset.install(url, folder: folder)
        let installed = try XCTUnwrap(asset.contentURL)
        XCTAssertTrue(FileManager.default.fileExists(atPath: installed.path))
        let attributes = try FileManager.default.attributesOfItem(atPath: installed.path)
        XCTAssertEqual((attributes[.posixPermissions] as? NSNumber)?.intValue, 0o600)
        asset.close()
        XCTAssertFalse(FileManager.default.fileExists(atPath: installed.deletingLastPathComponent().path))
        XCTAssertNil(asset.contentURL)
    }

    func testShowPreviewWritesTemporaryFileAndLockCleansItUp() async throws {
        guard ProcessInfo.processInfo.environment["VEIL_APP_DATA_DIR"] != nil,
              ProcessInfo.processInfo.environment["VEIL_NATIVE_TESTS"] == "1" else {
            throw XCTSkip("预览集成测试需要独立的测试目录")
        }
        let store = AppStore()
        await store.load()
        let space = try XCTUnwrap(store.activeSpace)
        await store.submit(.create(space: space.id), name: "预览测试 " + UUID().uuidString,
                           password: "preview-test-password", confirmation: "preview-test-password")
        XCTAssertNotNil(store.container)
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let source = folder.appendingPathComponent("说明.txt")
        let content = "打开预览后解密到临时目录。\n"
        try Data(content.utf8).write(to: source)
        await store.importFiles([source])
        XCTAssertNil(store.error)
        guard let entry = store.visibleEntries.first, case .file(let file) = entry else {
            return XCTFail("缺少导入的文件")
        }
        await store.showPreview(file)
        let asset = try XCTUnwrap(store.preview)
        let url = try XCTUnwrap(asset.contentURL, asset.error ?? "未生成预览临时文件")
        XCTAssertTrue(FileManager.default.fileExists(atPath: url.path))
        XCTAssertEqual(try String(contentsOf: url, encoding: .utf8), content)
        // 锁定容器必须同时清空预览并删除临时文件。
        await store.lock()
        XCTAssertNil(store.preview)
        XCTAssertFalse(FileManager.default.fileExists(atPath: url.deletingLastPathComponent().path))
    }

    func testExportReviewAndCancellationPreserveExistingFiles() async throws {
        guard ProcessInfo.processInfo.environment["VEIL_APP_DATA_DIR"] != nil,
              ProcessInfo.processInfo.environment["VEIL_NATIVE_TESTS"] == "1" else {
            throw XCTSkip("导出集成测试需要独立的测试目录")
        }
        let store = AppStore()
        await store.load()
        let space = try XCTUnwrap(store.activeSpace)
        await store.submit(.create(space: space.id), name: "导出测试 " + UUID().uuidString,
                           password: "export-password", confirmation: "export-password")
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let source = root.appendingPathComponent("source.txt")
        try Data("original".utf8).write(to: source)
        await store.importFiles([source])
        let output = root.appendingPathComponent("output.txt")
        await store.exportPaths(["source.txt"], to: output)
        XCTAssertNil(store.error)
        XCTAssertEqual(try Data(contentsOf: output), Data("original".utf8))
        func awaitReview() async throws -> ImportInspection {
            let deadline = Date().addingTimeInterval(15)
            while store.importInspection == nil && Date() < deadline { try await Task.sleep(for: .milliseconds(20)) }
            return try XCTUnwrap(store.importInspection, store.error ?? "未出现导出冲突")
        }
        try Data("keep".utf8).write(to: output)
        let overwrite = Task { await store.exportPaths(["source.txt"], to: output) }
        let batch = try await awaitReview()
        XCTAssertEqual(store.conflictOperation, "导出")
        XCTAssertFalse(try XCTUnwrap(batch.conflicts.first).sameContent)
        store.completeImportReview([ImportResolution(target: try XCTUnwrap(batch.conflicts.first).target, action: .overwrite)], id: batch.id)
        await overwrite.value
        XCTAssertNil(store.error)
        XCTAssertEqual(try Data(contentsOf: output), Data("original".utf8))
        let cancelling = Task { await store.exportPaths(["source.txt"], to: output) }
        _ = try await awaitReview()
        await store.lock()
        await cancelling.value
        XCTAssertNil(store.importInspection)
        XCTAssertNil(store.container)
        XCTAssertEqual(try Data(contentsOf: output), Data("original".utf8))
    }

    func testImportReviewRequiresChoicesAndLockCancelsPendingReview() async throws {
        guard ProcessInfo.processInfo.environment["VEIL_APP_DATA_DIR"] != nil,
              ProcessInfo.processInfo.environment["VEIL_NATIVE_TESTS"] == "1" else {
            throw XCTSkip("冲突集成测试需要独立测试目录")
        }
        let store = AppStore()
        await store.load()
        let space = try XCTUnwrap(store.activeSpace)
        await store.submit(.create(space: space.id), name: "冲突测试 " + UUID().uuidString,
                           password: "review-password", confirmation: "review-password")
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let file = root.appendingPathComponent("同名.txt")
        try Data("original".utf8).write(to: file)
        await store.importFiles([file])
        XCTAssertNil(store.error)
        func awaitReview() async throws -> ImportInspection {
            let deadline = Date().addingTimeInterval(15)
            while store.importInspection == nil && Date() < deadline {
                try await Task.sleep(for: .milliseconds(20))
            }
            return try XCTUnwrap(store.importInspection, store.error ?? "未出现冲突选择")
        }
        let skipping = Task { await store.importFiles([file]) }
        let same = try await awaitReview()
        XCTAssertTrue(try XCTUnwrap(same.conflicts.first).sameContent)
        XCTAssertEqual(store.container?.files.first?.version, 1)
        store.completeImportReview([ImportResolution(target: "同名.txt", action: .skip)])
        await skipping.value
        XCTAssertNil(store.error)
        XCTAssertEqual(store.container?.files.first?.version, 1)
        try Data("changed".utf8).write(to: file)
        let overwriting = Task { await store.importFiles([file]) }
        let different = try await awaitReview()
        XCTAssertFalse(try XCTUnwrap(different.conflicts.first).sameContent)
        store.completeImportReview([ImportResolution(target: "同名.txt", action: .overwrite)])
        await overwriting.value
        XCTAssertNil(store.error)
        XCTAssertEqual(store.container?.files.first?.version, 2)
        let cancelling = Task { await store.importFiles([file]) }
        _ = try await awaitReview()
        await store.lock()
        await cancelling.value
        XCTAssertNil(store.importInspection)
        XCTAssertNil(store.container)
        XCTAssertFalse(store.isBusy)
    }

    func testSwiftCallsRealRustService() async throws {
        guard ProcessInfo.processInfo.environment["VEIL_APP_DATA_DIR"] != nil,
              ProcessInfo.processInfo.environment["VEIL_NATIVE_TESTS"] == "1" else {
            throw XCTSkip("集成测试需要独立的 VEIL_APP_DATA_DIR 和 VEIL_NATIVE_TESTS=1")
        }
        let client = ServiceClient()
        try await client.execute(NativeRequest(operation: "set_import_commit_interval", seconds: 60))
        let interval: Int = try await client.json(NativeRequest(operation: "get_preference", key: "import_commit_interval_seconds"))
        XCTAssertEqual(interval, 60)
        try await client.execute(NativeRequest(operation: "set_import_commit_count", count: 2))
        let commitCount: Int = try await client.json(NativeRequest(operation: "get_preference", key: "import_commit_count"))
        XCTAssertEqual(commitCount, 2)
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
        try await client.execute(NativeRequest(operation: "set_import_commit_interval", seconds: 1))
        try await client.execute(NativeRequest(operation: "set_import_commit_count", count: 8))
        XCTAssertEqual(updated.files.count, 1)
        XCTAssertEqual(updated.files.first?.category, .document)
        let importProgress = try XCTUnwrap(client.transferProgress())
        XCTAssertEqual(importProgress.phase, "completed")
        XCTAssertEqual(importProgress.pendingFiles, 0)
        XCTAssertEqual(importProgress.processedBytes, UInt64(content.count))
        XCTAssertFalse(client.cancelTransfer())
        let retried: ContainerSnapshot = try await client.json(NativeRequest(operation: "import_files", sources: [source.path], directory: ""))
        XCTAssertEqual(retried.files.count, 1)
        XCTAssertEqual(client.transferProgress()?.skippedFiles, 1)
        let exported = root.appendingPathComponent("exported.txt")
        try await client.execute(NativeRequest(operation: "export_file", name: "说明.txt", destination: exported.path))
        let exportProgress = try XCTUnwrap(client.transferProgress())
        XCTAssertEqual(exportProgress.operation, "export")
        XCTAssertEqual(exportProgress.phase, "completed")
        XCTAssertEqual(exportProgress.processedBytes, UInt64(content.count))
        XCTAssertEqual(try Data(contentsOf: exported), content)
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
