/// 验证预览文件创建前的磁盘空间判断和系统错误提示转换。
/// 所属模块：macOS 预览存储测试；模拟空间不足、容量未知和底层文件系统错误。
/// 核心类型 PreviewStorageTests 依赖 XCTest、Darwin 错误码和 PreviewStorage；目标是验证预检计算与提示转换，不实际耗尽磁盘或写入预览文件。
import Darwin
import XCTest
@testable import VeilMac

/// 验证空间预检的算术边界和错误提示映射；除容量查询用例外，使用显式输入的模拟容量与错误。
final class PreviewStorageTests: XCTestCase {
    /// 验证可用空间明显大于预览需求时不返回空间不足提示。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；不符合预期时通过断言登记测试失败。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 仅验证本例提供的容量或错误输入，不保证后续文件写入一定成功，预检后磁盘空间仍可能变化。
    func testSufficientSpacePasses() {
        XCTAssertNil(
            PreviewStorage.insufficientSpaceMessage(
                fileSize: 1_000_000, availableBytes: 10_000_000_000))
    }

    /// 验证空间恰好足够时允许预览，少一个字节时给出不足提示。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；不符合预期时通过断言登记测试失败。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 仅验证本例提供的容量或错误输入，不保证后续文件写入一定成功，预检后磁盘空间仍可能变化。
    func testSpaceEqualToRequirementPassesAndOneByteLessFails() {
        // 模拟 700 MiB 文件；需求应为文件字节数加固定预留量，再检查阈值前后一个字节。
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

    /// 验证无法取得剩余容量时跳过预检，避免把未知误判为磁盘空间不足。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；不符合预期时通过断言登记测试失败。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 仅验证本例提供的容量或错误输入，不保证后续文件写入一定成功，预检后磁盘空间仍可能变化。
    func testUnknownCapacitySkipsPrecheck() {
        XCTAssertNil(
            PreviewStorage.insufficientSpaceMessage(fileSize: UInt64.max, availableBytes: nil))
    }

    /// 验证超大文件所需空间计算会饱和处理，不因整数溢出反而放行。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；不符合预期时通过断言登记测试失败。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 仅验证本例提供的容量或错误输入，不保证后续文件写入一定成功，预检后磁盘空间仍可能变化。
    func testHugeFileSaturatesWithoutOverflow() {
        XCTAssertEqual(PreviewStorage.requiredBytes(for: UInt64.max), UInt64.max)
        XCTAssertNotNil(
            PreviewStorage.insufficientSpaceMessage(fileSize: UInt64.max, availableBytes: 1_000_000))
    }

    /// 验证无法读取目录容量时按未知容量处理，而不是伪造可用空间。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；不符合预期时通过断言登记测试失败。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 仅查询不存在路径和当前系统临时目录；依赖测试运行环境允许读取容量，不验证真正的权限拒绝。
    func testUnreadableDirectoryHasNoCapacity() {
        XCTAssertNil(
            PreviewStorage.availableBytes(in: URL(fileURLWithPath: "/nonexistent/veil-preview")))
        XCTAssertNotNil(
            PreviewStorage.availableBytes(in: FileManager.default.temporaryDirectory))
    }

    /// 验证系统磁盘已满错误会转换为用户可理解的提示。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；不符合预期时通过断言登记测试失败。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 仅验证本例提供的容量或错误输入，不保证后续文件写入一定成功，预检后磁盘空间仍可能变化。
    func testStorageFullErrorsBecomeFriendlyMessage() {
        // 三种来源表示同一种磁盘已满情形：服务错误文本、POSIX 数值码和 Cocoa 文件写入错误码。
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

    /// 验证非空间不足错误保留原始说明，避免误导用户。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；不符合预期时通过断言登记测试失败。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 仅验证本例提供的容量或错误输入，不保证后续文件写入一定成功，预检后磁盘空间仍可能变化。
    func testOtherFailuresKeepOriginalDescription() {
        XCTAssertEqual(
            PreviewStorage.failureMessage(
                for: ClientError.service("文件超过预览大小限制"), fileSize: 1),
            "文件超过预览大小限制")
    }
}
