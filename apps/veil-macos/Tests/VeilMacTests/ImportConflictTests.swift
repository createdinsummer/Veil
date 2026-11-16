/// 验证导入冲突复核结果会按用户选择生成安全的逐项决策。
/// 所属模块：macOS 界面模型测试；覆盖跳过、覆盖限制和空批次的行为。
/// 核心类型 ImportInspectionResolutionTests 使用构造的冲突快照；依赖 XCTest 和 VeilMac 模型，不访问数据库或真实文件。
/// 目标是证明决策映射保持目标顺序并对不可覆盖项退回跳过，不验证服务执行阶段的冲突重检。
import XCTest
@testable import VeilMac

/// 检查冲突列表到导入决策的转换规则。
final class ImportInspectionResolutionTests: XCTestCase {
    /// 构造一项目标冲突快照，用于验证界面决策映射。
    /// - Parameters: target: String 模拟目标路径；directory: Bool 是否为目录，默认 false；error: String? 检查错误，默认 nil 表示可检查。
    /// - Returns: ImportConflict，保留输入并固定 sameContent 为 false，不访问真实目标。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 仅创建样本，不验证路径合法性，也不模拟真实文件内容比较。
    private func conflict(_ target: String, directory: Bool = false, error: String? = nil)
        -> ImportConflict
    {
        ImportConflict(target: target, sameContent: false, isDirectory: directory, error: error)
    }

    /// 将模拟冲突集合组成一批待用户复核的数据。
    /// - Parameters: conflicts: [ImportConflict]，每项代表一个模拟目标冲突，顺序用于验证结果保持原顺序，允许为空。
    /// - Returns: ImportInspection，批次 ID 固定为 batch，totalFiles 取冲突数组长度。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 此处 totalFiles 等于冲突数是测试简化，真实任务总文件数可大于冲突数；固定 ID 不表示生产批次标识生成规则。
    private func inspection(_ conflicts: [ImportConflict]) -> ImportInspection {
        ImportInspection(id: "batch", totalFiles: conflicts.count, conflicts: conflicts)
    }

    /// 验证用户选择跳过时，批次中的每条冲突都会被保留并标记为跳过。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；规则不符合预期时通过 XCTest 断言登记失败。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 断言只覆盖本用例列出的输入和环境，不能替代完整界面或系统行为验证。
    func testSkipResolvesEveryConflictToSkip() {
        let resolutions = inspection([conflict("a.txt"), conflict("dir", directory: true)])
            .resolutions(for: .skip)
        XCTAssertEqual(resolutions.map(\.target), ["a.txt", "dir"])
        XCTAssertTrue(resolutions.allSatisfy { $0.action == .skip })
    }

    /// 验证普通文件冲突可以覆盖，但目录冲突或无法检查的冲突会安全退回跳过。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；规则不符合预期时通过 XCTest 断言登记失败。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 断言只覆盖本用例列出的输入和环境，不能替代完整界面或系统行为验证。
    func testOverwriteFallsBackToSkipWhenConflictCannotBeOverwritten() {
        let resolutions = inspection([
            conflict("a.txt"),
            conflict("dir", directory: true),
            conflict("bad.txt", error: "无法读取"),
        ]).resolutions(for: .overwrite)

        XCTAssertEqual(resolutions.map(\.target), ["a.txt", "dir", "bad.txt"])
        XCTAssertEqual(resolutions.map(\.action), [.overwrite, .skip, .skip])
    }

    /// 验证没有冲突的导入批次不会生成多余决策记录。
    /// - Parameters: 无；由 XCTest 调用。
    /// - Returns: Void；规则不符合预期时通过 XCTest 断言登记失败。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 断言只覆盖本用例列出的输入和环境，不能替代完整界面或系统行为验证。
    func testEmptyBatchProducesNoResolutions() {
        XCTAssertTrue(inspection([]).resolutions(for: .skip).isEmpty)
    }
}
