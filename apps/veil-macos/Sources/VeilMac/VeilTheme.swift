import AppKit
import SwiftUI

/// 读取与界面设计稿相同的颜色与尺寸。
enum VeilTheme {
    static let canvas = designColor("canvas")
    static let sidebar = designColor("sidebar")
    static let card = designColor("surface")
    static let text = designColor("text")
    static let heading = designColor("heading")
    static let secondary = designColor("secondary")
    static let muted = designColor("muted")
    static let border = designColor("border")
    static let selected = designColor("selected")
    static let hover = designColor("hover")
    static let accent = designColor("primary")
    static let onAccent = designColor("on-primary")
    static let thumbnail = designColor("thumbnail")
    static let heroStart = designColor("hero-start")
    static let heroEnd = designColor("hero-end")
    static let danger = designColor("danger")
    static let dangerSurface = designColor("danger-surface")

    private static func designColor(_ key: String) -> Color {
        color(VeilDesign.hex(key, dark: false), VeilDesign.hex(key, dark: true))
    }

    static func color(_ light: UInt32, _ dark: UInt32) -> Color {
        Color(nsColor: NSColor(name: nil) { appearance in
            let value = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? dark : light
            return NSColor(srgbRed: Double((value >> 16) & 255) / 255,
                           green: Double((value >> 8) & 255) / 255,
                           blue: Double(value & 255) / 255, alpha: 1)
        })
    }
}

private struct PointerCursor: ViewModifier {
    @Environment(\.isEnabled) private var enabled
    @State private var hovering = false

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
    func pointerCursor() -> some View { modifier(PointerCursor()) }
}

struct VeilPlainButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label.contentShape(Rectangle()).pointerCursor()
    }
}

struct VeilButtonStyle: ButtonStyle {
    enum Variant { case primary, secondary, subtle }
    var variant: Variant = .secondary
    @Environment(\.isEnabled) private var enabled
    @State private var hovering = false

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

    private var background: Color {
        switch variant {
        case .primary: VeilTheme.accent.opacity(hovering ? 0.9 : 1)
        case .secondary: hovering ? VeilTheme.thumbnail : VeilTheme.card
        case .subtle: hovering ? VeilTheme.hover : .clear
        }
    }
}

struct VeilIconButtonStyle: ButtonStyle {
    @State private var hovering = false
    @Environment(\.isEnabled) private var enabled
    func makeBody(configuration: Configuration) -> some View {
        configuration.label.font(.system(size: 13)).foregroundStyle(VeilTheme.secondary)
            .frame(minWidth: 32, minHeight: 32)
            .background(hovering ? VeilTheme.hover : .clear, in: RoundedRectangle(cornerRadius: 6))
            .opacity(enabled ? (configuration.isPressed ? 0.6 : 1) : 0.4)
            .onHover { hovering = $0 }
            .contentShape(Rectangle()).pointerCursor()
    }
}
