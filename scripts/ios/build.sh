#!/bin/bash
# Veil iOS 构建入口。
# 所属模块：iOS 客户端构建；按模拟器或真机选择 Rust target 与 Apple SDK，生成 Xcode 项目并编译。
# 核心变量：platform 选择构建目标；target / sdk 对应 Rust bridge 和 Xcode SDK；DerivedData 放在临时目录。
# 依赖 Bash、Cargo、XcodeGen 和 xcodebuild。目标是构建不签名的 arm64 开发产物供本机调试。
# 任一步命令返回失败、引用未定义变量或管道失败时退出；已生成的库、工程和构建缓存不会自动回滚。
set -euo pipefail
cd "$(dirname "$0")/../.."
# 第一个位置参数选择平台，缺省或空值使用 simulator；后续参数不被读取，不支持传递额外 xcodebuild 选项。
platform="${1:-simulator}"
# 只允许 simulator 或 device 两种目标，避免 Rust target 与 SDK 不匹配。
case "$platform" in
  simulator) target=aarch64-apple-ios-sim; sdk=iphonesimulator ;;
  device) target=aarch64-apple-ios; sdk=iphoneos ;;
  *) echo "用法：$0 [simulator|device]" >&2; exit 1 ;;
esac
# Rust 依赖和 Apple 工具链共用最低 iOS 版本；运行环境需预装相应 Rust 交叉编译目标及 Xcode SDK。
export IPHONEOS_DEPLOYMENT_TARGET=17.0
# 先编译 Swift 调用的 Rust 静态桥接库，再生成并构建 Xcode 项目。
cargo build --release -p veil-macos-bridge --lib --target "$target"
# 根据 project.yml 重新生成 Xcode 工程，使目录中的当前源码纳入目标；这里不是复制源文件。
(cd apps/veil-ios && xcodegen generate)
# 显式覆盖库路径和链接参数，并禁用签名；产物位于固定 /tmp/veil-ios-build 下，不是可直接分发的已签名安装包。
# simulator 和 device 共用 DerivedData 路径，并发运行可能竞争同一缓存；脚本不提供互斥或每次运行隔离。
xcodebuild -project apps/veil-ios/VeilIOS.xcodeproj -scheme VeilIOS -configuration Release -sdk "$sdk" -derivedDataPath /tmp/veil-ios-build \
  'LIBRARY_SEARCH_PATHS=$(inherited) "$(SRCROOT)/../../target/$(RUST_TARGET)/release"' \
  'OTHER_LDFLAGS="$(SRCROOT)/../../target/$(RUST_TARGET)/release/libveil_macos_bridge.a" -lc++ -lresolv' \
  CODE_SIGNING_ALLOWED=NO ARCHS=arm64 build
