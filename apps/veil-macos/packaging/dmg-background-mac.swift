/// 绘制 macOS 安装镜像中 Finder 展示的背景图。
/// 所属模块：DMG 安装包外观；生成普通与 Retina 两种分辨率的说明文字和拖动箭头。
/// 核心流程：顶层脚本建立画布，label 绘制文字，两轮位图绘制后打包为包含多分辨率表示的 TIFF。
/// 依赖 macOS AppKit 图形上下文与 TIFF 编码；唯一命令行参数是输出文件路径。
/// 目标是生成 DMG 构建素材；父目录需预先存在，已有目标可能被覆盖，写入失败作为顶层错误结束执行。
import AppKit

// 原生绘制两种分辨率，避免 Retina 屏幕上的文字与箭头模糊。
// 设计画布以逻辑点为单位；第二轮绘制用 2 倍像素密度适配 Retina。
let size = NSSize(width: 640, height: 360)
// 汇总两种像素密度的位图表示，最终一次编码为 TIFF；不是分别生成两个输出文件。
let image = NSImage(size: size)
// 安装标题的深灰色；本脚本绘制固定浅色素材，不读取应用动态主题。
let textColor = NSColor(srgbRed: 0.153, green: 0.153, blue: 0.153, alpha: 1)
// 说明文字和箭头共用的弱化色。
let secondary = NSColor(srgbRed: 0.522, green: 0.537, blue: 0.525, alpha: 1)

// 同一布局分别输出标准和 Retina 位图，避免高分屏上出现模糊文字。
// 集合中的每项是像素密度倍率；每轮创建完整背景，逻辑尺寸一致，像素宽高分别乘以该倍率。
for scale in [1, 2] {
    // RGBA 位图，标准图为 640×360 像素，Retina 图为 1280×720；创建失败会因强制解包终止。
    let bitmap = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: 640 * scale, pixelsHigh: 360 * scale,
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
        colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
    )!
    bitmap.size = size
    // 保存调用前绘图状态，再切换到本轮位图，避免文字和路径画到其他上下文。
    NSGraphicsContext.saveGraphicsState()
    let context = NSGraphicsContext(bitmapImageRep: bitmap)!
    NSGraphicsContext.current = context
    NSColor(srgbRed: 0.969, green: 0.969, blue: 0.969, alpha: 1).setFill()
    NSBezierPath(rect: NSRect(origin: .zero, size: size)).fill()

    /// 在当前分辨率位图上绘制水平居中的安装说明文字。
    /// - Parameters: string: String 要绘制的文案；top: CGFloat 文字框上边距画布顶部的逻辑点数；font: NSFont 字体及字号；color: NSColor 文本颜色；tracking: CGFloat 字符间距，默认 0。
    /// - Returns: Void；文字直接绘制到当前 NSGraphicsContext，不生成独立图片对象。
    /// - Throws: 不抛出可捕获异常。
    /// - 注意事项: 捕获外层 640×360 逻辑画布，绘制框固定宽 600、高 32 点；调用前必须设置当前图形上下文，长文案可能受固定框裁切。
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

    // 分别绘制水平箭杆与两段箭头，位于左右安装图标之间；这里只生成背景，不放置 Finder 图标。
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

// 要求恰好一个输出路径，编码失败也终止；参数检查在绘图后执行，不会提前跳过素材生成。
guard CommandLine.arguments.count == 2, let data = image.tiffRepresentation else {
    fatalError("用法：swift dmg-background-mac.swift 输出路径")
}
// 直接写入目标文件，不创建父目录，也未请求原子替换；文件系统错误由顶层 try 传播。
try data.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
