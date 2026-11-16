/// ============================================================================
/// Veil iOS 客户端 · 导入 / 导出重名冲突裁决页（ImportConflictReview）
///
/// 文件作用：传输（导入 / 导出）过程中遇到目标位置已有同名条目时弹出的裁决界面，
/// 让用户逐项选择“覆盖”或“跳过”，再把这些决定回传给 Store 里挂起的传输任务。
/// 所属模块：iOS 客户端 UI 层（导入 / 导出流程的一个中间交互步骤）。
/// 文件内核心成员清单：
///   - ImportConflictReview 冲突裁决视图本体（列表 + 批量按钮 + 提交 / 中止）
/// 依赖能力：SwiftUI（界面）、Store 传入的冲突批次与回调闭包。
/// 整体业务目标：把服务端暂停移交的冲突批次变成用户可理解的选择项，
///   用户确认后由 Store 继续传输；用户中止则由调用方请求取消，已经完成的文件不会因此自动回滚。
/// ============================================================================
import SwiftUI

/// 冲突裁决页：传输过程中遇到重名条目时弹出，要求用户逐项选择覆盖 / 跳过。
/// 用户提交或中止后，把裁决结果经 completion 回传给 Store 挂起的导入 / 导出任务。
struct ImportConflictReview: View {
    /// 服务端发来的冲突批次（批次 id + 冲突列表）。
    let inspection: ImportInspection
    /// 本次操作名称（“导入”或“导出”），用于按钮与提示文案。
    var operationLabel: String = "导入"
    /// 顶部进度说明文字（来自 Store.importProgressDescription），帮助用户判断是否继续。
    var progressText: String? = nil
    /// 裁决结果回调：传 nil 表示中止整次操作。
    let completion: ([ImportResolution]?) -> Void
    /// 当前批次选择映射：键为服务返回的 target（导入目的路径或导出目标），值为覆盖 / 跳过。
    /// 同一 target 共用一个选择；集合只供生成提交结果，本身不执行文件写入。
    @State private var choices: [String: ImportAction] = [:]

    /// 每轮检查一项冲突是否有路径对应的决定；空批次为 true，重复 target 共用选择。
    private var complete: Bool { inspection.conflicts.allSatisfy { choices[$0.target] != nil } }
    /// 组装当前批次的列表、批量填选和提交按钮；completion 只回传决定，调用方负责恢复或取消任务。
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("处理同名文件").font(.title2.bold())
            Text("本批需处理 \(inspection.conflicts.count) 项。其他文件继续\(operationLabel)，内容相同的文件也由你选择。")
                .font(.subheadline).foregroundStyle(.secondary)
            // 进度说明：展示已传输进度，帮助用户判断是否值得继续。
            if let progressText { Text(progressText).font(.caption).monospacedDigit().foregroundStyle(.secondary) }
            HStack {
                // 批量操作一：本批全部跳过（对所有冲突写入“跳过”）。
                Button("本批全部跳过") { for conflict in inspection.conflicts { choices[conflict.target] = .skip } }
                // 每轮为一项可覆盖冲突填写 overwrite；其他条目保持已有选择，未选项仍需手动决定，不立即提交。
                Button("本批覆盖文件") { for conflict in inspection.conflicts where conflict.canOverwrite { choices[conflict.target] = .overwrite } }
                    .disabled(!inspection.conflicts.contains { $0.canOverwrite })
            }
            // 冲突列表：每条展示路径、冲突说明与“处理方式”选择器。
            List(inspection.conflicts) { conflict in
                VStack(alignment: .leading, spacing: 8) {
                    Text(conflict.target).font(.headline).textSelection(.enabled)
                    Text(conflict.detail).font(.caption).foregroundStyle(.secondary)
                    // 菜单式选择器：未选 / 跳过 / 覆盖（文件夹冲突不可覆盖）。
                    Picker("处理方式", selection: Binding<ImportAction?>(
                        get: { choices[conflict.target] },
                        set: { choices[conflict.target] = $0 }
                    )) {
                        Text("请选择").tag(Optional<ImportAction>.none)
                        Text("跳过").tag(Optional(ImportAction.skip))
                        if conflict.canOverwrite { Text("覆盖").tag(Optional(ImportAction.overwrite)) }
                    }
                    .pickerStyle(.menu)
                }.padding(.vertical, 6)
            }
            HStack {
                // 已选择进度提示。
                Text("已选择 \(choices.count) / \(inspection.conflicts.count)").font(.caption).foregroundStyle(.secondary)
                Spacer()
                // 向调用方请求中止（传 nil）；这里不撤销已成功传输的文件，也不直接控制后台工作线程。
                Button("中止\(operationLabel)", role: .cancel) { completion(nil) }
                // 应用选择：全部选择完成才可提交；按冲突列表顺序生成裁决结果数组。
                Button("应用选择") {
                    guard complete else { return }
                    completion(inspection.conflicts.compactMap { conflict in
                        choices[conflict.target].map { ImportResolution(target: conflict.target, action: $0) }
                    })
                }.disabled(!complete)
            }
        }
        .padding(20)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
