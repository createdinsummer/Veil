#!/usr/bin/env bash
# 构建并打包 Veil CLI，保留其他客户端产物和构建缓存。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$PROJECT_ROOT"

printf '构建 Veil CLI 发布版…\n'
cargo build --release --package veil-cli

OUTPUT="$PROJECT_ROOT/release/cli/bin"
TARGET_DIR="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
mkdir -p "$OUTPUT"
cp "$TARGET_DIR/release/veil" "$OUTPUT/veil"
printf '\n构建成功：%s/veil\n' "$OUTPUT"
