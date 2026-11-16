/// 欢迎页使用的原生保险箱插画、盾牌图形和说明卡片。
/// 所属模块：macOS 首页视觉组件；VaultIllustration 绘制保险箱，VaultShield 以 24×24 坐标定义盾牌，WelcomeCard 提供创建 / 导入入口。
/// 依赖 SwiftUI 和 VeilTheme / VeilDesign 共享的主题色与尺寸。整体目标是在浅色、深色外观下保持品牌插画清晰且不依赖外部图片文件。
import SwiftUI

/// 首页的纯装饰保险箱插画，用 SwiftUI 图形组合，不保存业务状态或访问文件。
/// 固定画布为 260×260 点，颜色跟随主题；整体对无障碍隐藏，操作入口由外层欢迎卡片提供。
struct VaultIllustration: View {
    /// 组合同心圆、保险箱主体、盾牌和装饰图形，作为首页视觉说明。
    var body: some View {
        ZStack {
            Circle().stroke(VeilTheme.secondary.opacity(0.15)).frame(width: 258, height: 258)
            Circle().stroke(VeilTheme.secondary.opacity(0.15)).frame(width: 205, height: 205)
            VStack(spacing: 0) {
                HStack(spacing: 4) {
                    // 三轮分别绘制一个顶部装饰圆点，索引不对应容器或文件业务数据。
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

/// 在归一化 24×24 画布上绘制盾牌外框和内部勾选符号。
private struct VaultShield: Shape {
    /// 生成欢迎插画中盾牌外轮廓和内部勾选线段的缩放路径。
    /// - Parameters: rect: CGRect，SwiftUI 分配的绘图矩形，包含原点和目标宽高。
    /// - Returns: Path；外轮廓为闭合子路径，内部勾选为另一条未闭合折线，供调用方描边。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 以 24×24 设计网格分别缩放横纵轴，非正方形矩形会拉伸图案，不额外保持宽高比；本方法不负责颜色和线宽。
    func path(in rect: CGRect) -> Path {
        // 累积两段矢量子路径：先构造闭合盾牌，再移动画笔绘制内部勾选。
        var path = Path()
        // 所有图形坐标都基于 24×24 设计网格，按 rect 宽高缩放到实际视图尺寸。
        /// 功能：将保险库图案的设计坐标映射到当前绘制区域，使图案随视图缩放。
        /// - Parameters:
        ///   - x: Double 类型，24 单位设计网格中的横坐标。
        ///   - y: Double 类型，24 单位设计网格中的纵坐标。
        /// - Returns: CGPoint 类型，包含 rect 原点偏移后的实际绘制坐标。
        /// - Throws: 不抛出错误。
        /// - Note: 捕获外层 rect，分别按宽高缩放；不限制传入点必须位于网格内。
        func point(_ x: Double, _ y: Double) -> CGPoint { CGPoint(x: rect.minX + rect.width * x / 24, y: rect.minY + rect.height * y / 24) }
        // 从顶点沿右边缘向下，两个曲线控制点确定圆滑的盾牌下缘，再沿左边缘闭合。
        path.move(to: point(12, 2.8))
        path.addLine(to: point(19, 5.5))
        path.addLine(to: point(19, 11.2))
        path.addCurve(to: point(12, 21), control1: point(19, 15.6), control2: point(16.2, 18.9))
        path.addCurve(to: point(5, 11.2), control1: point(7.8, 18.9), control2: point(5, 15.6))
        path.addLine(to: point(5, 5.5))
        path.closeSubpath()
        // 单独起笔画勾选，两段直线不闭合，避免连回盾牌边缘。
        path.move(to: point(9.2, 11.8))
        path.addLine(to: point(11.1, 13.7))
        path.addLine(to: point(14.9, 9.7))
        return path
    }
}

/// 首页欢迎卡片，组合本地加密说明、装饰插画和创建 / 导入入口。
/// 不持有表单或容器数据；按钮把操作交给共享 Store，按忙碌状态和空间可用性禁用。
struct WelcomeCard: View {
    /// 首页动作使用的应用 Store；控制空间可用性和当前忙碌状态。
    @EnvironmentObject private var store: AppStore
    /// 根据宽度切换插画和文案布局，并绑定创建 / 导入操作。
    var body: some View {
        GeometryReader { geometry in
            ZStack(alignment: .leading) {
                // 宽度超过 680 点才显示右侧装饰图；窄布局保留操作文案，避免挤压按钮。
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
                            .buttonStyle(VeilButtonStyle(variant: .primary)).disabled(store.activeSpace?.isAvailable != true)
                        Button("导入容器", systemImage: "folder") { Task { await store.choosePackage() } }
                            .buttonStyle(VeilButtonStyle()).disabled(store.activeSpace?.isAvailable != true)
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
