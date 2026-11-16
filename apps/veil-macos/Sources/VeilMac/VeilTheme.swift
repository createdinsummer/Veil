/// macOS 共用颜色、光标效果和按钮样式。
/// 所属模块：原生界面设计系统；主题颜色从 VeilDesign 读取，按钮组件统一处理层级、悬停、按下、禁用及指针状态。
/// 核心类型：VeilTheme 提供颜色；PointerCursor 维护悬停状态；VeilButtonStyle / VeilIconButtonStyle / VeilPlainButtonStyle 统一控件外观。
/// 依赖 AppKit 光标、SwiftUI ViewModifier / ButtonStyle 和共享设计 token。整体目标是减少各页面重复写颜色和按钮行为。
import AppKit
import SwiftUI

/// 主题颜色入口；每个静态颜色会依据当前系统浅色 / 深色外观解析。
enum VeilTheme {
    /// 窗口主画布背景，来源为共享设计配置的浅色和深色主题。
    static let canvas = designColor("canvas")
    /// 侧栏背景，来源为共享设计配置的浅色和深色主题。
    static let sidebar = designColor("sidebar")
    /// 卡片与表单表面，来源为共享设计配置的浅色和深色主题。
    static let card = designColor("surface")
    /// 正文前景，来源为共享设计配置的浅色和深色主题。
    static let text = designColor("text")
    /// 标题前景，来源为共享设计配置的浅色和深色主题。
    static let heading = designColor("heading")
    /// 次级说明文字，来源为共享设计配置的浅色和深色主题。
    static let secondary = designColor("secondary")
    /// 弱化辅助文字，来源为共享设计配置的浅色和深色主题。
    static let muted = designColor("muted")
    /// 控件和卡片边框，来源为共享设计配置的浅色和深色主题。
    static let border = designColor("border")
    /// 列表选中背景，来源为共享设计配置的浅色和深色主题。
    static let selected = designColor("selected")
    /// 轻量控件悬停背景，来源为共享设计配置的浅色和深色主题。
    static let hover = designColor("hover")
    /// 主要操作强调色，来源为共享设计配置的浅色和深色主题。
    static let accent = designColor("primary")
    /// 强调色背景上的文字或图标，来源为共享设计配置的浅色和深色主题。
    static let onAccent = designColor("on-primary")
    /// 缩略图底色及次级按钮悬停背景，来源为共享设计配置的浅色和深色主题。
    static let thumbnail = designColor("thumbnail")
    /// 品牌渐变起始色，来源为共享设计配置的浅色和深色主题。
    static let heroStart = designColor("hero-start")
    /// 品牌渐变结束色，来源为共享设计配置的浅色和深色主题。
    static let heroEnd = designColor("hero-end")
    /// 删除或错误强调色，来源为共享设计配置的浅色和深色主题。
    static let danger = designColor("danger")
    /// 危险提示背景，来源为共享设计配置的浅色和深色主题。
    static let dangerSurface = designColor("danger-surface")

    /// 把共享主题配置中的颜色键转换为随系统外观切换的颜色。
    /// - Parameters: key: String，必须同时存在于 light 和 dark 分组中的颜色键。
    /// - Returns: Color，保存浅色和深色数值，并在系统解析时选择对应外观。
    /// - Throws: 不抛出可捕获异常；配置缺失或颜色无效时由 VeilDesign 触发运行终止。
    /// - 注意事项: 创建时同时读取两套配置，不会等切换外观后才验证深色值。
    private static func designColor(_ key: String) -> Color {
        color(VeilDesign.hex(key, dark: false), VeilDesign.hex(key, dark: true))
    }

    /// 从两套 RGB 数值创建 macOS 动态主题颜色。
    /// - Parameters: light: UInt32 浅色主题 RGB；dark: UInt32 深色主题 RGB，通常为 0xRRGGBB。
    /// - Returns: Color；外观最匹配 darkAqua 时选 dark，否则选 light，透明度固定为 1。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 只提取低 24 位并将每个通道除以 255，高位忽略；不把高位解释为透明度。
    static func color(_ light: UInt32, _ dark: UInt32) -> Color {
        Color(nsColor: NSColor(name: nil) { appearance in
            let value = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? dark : light
            return NSColor(srgbRed: Double((value >> 16) & 255) / 255,
                           green: Double((value >> 8) & 255) / 255,
                           blue: Double(value & 255) / 255, alpha: 1)
        })
    }
}

/// 在控件启用且鼠标悬停时切换手形光标，离开或禁用时恢复箭头。
private struct PointerCursor: ViewModifier {
    /// 从控件环境读取是否可用，用于透明度和光标反馈，不改变业务权限。
    @Environment(\.isEnabled) private var enabled
    /// 当前视图收到的鼠标悬停状态，驱动本地外观；不保存任何业务数据。
    @State private var hovering = false

