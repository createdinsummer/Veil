/// 验证扩展名默认应用规则与解密临时目录的清理策略。
/// 所属模块：macOS 打开方式 / 临时文件测试；覆盖字符串规则、应用设置读写和明文目录清理。
/// 核心类型：规则、清扫、进程登记及目录权限等 XCTestCase；依赖 Foundation 文件系统、XCTest 和 VeilMac 模块。
/// 多数清理用例使用隔离目录；TemporaryDirectoryTests 会访问实际 Veil 临时根目录，集成用例依赖显式配置的测试应用数据目录。
import XCTest
@testable import VeilMac

/// 检查扩展名规范化、排序和用户输入校验。
final class OpenWithRulesTests: XCTestCase {
    /// 验证文件名中的大小写扩展名会归一化，复合后缀按最后一段识别，无后缀或非法字符会被拒绝。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；规则不符合预期时通过 XCTest 断言登记失败。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 断言只覆盖本用例列出的输入和环境，不能替代完整界面或系统行为验证。
    func testFileExtensionIsNormalized() {
        XCTAssertEqual(OpenWithRules.normalizeExtension(forFileName: "movie.MP4"), "mp4")
        XCTAssertEqual(OpenWithRules.normalizeExtension(forFileName: "archive.tar.gz"), "gz")
        XCTAssertEqual(OpenWithRules.normalizeExtension(forFileName: "封面.PNG"), "png")
        XCTAssertNil(OpenWithRules.normalizeExtension(forFileName: "README"))
        XCTAssertNil(OpenWithRules.normalizeExtension(forFileName: "a."))
        XCTAssertNil(OpenWithRules.normalizeExtension(forFileName: "file.bad!"))
    }

    /// 验证带点号、空白或大小写变化的常见后缀写法能转换成统一形式。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；规则不符合预期时通过 XCTest 断言登记失败。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 断言只覆盖本用例列出的输入和环境，不能替代完整界面或系统行为验证。
    func testUserInputAcceptsCommonSpellings() {
        XCTAssertEqual(OpenWithRules.normalizeUserInput(" MP4 "), "mp4")
        XCTAssertEqual(OpenWithRules.normalizeUserInput(".PDF"), "pdf")
        XCTAssertEqual(OpenWithRules.normalizeUserInput("7z"), "7z")
        XCTAssertEqual(OpenWithRules.normalizeUserInput("..Zip"), "zip")
        XCTAssertEqual(OpenWithRules.normalizeUserInput(String(repeating: "a", count: 16))?.count, 16)
    }

    /// 验证空值、路径片段和不允许字符不会被登记为默认打开后缀。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；规则不符合预期时通过 XCTest 断言登记失败。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 断言只覆盖本用例列出的输入和环境，不能替代完整界面或系统行为验证。
    func testUserInputRejectsInvalidExtensions() {
        XCTAssertNil(OpenWithRules.normalizeUserInput(""))
        XCTAssertNil(OpenWithRules.normalizeUserInput("."))
        XCTAssertNil(OpenWithRules.normalizeUserInput("a.b"))
        XCTAssertNil(OpenWithRules.normalizeUserInput("中文"))
        XCTAssertNil(OpenWithRules.normalizeUserInput("mp4 file"))
        XCTAssertNil(OpenWithRules.normalizeUserInput(String(repeating: "a", count: 17)))
    }

    /// 验证默认打开后缀按稳定顺序提供给设置界面。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；规则不符合预期时通过 XCTest 断言登记失败。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 断言只覆盖本用例列出的输入和环境，不能替代完整界面或系统行为验证。
    func testSortedExtensions() {
        XCTAssertEqual(
            OpenWithRules.sortedExtensions(["mp4": "a", "7z": "b", "pdf": "c"]),
            ["7z", "mp4", "pdf"])
        XCTAssertEqual(OpenWithRules.sortedExtensions([:]), [])
    }
}

