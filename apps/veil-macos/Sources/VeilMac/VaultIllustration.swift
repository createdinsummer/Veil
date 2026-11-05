import SwiftUI

/// 使用原生形状复现 Web 版的保险箱插画。
struct VaultIllustration: View {
    var body: some View {
        ZStack {
            Circle().stroke(VeilTheme.secondary.opacity(0.15)).frame(width: 258, height: 258)
            Circle().stroke(VeilTheme.secondary.opacity(0.15)).frame(width: 205, height: 205)
            VStack(spacing: 0) {
                HStack(spacing: 4) {
                    ForEach(0..<3) { _ in Circle().fill(VeilTheme.color(0xc7c8c7, 0x636a65)).frame(width: 4, height: 4) }
                    Spacer()
                }.padding(.leading, 11).frame(height: 26)
                Rectangle().fill(VeilTheme.border).frame(height: 1)
                VaultShield().stroke(VeilTheme.color(0x8b8e8c, 0xa5afa8), style: StrokeStyle(lineWidth: 1.6, lineCap: .round, lineJoin: .round))
                    .frame(width: 53, height: 53).frame(height: 95)
                VStack(alignment: .leading, spacing: 6) {
                    Capsule().frame(width: 68, height: 4)
                    Capsule().frame(width: 42, height: 4)
                    Spacer(minLength: 0)
                }.foregroundStyle(VeilTheme.color(0xd6d7d6, 0x525a54)).frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 15)
            }
            .frame(width: 143, height: 158)
            .background(LinearGradient(colors: [VeilTheme.color(0xfdfdfd, 0x414843), VeilTheme.color(0xe6e7e6, 0x2d332f)], startPoint: .topLeading, endPoint: .bottomTrailing))
            .clipShape(RoundedRectangle(cornerRadius: 16))
            .overlay(RoundedRectangle(cornerRadius: 16).stroke(VeilTheme.color(0xd3d4d3, 0x58615b)))
            .shadow(color: .black.opacity(0.1), radius: 15, x: 0, y: 18)
            .rotationEffect(.degrees(-5))
            Circle().fill(VeilTheme.muted.opacity(0.7)).frame(width: 7, height: 7).offset(x: 78, y: -99)
            Circle().fill(VeilTheme.muted.opacity(0.7)).frame(width: 5, height: 5).offset(x: -100, y: 79)
            Rectangle().stroke(VeilTheme.muted, lineWidth: 1).frame(width: 11, height: 11).rotationEffect(.degrees(25)).offset(x: 86, y: 85)
        }.frame(width: 260, height: 260).accessibilityHidden(true)
    }
}

private struct VaultShield: Shape {
    func path(in rect: CGRect) -> Path {
        var path = Path()
        func point(_ x: Double, _ y: Double) -> CGPoint { CGPoint(x: rect.minX + rect.width * x / 24, y: rect.minY + rect.height * y / 24) }
        path.move(to: point(12, 2.8))
        path.addLine(to: point(19, 5.5))
        path.addLine(to: point(19, 11.2))
        path.addCurve(to: point(12, 21), control1: point(19, 15.6), control2: point(16.2, 18.9))
        path.addCurve(to: point(5, 11.2), control1: point(7.8, 18.9), control2: point(5, 15.6))
        path.addLine(to: point(5, 5.5))
        path.closeSubpath()
        path.move(to: point(9.2, 11.8))
        path.addLine(to: point(11.1, 13.7))
        path.addLine(to: point(14.9, 9.7))
        return path
    }
}

struct WelcomeCard: View {
    @EnvironmentObject private var store: AppStore
    var body: some View {
        GeometryReader { geometry in
            ZStack(alignment: .leading) {
                if geometry.size.width > 680 {
                    VaultIllustration().frame(maxWidth: .infinity, alignment: .trailing).padding(.trailing, 24)
                }
                VStack(alignment: .leading, spacing: 0) {
                    HStack(spacing: 6) {
                        Circle().fill(VeilTheme.secondary).frame(width: 6, height: 6)
                        Text("一切都在你的掌控中").font(.system(size: 10, weight: .medium))
                    }
                    .foregroundStyle(VeilTheme.secondary).padding(.horizontal, 10).padding(.vertical, 7)
                    .background(VeilTheme.card.opacity(0.52), in: Capsule()).overlay(Capsule().stroke(VeilTheme.border))
                    Text("安全，从这里开始。").font(.system(size: VeilDesign.font("hero"), weight: .semibold)).tracking(-0.6)
                        .foregroundStyle(VeilTheme.heading).padding(.top, 19)
                    Text("创建一个加密容器，或导入已有容器。\n你的文件加密保存在本机，由你掌管。")
                        .font(.system(size: 12)).foregroundStyle(VeilTheme.secondary).lineSpacing(7).padding(.top, 11)
                    HStack(spacing: 10) {
                        Button("创建容器", systemImage: "plus") { Task { await store.newContainer() } }
                            .buttonStyle(VeilButtonStyle(variant: .primary)).disabled(store.activeSpace == nil)
                        Button("导入容器", systemImage: "folder") { Task { await store.choosePackage() } }
                            .buttonStyle(VeilButtonStyle()).disabled(store.activeSpace == nil)
                    }.padding(.top, 22).disabled(store.isBusy)
                }
                .padding(.horizontal, 38)
                .frame(maxWidth: geometry.size.width > 680 ? geometry.size.width - 270 : .infinity, alignment: .leading)
            }.frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .frame(height: VeilDesign.layout("hero-height"))
        .background(LinearGradient(colors: [VeilTheme.heroStart, VeilTheme.heroEnd], startPoint: .topLeading, endPoint: .bottomTrailing), in: RoundedRectangle(cornerRadius: VeilDesign.radius("hero")))
        .overlay(RoundedRectangle(cornerRadius: VeilDesign.radius("hero")).stroke(VeilTheme.border))
    }
}
