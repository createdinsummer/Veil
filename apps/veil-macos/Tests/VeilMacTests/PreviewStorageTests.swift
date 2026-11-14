import Darwin
import XCTest
@testable import VeilMac

final class PreviewStorageTests: XCTestCase {
    func testSufficientSpacePasses() {
        XCTAssertNil(
            PreviewStorage.insufficientSpaceMessage(
                fileSize: 1_000_000, availableBytes: 10_000_000_000))
    }

    func testSpaceEqualToRequirementPassesAndOneByteLessFails() {
        let size: UInt64 = 700 * 1024 * 1024
        let required = PreviewStorage.requiredBytes(for: size)
        XCTAssertEqual(required, size + PreviewStorage.reserveBytes)
        XCTAssertNil(
            PreviewStorage.insufficientSpaceMessage(fileSize: size, availableBytes: required))

        let message = PreviewStorage.insufficientSpaceMessage(
            fileSize: size, availableBytes: required - 1)
        XCTAssertNotNil(message)
        XCTAssertTrue(message?.contains("磁盘空间不足") == true)
        XCTAssertTrue(message?.contains("临时空间") == true)
        XCTAssertTrue(message?.contains("导出") == true)
    }

    func testUnknownCapacitySkipsPrecheck() {
        XCTAssertNil(
            PreviewStorage.insufficientSpaceMessage(fileSize: UInt64.max, availableBytes: nil))
    }

    func testHugeFileSaturatesWithoutOverflow() {
        XCTAssertEqual(PreviewStorage.requiredBytes(for: UInt64.max), UInt64.max)
        XCTAssertNotNil(
            PreviewStorage.insufficientSpaceMessage(fileSize: UInt64.max, availableBytes: 1_000_000))
    }

    func testUnreadableDirectoryHasNoCapacity() {
        XCTAssertNil(
            PreviewStorage.availableBytes(in: URL(fileURLWithPath: "/nonexistent/veil-preview")))
        XCTAssertNotNil(
            PreviewStorage.availableBytes(in: FileManager.default.temporaryDirectory))
    }

    func testStorageFullErrorsBecomeFriendlyMessage() {
        let serviceError = ClientError.service("I/O 错误: No space left on device (os error 28)")
        XCTAssertTrue(
            PreviewStorage.failureMessage(for: serviceError, fileSize: 2 * 1024 * 1024 * 1024)
                .contains("磁盘空间不足"))

        let posix = NSError(domain: NSPOSIXErrorDomain, code: Int(ENOSPC))
        XCTAssertTrue(
            PreviewStorage.failureMessage(for: posix, fileSize: 1).contains("磁盘空间不足"))

        let cocoa = NSError(domain: NSCocoaErrorDomain, code: NSFileWriteOutOfSpaceError)
        XCTAssertTrue(
            PreviewStorage.failureMessage(for: cocoa, fileSize: 1).contains("磁盘空间不足"))
    }

    func testOtherFailuresKeepOriginalDescription() {
        XCTAssertEqual(
            PreviewStorage.failureMessage(
                for: ClientError.service("文件超过预览大小限制"), fileSize: 1),
            "文件超过预览大小限制")
    }
}
