/// 同名文件冲突审核窗口。
/// 所属模块：导入 / 导出工作流的交互确认层；将服务端冲突检查结果展示给用户并回传逐项或批量决定。
/// 核心数据：inspection 包含冲突条目和可用解决方案；choices 按目标路径保存本批选择；completion 将结果交还执行任务。
/// 依赖 SwiftUI 表单和 ImportInspection / ImportReviewOutcome 模型。整体目标是让用户决定跳过、覆盖或中止，不让文件冲突默认覆盖。
import SwiftUI

/// 将服务端一批目标冲突转换为用户决定，不直接执行覆盖或取消任务。
/// choices 仅保存本视图按目标路径选择的动作；completion 把逐项决定和可选后续批次策略交回 Store。
struct ImportConflictReview: View {
    /// 服务层已检查出的冲突与批量解决方案。
    let inspection: ImportInspection
    /// 当前业务动作的显示名，用于按钮和说明文案。
    var operationLabel: String = "导入"
    /// 可选的批次进度说明。
    var progressText: String? = nil
    /// 回传本批决定；stickyAction 表示后续批次沿用策略，nil 结果表示请求中止，不会回滚已经完成的文件。
    let completion: (ImportReviewOutcome?) -> Void
    /// 目标路径到用户动作的映射；列表遍历每条 conflict 时读取 / 更新对应决定。
    @State private var choices: [String: ImportAction] = [:]

    /// 每轮检查一项冲突的 target 是否有选择；空批次为 true，路径重复的条目共用同一个决定。
    private var complete: Bool { inspection.conflicts.allSatisfy { choices[$0.target] != nil } }
    /// 至少有一项允许覆盖时才启用“全部覆盖”快捷操作。
    private var canOverwriteAny: Bool { inspection.conflicts.contains { $0.canOverwrite } }

    /// 根据检查结果、批次进度和逐项选择构建审核界面。
    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("处理同名文件").font(.title2.bold())
            Text("本批需处理 \(inspection.conflicts.count) 项。其他文件继续\(operationLabel)，内容相同的文件也由你选择。")
                .font(.subheadline).foregroundStyle(.secondary)
            Text("同名文件很多时，用「全部跳过 / 全部覆盖」一次决定：后续同名文件不再询问。")
                .font(.caption).foregroundStyle(.secondary)
            if let progressText {
                Text(progressText).font(.caption).monospacedDigit().foregroundStyle(.secondary)
            }
            HStack {
                // 直接提交本批全跳过，并要求调用方把跳过策略延续到后续批次。
                Button("全部跳过，不再询问") {
                    completion(
                        ImportReviewOutcome(
                            resolutions: inspection.resolutions(for: .skip), stickyAction: .skip))
                }
                // 本批不可覆盖的条目由 resolutions 转成跳过；后续覆盖策略仍受服务检查约束。
                Button("全部覆盖，不再询问") {
                    completion(
                        ImportReviewOutcome(
                            resolutions: inspection.resolutions(for: .overwrite),
                            stickyAction: .overwrite))
                }.disabled(!canOverwriteAny)
            }
            // 每行对应一个冲突目标；逐项列出路径、原因和允许的处理动作。
            List(inspection.conflicts) { conflict in
                VStack(alignment: .leading, spacing: 8) {
                    Text(conflict.target).font(.headline).textSelection(.enabled)
                    Text(conflict.detail).font(.caption).foregroundStyle(.secondary)
                    Picker(
                        "处理方式",
                        selection: Binding<ImportAction?>(
                            get: { choices[conflict.target] },
                            set: { choices[conflict.target] = $0 }
                        )
                    ) {
                        Text("请选择").tag(Optional<ImportAction>.none)
                        Text("跳过").tag(Optional(ImportAction.skip))
                        // 只为快照中非目录且没有错误的条目提供覆盖选项；最终写入仍由服务重新校验。
                        if conflict.canOverwrite {
                            Text("覆盖").tag(Optional(ImportAction.overwrite))
                        }
                    }
                    .pickerStyle(.menu)
                }.padding(.vertical, 6)
            }
            HStack {
                Text("已选择 \(choices.count) / \(inspection.conflicts.count)").font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
                Button("中止\(operationLabel)", role: .cancel) { completion(nil) }
                Button("应用选择（本批）") {
                    // 防止未选完就提交；按原冲突顺序构造本批决定，stickyAction 为 nil，不改变后续批次策略。
                    guard complete else { return }
                    completion(
                        ImportReviewOutcome(
                            resolutions: inspection.conflicts.compactMap { conflict in
                                choices[conflict.target].map {
                                    ImportResolution(target: conflict.target, action: $0)
                                }
                            },
                            stickyAction: nil))
                }.disabled(!complete)
            }
        }
        .padding(20)
        .frame(width: 620, height: 520)
    }
}
