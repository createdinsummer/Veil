// swift-tools-version: 6.0
import PackageDescription
import Foundation

let rustLibrary = ProcessInfo.processInfo.environment["VEIL_RUST_LIB_DIR"]
    ?? URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        .appendingPathComponent("../../target/debug").standardized.path

let package = Package(
    name: "VeilMac",
    platforms: [.macOS(.v14)],
    products: [.executable(name: "VeilMac", targets: ["VeilMac"])],
    targets: [
        .systemLibrary(name: "CVeilBridge", path: "Sources/CVeilBridge"),
        .executableTarget(
            name: "VeilMac", dependencies: ["CVeilBridge"],
            resources: [.process("Resources")],
            linkerSettings: [
                .unsafeFlags(["-L", rustLibrary, "-Xlinker", "-rpath", "-Xlinker", rustLibrary,
                              "-Xlinker", "-rpath", "-Xlinker", "@executable_path/../Frameworks"])
            ]
        ),
        .testTarget(name: "VeilMacTests", dependencies: ["VeilMac"])
    ]
)