/// 验证旧版临时目录的命名、年龄和目录类型过滤，使用自建根目录作为输入。
final class TemporaryFilesTests: XCTestCase {
    /// 为临时清理用例创建隔离的测试根目录。
    /// - Parameters: 无。
    /// - Returns: URL，系统临时目录下带 veil-sweep 前缀和随机 UUID 的新目录。
    /// - Throws: 创建目录失败时传播 Foundation 文件系统错误。
    /// - 注意事项: 不创建缺失的父目录；调用方负责在测试结束后删除根目录及其剩余内容。
    private func makeRoot() throws -> URL {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("veil-sweep-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        return root
    }

    @discardableResult
    /// 创建一个具有指定修改时间的清扫候选目录。
    /// - Parameters: root: URL 已存在的测试根目录；name: String 子目录名，由测试提供；modified: Date 设置的目录修改时间，用于模拟新旧副本。
    /// - Returns: URL，新建子目录的位置。
    /// - Throws: 目录创建或修改时间写入失败时传播文件系统错误。
    /// - 注意事项: 不验证 name 是否含路径分隔符；仅使用可信测试名称。设置属性失败时目录可能已创建，调用方清理整个 root。
    private func makeDirectory(in root: URL, _ name: String, modified: Date) throws -> URL {
        let url = root.appendingPathComponent(name, isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: false)
        try FileManager.default.setAttributes([.modificationDate: modified], ofItemAtPath: url.path)
        return url
    }

    /// 验证清理器只移除已过期的预览 / 外部打开目录，保留仍有效的临时数据。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；规则不符合预期时通过 XCTest 断言登记失败。
    /// - Throws: 测试目录创建、写入或属性读取失败时传播文件系统错误。
    /// - 注意事项: 断言只覆盖本用例列出的输入和环境，不能替代完整界面或系统行为验证。
    func testSweepRemovesOnlyStalePreviewAndOpenDirectories() throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let now = Date()
        let stale = now.addingTimeInterval(-72 * 3600)
        let stalePreview = try makeDirectory(
            in: root, "veil-native-preview-\(UUID().uuidString)", modified: stale)
        let staleOpen = try makeDirectory(in: root, "veil-open-\(UUID().uuidString)", modified: stale)
        let freshOpen = try makeDirectory(in: root, "veil-open-\(UUID().uuidString)", modified: now)
        let unrelated = try makeDirectory(in: root, "other-\(UUID().uuidString)", modified: stale)

        let removed = TemporaryFiles.sweep(root: root, olderThan: 48 * 3600, now: now)

        XCTAssertEqual(
            Set(removed),
            [stalePreview.lastPathComponent, staleOpen.lastPathComponent])
        XCTAssertFalse(FileManager.default.fileExists(atPath: stalePreview.path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: staleOpen.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: freshOpen.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: unrelated.path))
    }

    /// 验证清理不会删除根目录中的普通文件，目录不存在时也能安全结束。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；规则不符合预期时通过 XCTest 断言登记失败。
    /// - Throws: 测试目录创建、写入或属性读取失败时传播文件系统错误。
    /// - 注意事项: 断言只覆盖本用例列出的输入和环境，不能替代完整界面或系统行为验证。
    func testSweepIgnoresFilesAndMissingRoot() throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let stale = Date().addingTimeInterval(-72 * 3600)
        let file = root.appendingPathComponent("veil-open-\(UUID().uuidString)")
        try Data("not a directory".utf8).write(to: file)
        try FileManager.default.setAttributes([.modificationDate: stale], ofItemAtPath: file.path)

        XCTAssertEqual(TemporaryFiles.sweep(root: root, olderThan: 48 * 3600), [])
        XCTAssertTrue(FileManager.default.fileExists(atPath: file.path))
        XCTAssertEqual(
            TemporaryFiles.sweep(root: URL(fileURLWithPath: "/nonexistent/veil-sweep-\(UUID().uuidString)")),
            [])
    }
}

