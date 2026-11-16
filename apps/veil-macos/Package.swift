// swift-tools-version: 6.0
// macOS 桌面应用的 Swift Package 清单。
// 所属模块：构建与打包配置；声明 VeilMac 可执行程序、C/Rust 桥接模块、资源和测试目标。
// VEIL_RUST_LIB_DIR 可覆盖本机构建时 Rust 动态库目录；链接设置把该目录和应用内 Frameworks 加入运行时搜索路径。
// 业务目标是把 SwiftUI 界面与 Veil Rust 服务连接成可运行的 macOS 应用。
import PackageDescription
import Foundation

// Rust 服务动态库的搜索目录：优先使用构建环境显式提供的路径，否则使用仓库 target/debug。
let rustLibrary = ProcessInfo.processInfo.environment["VEIL_RUST_LIB_DIR"]
    ?? URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        .appendingPathComponent("../../target/debug").standardized.path

// SwiftPM 包描述对象；Cargo 不读取此清单，Rust 动态库需由外部构建流程预先生成。
let package = Package(
    name: "VeilMac",
    platforms: [.macOS(.v14)],
    products: [.executable(name: "VeilMac", targets: ["VeilMac"])],
    // 每项声明一个构建目标：C 头文件模块、Swift 可执行程序和依赖该程序模块的测试。
    targets: [
        // 暴露现有桥接头文件与模块映射，不在 SwiftPM 中编译 Rust 服务。
        .systemLibrary(name: "CVeilBridge", path: "Sources/CVeilBridge"),
        .executableTarget(
            name: "VeilMac", dependencies: ["CVeilBridge"],
            // 将品牌图和设计参数打包为 Bundle.module 资源，供界面在运行时加载。
            resources: [.process("Resources")],
            linkerSettings: [
                // -L 指定链接搜索目录；两个 rpath 分别支持本地开发和 .app 内 Frameworks 布局。
                // 这些参数不负责复制动态库，打包脚本仍需将库放到对应目录。
                .unsafeFlags(["-L", rustLibrary, "-Xlinker", "-rpath", "-Xlinker", rustLibrary,
                              "-Xlinker", "-rpath", "-Xlinker", "@executable_path/../Frameworks"])
            ]
        ),
        // 测试可访问应用模块；真实服务集成用例仍需运行环境显式配置隔离数据目录。
        .testTarget(name: "VeilMacTests", dependencies: ["VeilMac"])
    ]
)
