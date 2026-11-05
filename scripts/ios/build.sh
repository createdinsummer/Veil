#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/../.."
platform="${1:-simulator}"
case "$platform" in
  simulator) target=aarch64-apple-ios-sim; sdk=iphonesimulator ;;
  device) target=aarch64-apple-ios; sdk=iphoneos ;;
  *) echo "用法：$0 [simulator|device]" >&2; exit 1 ;;
esac
export IPHONEOS_DEPLOYMENT_TARGET=17.0
cargo build --release -p veil-macos-bridge --lib --target "$target"
if [ ! -d apps/veil-ios/VeilIOS.xcodeproj ]; then
  (cd apps/veil-ios && xcodegen generate)
fi
xcodebuild -project apps/veil-ios/VeilIOS.xcodeproj -scheme VeilIOS -configuration Release -sdk "$sdk" -derivedDataPath /tmp/veil-ios-build \
  'LIBRARY_SEARCH_PATHS=$(inherited) "$(SRCROOT)/../../target/$(RUST_TARGET)/release"' \
  'OTHER_LDFLAGS="$(SRCROOT)/../../target/$(RUST_TARGET)/release/libveil_macos_bridge.a" -lc++ -lresolv' \
  CODE_SIGNING_ALLOWED=NO ARCHS=arm64 build
