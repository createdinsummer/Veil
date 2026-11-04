import AppKit

// 原生绘制两种分辨率，避免 Retina 屏幕上的文字与箭头模糊。
let size = NSSize(width: 640, height: 360)
let image = NSImage(size: size)
let textColor = NSColor(srgbRed: 0.153, green: 0.153, blue: 0.153, alpha: 1)
let secondary = NSColor(srgbRed: 0.522, green: 0.537, blue: 0.525, alpha: 1)

for scale in [1, 2] {
    let bitmap = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: 640 * scale, pixelsHigh: 360 * scale,
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
        colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
    )!
    bitmap.size = size
    NSGraphicsContext.saveGraphicsState()
    let context = NSGraphicsContext(bitmapImageRep: bitmap)!
    NSGraphicsContext.current = context
    NSColor(srgbRed: 0.969, green: 0.969, blue: 0.969, alpha: 1).setFill()
    NSBezierPath(rect: NSRect(origin: .zero, size: size)).fill()

    func label(_ string: String, top: CGFloat, font: NSFont, color: NSColor, tracking: CGFloat = 0) {
        let style = NSMutableParagraphStyle()
        style.alignment = .center
        (string as NSString).draw(in: NSRect(x: 20, y: size.height - top - 32, width: 600, height: 32), withAttributes: [
            .font: font, .foregroundColor: color, .paragraphStyle: style, .kern: tracking
        ])
    }
    label("安装 Veil", top: 34, font: .systemFont(ofSize: 24, weight: .semibold), color: textColor)
    label("将左侧 Veil 拖到右侧 Applications，即可完成安装", top: 77,
          font: .systemFont(ofSize: 12), color: secondary)

    let arrow = NSBezierPath()
    arrow.move(to: NSPoint(x: 291, y: 170))
    arrow.line(to: NSPoint(x: 349, y: 170))
    arrow.move(to: NSPoint(x: 338, y: 181))
    arrow.line(to: NSPoint(x: 349, y: 170))
    arrow.line(to: NSPoint(x: 338, y: 159))
    arrow.lineWidth = 2
    arrow.lineCapStyle = .round
    arrow.lineJoinStyle = .round
    secondary.withAlphaComponent(0.65).setStroke()
    arrow.stroke()

    label("安装完成后，可推出此磁盘映像。", top: 285, font: .systemFont(ofSize: 11), color: secondary)
    NSGraphicsContext.restoreGraphicsState()
    image.addRepresentation(bitmap)
}

guard CommandLine.arguments.count == 2, let data = image.tiffRepresentation else {
    fatalError("用法：swift dmg-background-mac.swift 输出路径")
}
try data.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
