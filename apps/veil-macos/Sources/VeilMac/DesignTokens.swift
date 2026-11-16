/// macOS UI 设计参数读取模块。
/// 所属模块：Veil 原生界面基础层；将共享 JSON token 解码后提供颜色、尺寸、圆角与字体访问入口。
/// 核心类型：Tokens / Font 描述 JSON 数据；VeilDesign 负责加载资源、读取指定分组并把像素文案转成 CGFloat。
/// 依赖能力：Foundation 解码与 Swift Package Bundle.module 资源访问。整体目标是让 macOS 视图复用 Web 设计源参数，缺少必需 token 时及早暴露资源错误。
import Foundation

/// 共享 UI token 的只读访问门面；尺寸值以点数解释，颜色按系统外观选择。
enum VeilDesign {
    /// DesignTokens.json 的可解码结构；键名与 Web 共享 token 分组一致。
    struct Tokens: Decodable, Sendable {
        /// 字体配置对象，仅保存 size 映射；键为字号用途（如 body），值为可解析尺寸文本（如 12px）。
        struct Font: Decodable, Sendable { let size: [String: String] }
        /// 主题名 → 颜色键 → 十六进制颜色文本。
        let color: [String: [String: String]]
        /// 字体分组及其字号映射。
        let font: Font
        /// 通用布局尺寸键值，例如 sidebar 或 button-height。
        let layout: [String: String]
        /// 控件圆角尺寸键值。
        let radius: [String: String]
    }

    /// 首次访问同步读取并缓存完整资源；Swift 静态常量初始化只执行一次，后续不重新读取磁盘。
    /// 缺资源直接终止，读取或解码失败先尝试记录日志再终止，没有内置默认配置回退。
    static let tokens: Tokens = {
        guard let url = Bundle.module.url(forResource: "DesignTokens", withExtension: "json"),
              let data = AppLog.attempt({ try Data(contentsOf: url) }),
              let tokens = AppLog.attempt({ try JSONDecoder().decode(Tokens.self, from: data) }) else {
            preconditionFailure("缺少有效的 DesignTokens.json，请同步 ui/tokens.json 后构建")
        }
        return tokens
    }()

    /// 为原生界面读取共享设计配置中的布局尺寸。
    /// - Parameters: key: String，layout 分组内已配置的键；任意未知键都不会使用默认值。
    /// - Returns: CGFloat，把配置文本中的 px 移除后解析为数值，调用方按界面点数使用。
    /// - Throws: 不抛出可捕获异常；资源加载失败、键缺失或数值解析失败会触发 preconditionFailure 终止运行。
    /// - 注意事项: 首次访问会同步加载并缓存整份 JSON；没有正数或有限值检查，配置必须由构建流程保证有效。
    static func layout(_ key: String) -> CGFloat { number(tokens.layout[key]) }
    /// 为原生界面读取共享设计配置中的控件圆角尺寸。
    /// - Parameters: key: String，radius 分组内已配置的键；任意未知键都不会使用默认值。
    /// - Returns: CGFloat，把配置文本中的 px 移除后解析为数值，调用方按界面点数使用。
    /// - Throws: 不抛出可捕获异常；资源加载失败、键缺失或数值解析失败会触发 preconditionFailure 终止运行。
    /// - 注意事项: 首次访问会同步加载并缓存整份 JSON；没有正数或有限值检查，配置必须由构建流程保证有效。
    static func radius(_ key: String) -> CGFloat { number(tokens.radius[key]) }
    /// 为原生界面读取共享设计配置中的字体大小。
    /// - Parameters: key: String，font.size 分组内已配置的键；任意未知键都不会使用默认值。
    /// - Returns: CGFloat，把配置文本中的 px 移除后解析为数值，调用方按界面点数使用。
    /// - Throws: 不抛出可捕获异常；资源加载失败、键缺失或数值解析失败会触发 preconditionFailure 终止运行。
    /// - 注意事项: 首次访问会同步加载并缓存整份 JSON；没有正数或有限值检查，配置必须由构建流程保证有效。
    static func font(_ key: String) -> CGFloat { number(tokens.font.size[key]) }

    /// 为界面主题读取颜色配置的十六进制整数。
    /// - Parameters: key: String 颜色键；dark: Bool，true 选 dark 分组，false 选 light 分组。
    /// - Returns: UInt32，移除文本中所有 # 后按十六进制解析的无符号整数。
    /// - Throws: 不抛出可捕获异常；资源无效、主题或键缺失、整数解析失败时触发 preconditionFailure。
    /// - 注意事项: 不校验必须是六位 RGB，也不限制为 24 位；调用方应使用经审核的主题配置，不传入运行时用户输入。
    static func hex(_ key: String, dark: Bool) -> UInt32 {
        guard let value = tokens.color[dark ? "dark" : "light"]?[key],
              let result = UInt32(value.replacingOccurrences(of: "#", with: ""), radix: 16) else {
            preconditionFailure("缺少颜色参数：\(key)")
        }
        return result
    }

    /// 把共享尺寸文本转换为原生界面可用数值。
    /// - Parameters: value: String? 可选尺寸文本，通常形如 12px；nil 表示配置键缺失。
    /// - Returns: CGFloat，移除所有小写 px 子串后由 Double 解析的数值，不进行像素密度换算。
    /// - Throws: 不抛出可捕获异常；缺失或无法解析时触发 preconditionFailure。
    /// - 注意事项: 不验证 px 只在后缀，不拒绝负数或非有限数；此方法依赖可信的构建设计资源。
    private static func number(_ value: String?) -> CGFloat {
        guard let value, let number = Double(value.replacingOccurrences(of: "px", with: "")) else {
            preconditionFailure("缺少尺寸参数")
        }
        return CGFloat(number)
    }
}
