import SwiftUI

struct ImportConflictReview: View {
    let inspection: ImportInspection
    var operationLabel: String = "导入"
    var progressText: String? = nil
    let completion: (ImportReviewOutcome?) -> Void
    @State private var choices: [String: ImportAction] = [:]

    private var complete: Bool { inspection.conflicts.allSatisfy { choices[$0.target] != nil } }
    private var canOverwriteAny: Bool { inspection.conflicts.contains { $0.canOverwrite } }

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
                Button("全部跳过，不再询问") {
                    completion(
                        ImportReviewOutcome(
                            resolutions: inspection.resolutions(for: .skip), stickyAction: .skip))
                }
                Button("全部覆盖，不再询问") {
                    completion(
                        ImportReviewOutcome(
                            resolutions: inspection.resolutions(for: .overwrite),
                            stickyAction: .overwrite))
                }.disabled(!canOverwriteAny)
            }
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
