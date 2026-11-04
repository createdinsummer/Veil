#!/usr/bin/env bash
# 构建 macOS SwiftUI 客户端和 DMG 安装包。
set -euo pipefail

CONFIGURATION="release"
case "${1:-}" in
    "") ;;
    --release) CONFIGURATION="release" ;;
    --debug) CONFIGURATION="debug" ;;
    --help|-h)
        printf '用法：bash scripts/desktop/build-mac-app.sh [--release|--debug]\n\n默认构建发布版，在 release/desktop/ 生成 DMG 安装包及 SHA-256 校验文件，临时 Veil.app 自动清理。\n需要 macOS、Xcode 和 Rust，不依赖 Node.js。\n'
        exit 0 ;;
    *) printf '不支持此参数，请使用 --help 查看用法。\n' >&2; exit 1 ;;
esac
if [[ $# -gt 1 || "$(uname -s)" != "Darwin" ]]; then
    printf '此脚本用于 macOS，且最多接受一个参数。\n' >&2; exit 1
fi
for tool in cargo swift xcrun python3 otool install_name_tool codesign hdiutil shasum; do
    command -v "$tool" >/dev/null 2>&1 || { printf '缺少构建工具：%s\n' "$tool" >&2; exit 1; }
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
APP_DIR="$PROJECT_ROOT/apps/veil-macos"
PACKAGING_DIR="$APP_DIR/packaging"
DMG_TOOLS="$APP_DIR/.build/dmg-tools"
if [[ ! -x "$DMG_TOOLS/bin/python" ]]; then
    python3 -m venv "$DMG_TOOLS"
fi
if ! cmp -s "$PACKAGING_DIR/requirements-mac.txt" "$DMG_TOOLS/requirements.txt"; then
    printf '准备 DMG 布局工具…\n'
    "$DMG_TOOLS/bin/python" -m pip install --disable-pip-version-check -r "$PACKAGING_DIR/requirements-mac.txt"
    cp "$PACKAGING_DIR/requirements-mac.txt" "$DMG_TOOLS/requirements.txt"
fi
python3 "$PROJECT_ROOT/ui/tools/sync-macos.py"
TARGET_DIR="$(cargo metadata --manifest-path "$PROJECT_ROOT/Cargo.toml" --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
RUST_ARGS=()
[[ "$CONFIGURATION" != "release" ]] || RUST_ARGS+=(--release)

printf '构建 Rust 原生调用库…\n'
cargo build --manifest-path "$PROJECT_ROOT/Cargo.toml" -p veil-macos-bridge "${RUST_ARGS[@]}"
export VEIL_RUST_LIB_DIR="$TARGET_DIR/$CONFIGURATION"
export CLANG_MODULE_CACHE_PATH="$APP_DIR/.build/clang-cache"
export SWIFTPM_MODULECACHE_OVERRIDE="$APP_DIR/.build/swift-cache"
SWIFT_ARGS=(--package-path "$APP_DIR" --scratch-path "$APP_DIR/.build/swift" --cache-path "$APP_DIR/.build/cache" --config-path "$APP_DIR/.build/config" --security-path "$APP_DIR/.build/security" --disable-sandbox -c "$CONFIGURATION")
printf '\n构建 SwiftUI 客户端…\n'
swift build "${SWIFT_ARGS[@]}"
BIN_DIR="$(swift build "${SWIFT_ARGS[@]}" --show-bin-path)"

OUTPUT="$PROJECT_ROOT/release/desktop"
mkdir -p "$PROJECT_ROOT/release"
STAGING="$(mktemp -d "$OUTPUT-staging.XXXXXX")"
trap 'rm -rf "$STAGING"' EXIT
IMAGE_ROOT="$STAGING/image"
BUNDLE="$IMAGE_ROOT/Veil.app"
mkdir -p "$BUNDLE/Contents/MacOS" "$BUNDLE/Contents/Frameworks" "$BUNDLE/Contents/Resources"
cp "$BIN_DIR/VeilMac" "$BUNDLE/Contents/MacOS/VeilMac"
cp "$VEIL_RUST_LIB_DIR/libveil_macos_bridge.dylib" "$BUNDLE/Contents/Frameworks/"
cp "$APP_DIR/Info.plist" "$BUNDLE/Contents/Info.plist"
cp "$APP_DIR/Assets/Veil.icns" "$BUNDLE/Contents/Resources/Veil.icns"
for resource in "$BIN_DIR/"*.bundle; do
    [[ ! -d "$resource" ]] || cp -R "$resource" "$BUNDLE/Contents/Resources/"
done

LIBRARY_NAME="$(otool -L "$BUNDLE/Contents/MacOS/VeilMac" | awk '/libveil_macos_bridge.dylib/{print $1; exit}')"
install_name_tool -id '@rpath/libveil_macos_bridge.dylib' "$BUNDLE/Contents/Frameworks/libveil_macos_bridge.dylib"
install_name_tool -change "$LIBRARY_NAME" '@rpath/libveil_macos_bridge.dylib' "$BUNDLE/Contents/MacOS/VeilMac"
install_name_tool -delete_rpath "$VEIL_RUST_LIB_DIR" "$BUNDLE/Contents/MacOS/VeilMac"
codesign --force --sign - "$BUNDLE/Contents/Frameworks/libveil_macos_bridge.dylib"
codesign --force --sign - "$BUNDLE"
codesign --verify --deep --strict "$BUNDLE"

VERSION="$(python3 -c 'import plistlib,sys; print(plistlib.load(open(sys.argv[1], "rb"))["CFBundleShortVersionString"])' "$APP_DIR/Info.plist")"
DMG_NAME="Veil_${VERSION}_$(uname -m)"
[[ "$CONFIGURATION" != "debug" ]] || DMG_NAME="${DMG_NAME}_debug"
DMG_NAME="${DMG_NAME}.dmg"
printf '\n创建 DMG 安装包…\n'
xcrun swift -module-cache-path "$APP_DIR/.build/swift-cache" "$PACKAGING_DIR/dmg-background-mac.swift" "$STAGING/background.tiff"
"$DMG_TOOLS/bin/dmgbuild" -s "$PACKAGING_DIR/dmg-layout-mac.py" \
    -D "app=$BUNDLE" -D "background=$STAGING/background.tiff" -D "icon=$APP_DIR/Assets/Veil.icns" \
    Veil "$STAGING/$DMG_NAME"
hdiutil verify "$STAGING/$DMG_NAME"
(cd "$STAGING" && shasum -a 256 "$DMG_NAME" > "$DMG_NAME.sha256")

mkdir -p "$OUTPUT"
mv -f "$STAGING/$DMG_NAME" "$STAGING/$DMG_NAME.sha256" "$OUTPUT/"
# 应用本体留在临时目录，由退出清理处理；同时清理旧脚本遗留的副本。
rm -rf "$OUTPUT/Veil.app"
printf '\n构建成功：%s/%s\n打开 DMG，将 Veil 拖入 Applications 即可安装。\n' "$OUTPUT" "$DMG_NAME"