    /// 给可点击控件添加随悬停和启用状态变化的系统光标反馈。
    /// - Parameters: content: Content 被修饰的原始视图。
    /// - Returns: some View，订阅鼠标进入离开、启用状态变化和视图消失事件的视图。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 直接设置系统当前光标，没有压栈恢复机制；禁用控件收到进入事件时不会主动设箭头，离开或悬停中被禁用时才恢复。
    func body(content: Content) -> some View {
        content.onHover { inside in
            hovering = inside
            if inside && enabled { NSCursor.pointingHand.set() }
            else if !inside { NSCursor.arrow.set() }
        }.onChange(of: enabled) { _, value in
            if hovering { (value ? NSCursor.pointingHand : NSCursor.arrow).set() }
        }.onDisappear {
            if hovering { NSCursor.arrow.set() }
        }
    }
}

extension View {
    /// 为当前视图安装可点击控件的光标反馈。
    /// - Parameters: 无；修饰当前 View。
    /// - Returns: some View，附加 PointerCursor 的视图。
    /// - Throws: 不抛出异常。
    /// - 注意事项: 只改变光标，不添加点击处理或解除控件禁用状态。
    func pointerCursor() -> some View { modifier(PointerCursor()) }
}

/// 不添加背景和边框的按钮样式，只保留可点击区域和指针反馈。
struct VeilPlainButtonStyle: ButtonStyle {
    /// 保留按钮标签原有外观，只补充矩形命中区域和光标反馈。
    /// - Parameters: configuration: ButtonStyle.Configuration，提供原始标签和当前 isPressed 状态。
    /// - Returns: some View，按本样式包装的按钮内容。
    /// - Throws: 不抛出可捕获异常。
    /// - 注意事项: 不使用 isPressed 提供按压外观，也不在这里触发业务操作。
    func makeBody(configuration: Configuration) -> some View {
        configuration.label.contentShape(Rectangle()).pointerCursor()
    }
}

/// 具有主、次、轻量三个视觉层级的标准按钮样式。
struct VeilButtonStyle: ButtonStyle {
    /// primary 用于主要确认，secondary 用于普通操作，subtle 用于弱化操作。
    enum Variant { case primary, secondary, subtle }
    /// 当前按钮的视觉层级。
    var variant: Variant = .secondary
    /// 从控件环境读取是否可用，用于透明度和光标反馈，不改变业务权限。
    @Environment(\.isEnabled) private var enabled
    /// 当前视图收到的鼠标悬停状态，驱动本地外观；不保存任何业务数据。
    @State private var hovering = false

    /// 为主、次和轻量操作按钮生成统一尺寸与交互外观。
    /// - Parameters: configuration: ButtonStyle.Configuration，提供原始标签和当前 isPressed 状态。
    /// - Returns: some View，按本样式包装的按钮内容。
    /// - Throws: 不抛出可捕获异常。
    /// - 注意事项: 读取主题配置，配置缺失可能终止运行；禁用以整体透明度表达，hovering 仍可改变背景。
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: VeilDesign.font("body"), weight: .semibold))
            .padding(.horizontal, 16).frame(minHeight: VeilDesign.layout("button-height"))
            .foregroundStyle(variant == .primary ? VeilTheme.onAccent : VeilTheme.text)
            .background(background, in: RoundedRectangle(cornerRadius: VeilDesign.radius("button")))
            .overlay(RoundedRectangle(cornerRadius: VeilDesign.radius("button")).stroke(variant == .secondary ? VeilTheme.border : .clear))
            .opacity(enabled ? (configuration.isPressed ? 0.8 : 1) : 0.45)
            .onHover { hovering = $0 }
            .animation(.easeOut(duration: 0.12), value: hovering)
            .contentShape(Rectangle()).pointerCursor()
    }

    /// 依据按钮层级和悬停状态选择背景色。
    private var background: Color {
        switch variant {
        case .primary: VeilTheme.accent.opacity(hovering ? 0.9 : 1)
        case .secondary: hovering ? VeilTheme.thumbnail : VeilTheme.card
        case .subtle: hovering ? VeilTheme.hover : .clear
        }
    }
}

/// 工具栏图标按钮的紧凑样式；背景、透明度和光标随交互状态变化。
struct VeilIconButtonStyle: ButtonStyle {
    /// 当前视图收到的鼠标悬停状态，驱动本地外观；不保存任何业务数据。
    @State private var hovering = false
    /// 从控件环境读取是否可用，用于透明度和光标反馈，不改变业务权限。
    @Environment(\.isEnabled) private var enabled
    /// 为工具栏图标操作生成紧凑命中区域与交互反馈。
    /// - Parameters: configuration: ButtonStyle.Configuration，提供原始标签和当前 isPressed 状态。
    /// - Returns: some View，按本样式包装的按钮内容。
    /// - Throws: 不抛出可捕获异常。
    /// - 注意事项: 最小命中尺寸为 32×32 点；禁用和按下只改变透明度，实际点击行为由 Button 管理。
    func makeBody(configuration: Configuration) -> some View {
        configuration.label.font(.system(size: 13)).foregroundStyle(VeilTheme.secondary)
            .frame(minWidth: 32, minHeight: 32)
            .background(hovering ? VeilTheme.hover : .clear, in: RoundedRectangle(cornerRadius: 6))
            .opacity(enabled ? (configuration.isPressed ? 0.6 : 1) : 0.4)
            .onHover { hovering = $0 }
            .contentShape(Rectangle()).pointerCursor()
    }
}
