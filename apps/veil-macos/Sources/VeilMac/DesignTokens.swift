import Foundation

/// 数值使用设计稿中的点数；颜色随系统外观切换。
enum VeilDesign {
    struct Tokens: Decodable, Sendable {
        struct Font: Decodable, Sendable { let size: [String: String] }
        let color: [String: [String: String]]
        let font: Font
        let layout: [String: String]
        let radius: [String: String]
    }

    static let tokens: Tokens = {
        guard let url = Bundle.module.url(forResource: "DesignTokens", withExtension: "json"),
              let data = try? Data(contentsOf: url),
              let tokens = try? JSONDecoder().decode(Tokens.self, from: data) else {
            preconditionFailure("缺少有效的 DesignTokens.json，请同步 ui/tokens.json 后构建")
        }
        return tokens
    }()

    static func layout(_ key: String) -> CGFloat { number(tokens.layout[key]) }
    static func radius(_ key: String) -> CGFloat { number(tokens.radius[key]) }
    static func font(_ key: String) -> CGFloat { number(tokens.font.size[key]) }

    static func hex(_ key: String, dark: Bool) -> UInt32 {
        guard let value = tokens.color[dark ? "dark" : "light"]?[key],
              let result = UInt32(value.replacingOccurrences(of: "#", with: ""), radix: 16) else {
            preconditionFailure("缺少颜色参数：\(key)")
        }
        return result
    }

    private static func number(_ value: String?) -> CGFloat {
        guard let value, let number = Double(value.replacingOccurrences(of: "px", with: "")) else {
            preconditionFailure("缺少尺寸参数")
        }
        return CGFloat(number)
    }
}