/// 验证默认打开应用配置在本机数据库中的保存、重载和删除。
@MainActor
final class OpenWithIntegrationTests: XCTestCase {
    /// 验证后缀与默认应用绑定写入本机数据库后可正确读取。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；规则不符合预期时通过 XCTest 断言登记失败。
    /// - Throws: 环境不满足时抛出 XCTSkip 跳过；业务失败通过 Store 状态和断言体现。
    /// - 注意事项: 需 VEIL_NATIVE_TESTS=1 且设置独立 VEIL_APP_DATA_DIR；测试会修改该目录的应用设置，初始规则必须为空，不验证真实应用启动。
    func testDefaultOpenApplicationRoundTripsThroughAppDatabase() async throws {
        guard ProcessInfo.processInfo.environment["VEIL_APP_DATA_DIR"] != nil,
            ProcessInfo.processInfo.environment["VEIL_NATIVE_TESTS"] == "1"
        else {
            throw XCTSkip("打开方式集成测试需要独立的测试目录")
        }
        let application = ExternalApplication(
            bundleID: "com.apple.QuickTimePlayerX",
            url: URL(fileURLWithPath: "/System/Applications/QuickTime Player.app"))

        let store = AppStore()
        await store.load()
        XCTAssertEqual(store.openWithDefaults, [:])

        await store.setDefaultOpenApplication(application, forExtension: ".MP4")
        XCTAssertNil(store.error)
        XCTAssertEqual(store.openWithDefaults["mp4"], application.bundleID)

        let reopened = AppStore()
        await reopened.load()
        XCTAssertEqual(reopened.openWithDefaults["mp4"], application.bundleID)

        await reopened.removeDefaultOpenApplication(forExtension: "mp4")
        XCTAssertNil(reopened.error)
        XCTAssertNil(reopened.openWithDefaults["mp4"])

        let cleared = AppStore()
        await cleared.load()
        XCTAssertEqual(cleared.openWithDefaults, [:])
    }
}

