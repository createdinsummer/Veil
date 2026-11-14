import XCTest
@testable import VeilMac

final class ImportInspectionResolutionTests: XCTestCase {
    private func conflict(_ target: String, directory: Bool = false, error: String? = nil)
        -> ImportConflict
    {
        ImportConflict(target: target, sameContent: false, isDirectory: directory, error: error)
    }

    private func inspection(_ conflicts: [ImportConflict]) -> ImportInspection {
        ImportInspection(id: "batch", totalFiles: conflicts.count, conflicts: conflicts)
    }

    func testSkipResolvesEveryConflictToSkip() {
        let resolutions = inspection([conflict("a.txt"), conflict("dir", directory: true)])
            .resolutions(for: .skip)
        XCTAssertEqual(resolutions.map(\.target), ["a.txt", "dir"])
        XCTAssertTrue(resolutions.allSatisfy { $0.action == .skip })
    }

    func testOverwriteFallsBackToSkipWhenConflictCannotBeOverwritten() {
        let resolutions = inspection([
            conflict("a.txt"),
            conflict("dir", directory: true),
            conflict("bad.txt", error: "无法读取"),
        ]).resolutions(for: .overwrite)

        XCTAssertEqual(resolutions.map(\.target), ["a.txt", "dir", "bad.txt"])
        XCTAssertEqual(resolutions.map(\.action), [.overwrite, .skip, .skip])
    }

    func testEmptyBatchProducesNoResolutions() {
        XCTAssertTrue(inspection([]).resolutions(for: .skip).isEmpty)
    }
}
