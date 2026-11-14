import XCTest
@testable import VeilMac

final class OpenWithRulesTests: XCTestCase {
    func testFileExtensionIsNormalized() {
        XCTAssertEqual(OpenWithRules.normalizeExtension(forFileName: "movie.MP4"), "mp4")
        XCTAssertEqual(OpenWithRules.normalizeExtension(forFileName: "archive.tar.gz"), "gz")
        XCTAssertEqual(OpenWithRules.normalizeExtension(forFileName: "封面.PNG"), "png")
        XCTAssertNil(OpenWithRules.normalizeExtension(forFileName: "README"))
        XCTAssertNil(OpenWithRules.normalizeExtension(forFileName: "a."))
        XCTAssertNil(OpenWithRules.normalizeExtension(forFileName: "file.bad!"))
    }

    func testUserInputAcceptsCommonSpellings() {
        XCTAssertEqual(OpenWithRules.normalizeUserInput(" MP4 "), "mp4")
        XCTAssertEqual(OpenWithRules.normalizeUserInput(".PDF"), "pdf")
        XCTAssertEqual(OpenWithRules.normalizeUserInput("7z"), "7z")
        XCTAssertEqual(OpenWithRules.normalizeUserInput("..Zip"), "zip")
        XCTAssertEqual(OpenWithRules.normalizeUserInput(String(repeating: "a", count: 16))?.count, 16)
    }

    func testUserInputRejectsInvalidExtensions() {
        XCTAssertNil(OpenWithRules.normalizeUserInput(""))
        XCTAssertNil(OpenWithRules.normalizeUserInput("."))
        XCTAssertNil(OpenWithRules.normalizeUserInput("a.b"))
        XCTAssertNil(OpenWithRules.normalizeUserInput("中文"))
        XCTAssertNil(OpenWithRules.normalizeUserInput("mp4 file"))
        XCTAssertNil(OpenWithRules.normalizeUserInput(String(repeating: "a", count: 17)))
    }

    func testSortedExtensions() {
        XCTAssertEqual(
            OpenWithRules.sortedExtensions(["mp4": "a", "7z": "b", "pdf": "c"]),
            ["7z", "mp4", "pdf"])
        XCTAssertEqual(OpenWithRules.sortedExtensions([:]), [])
    }
}

final class TemporaryFilesTests: XCTestCase {
    private func makeRoot() throws -> URL {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("veil-sweep-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        return root
    }

    @discardableResult
    private func makeDirectory(in root: URL, _ name: String, modified: Date) throws -> URL {
        let url = root.appendingPathComponent(name, isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: false)
        try FileManager.default.setAttributes([.modificationDate: modified], ofItemAtPath: url.path)
        return url
    }

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

@MainActor
final class OpenWithIntegrationTests: XCTestCase {
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

@MainActor
final class ExternalOpenTrackerTests: XCTestCase {
    private func makeFolder() throws -> URL {
        let folder = FileManager.default.temporaryDirectory
            .appendingPathComponent("veil-open-tracker-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false)
        return folder
    }

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

final class TemporaryDirectoryTests: XCTestCase {
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

final class TemporaryPurgeTests: XCTestCase {
    private func makeRoot() throws -> URL {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("veil-purge-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        return root
    }

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

    func testPurgeMissingRootIsNoOp() {
        let missing = FileManager.default.temporaryDirectory
            .appendingPathComponent("veil-missing-\(UUID().uuidString)", isDirectory: true)
        XCTAssertEqual(TemporaryFiles.purge(root: missing), 0)
    }
}