/// 使用模拟进程 ID 直接验证登记、关联和清理的数据关系，不启动真实应用或发送退出通知。
@MainActor
final class ExternalOpenTrackerTests: XCTestCase {
    /// 为外部应用登记表测试创建独立的模拟明文目录。
    /// - Parameters: 无。
    /// - Returns: URL，系统临时目录下带随机 UUID 的新建目录，名称以 veil-open-tracker- 开头。
    /// - Throws: 目录创建失败时传播文件系统错误。
    /// - 注意事项: 主 Actor 测试调用；只创建空目录，不实际解密文件，清理由后续登记表操作承担。
    private func makeFolder() throws -> URL {
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("veil-open-tracker-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false)
        return folder
    }

    /// 验证登记给应用的临时目录会在对应应用结束后清理。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；规则不符合预期时通过 XCTest 断言登记失败。
    /// - Throws: 测试目录创建、写入或属性读取失败时传播文件系统错误。
    /// - 注意事项: 测试直接调用清理方法并使用模拟进程号，不启动真实外部应用，也不验证系统退出通知的送达。
    func testFoldersAreRemovedWhenTrackedApplicationTerminates() throws {
        let first = try makeFolder()
        let second = try makeFolder()
        let process: pid_t = 987_001
        ExternalOpenTracker.shared.register(first)
        ExternalOpenTracker.shared.register(second)
        ExternalOpenTracker.shared.associate(folder: first, process: process)
        ExternalOpenTracker.shared.associate(folder: second, process: process)

        ExternalOpenTracker.shared.cleanup(process: process)

        XCTAssertFalse(FileManager.default.fileExists(atPath: first.path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: second.path))
    }

    /// 验证清理一个应用的临时文件时不会影响其他仍在使用的应用目录。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；规则不符合预期时通过 XCTest 断言登记失败。
    /// - Throws: 测试目录创建、写入或属性读取失败时传播文件系统错误。
    /// - 注意事项: 测试直接调用清理方法并使用模拟进程号，不启动真实外部应用，也不验证系统退出通知的送达。
    func testOtherApplicationsKeepTheirFolders() throws {
        let kept = try makeFolder()
        let removed = try makeFolder()
        ExternalOpenTracker.shared.associate(folder: kept, process: 987_002)
        ExternalOpenTracker.shared.associate(folder: removed, process: 987_003)

        ExternalOpenTracker.shared.cleanup(process: 987_003)

        XCTAssertTrue(FileManager.default.fileExists(atPath: kept.path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: removed.path))
        ExternalOpenTracker.shared.cleanupAll()
        XCTAssertFalse(FileManager.default.fileExists(atPath: kept.path))
    }

    /// 验证未完成交接或已放弃的临时目录会被回收。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；规则不符合预期时通过 XCTest 断言登记失败。
    /// - Throws: 测试目录创建、写入或属性读取失败时传播文件系统错误。
    /// - 注意事项: 断言只覆盖本用例列出的输入和环境，不能替代完整界面或系统行为验证。
    func testPendingAndDiscardedFoldersAreRemoved() throws {
        let pending = try makeFolder()
        let discarded = try makeFolder()
        ExternalOpenTracker.shared.register(pending)
        ExternalOpenTracker.shared.register(discarded)

        ExternalOpenTracker.shared.discard(discarded)
        XCTAssertFalse(FileManager.default.fileExists(atPath: discarded.path))

        ExternalOpenTracker.shared.cleanupAll()
        XCTAssertFalse(FileManager.default.fileExists(atPath: pending.path))
    }
}

/// 验证应用专用临时目录的位置、权限和重复初始化行为。
final class TemporaryDirectoryTests: XCTestCase {
    /// 验证临时目录位于系统临时区域且权限仅允许当前用户访问。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；规则不符合预期时通过 XCTest 断言登记失败。
    /// - Throws: 测试目录创建、写入或属性读取失败时传播文件系统错误。
    /// - 注意事项: 断言只覆盖本用例列出的输入和环境，不能替代完整界面或系统行为验证。
    func testTemporaryDirectorySitsUnderSystemTempWithUserOnlyPermissions() throws {
        XCTAssertEqual(TemporaryFiles.directory.lastPathComponent, "Veil")
        XCTAssertEqual(
            TemporaryFiles.directory.deletingLastPathComponent().standardizedFileURL,
            FileManager.default.temporaryDirectory.standardizedFileURL)

        try TemporaryFiles.ensureDirectory()
        try TemporaryFiles.ensureDirectory()
        let attributes = try FileManager.default.attributesOfItem(
            atPath: TemporaryFiles.directory.path)
        XCTAssertEqual(attributes[.posixPermissions] as? Int, 0o700)
        var isDirectory: ObjCBool = false
        XCTAssertTrue(
            FileManager.default.fileExists(
                atPath: TemporaryFiles.directory.path, isDirectory: &isDirectory))
        XCTAssertTrue(isDirectory.boolValue)
    }
}

/// 验证清理操作只删除 Veil 专用临时数据和明确标记的旧版残留。
final class TemporaryPurgeTests: XCTestCase {
    /// 为临时清理用例创建隔离的测试根目录。
    /// - Parameters: 无。
    /// - Returns: URL，系统临时目录下带 veil-purge 前缀和随机 UUID 的新目录。
    /// - Throws: 创建目录失败时传播 Foundation 文件系统错误。
    /// - 注意事项: 不创建缺失的父目录；调用方负责在测试结束后删除根目录及其剩余内容。
    private func makeRoot() throws -> URL {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("veil-purge-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        return root
    }

    /// 验证专用 root 内所有项（含无前缀目录和普通文件）都删除，而旧版根目录只删匹配前缀的目录。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；规则不符合预期时通过 XCTest 断言登记失败。
    /// - Throws: 测试目录创建、写入或属性读取失败时传播文件系统错误。
    /// - 注意事项: 断言只覆盖本用例列出的输入和环境，不能替代完整界面或系统行为验证。
    func testPurgeClearsVeilRootAndLegacyResidueOnly() throws {
        let root = try makeRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let legacy = try makeRoot()
        defer { try? FileManager.default.removeItem(at: legacy) }

        try FileManager.default.createDirectory(
            at: root.appendingPathComponent("veil-open-\(UUID().uuidString)"),
            withIntermediateDirectories: false)
        try FileManager.default.createDirectory(
            at: root.appendingPathComponent("unrelated"), withIntermediateDirectories: false)
        try Data("plaintext".utf8).write(to: root.appendingPathComponent("stray-file"))

        let legacyVeil = legacy.appendingPathComponent(
            "veil-native-preview-\(UUID().uuidString)", isDirectory: true)
        let legacyOther = legacy.appendingPathComponent("other-app", isDirectory: true)
        try FileManager.default.createDirectory(at: legacyVeil, withIntermediateDirectories: false)
        try FileManager.default.createDirectory(at: legacyOther, withIntermediateDirectories: false)

        let removed = TemporaryFiles.purge(root: root, legacyRoot: legacy)

        XCTAssertEqual(removed, 4)
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: root.path), [])
        XCTAssertFalse(FileManager.default.fileExists(atPath: legacyVeil.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: legacyOther.path))
    }

    /// 验证 Veil 临时根目录不存在时，清理操作作为无操作成功结束。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；规则不符合预期时通过 XCTest 断言登记失败。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 断言只覆盖本用例列出的输入和环境，不能替代完整界面或系统行为验证。
    func testPurgeMissingRootIsNoOp() {
        let missing = FileManager.default.temporaryDirectory
            .appendingPathComponent("veil-missing-\(UUID().uuidString)", isDirectory: true)
        XCTAssertEqual(TemporaryFiles.purge(root: missing), 0)
    }
}
