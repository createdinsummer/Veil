#!/bin/bash
# Veil CLI 打包脚本

set -e

echo "开始打包 Veil CLI..."

# 编译（仅 CLI，跳过测试节省空间）
echo "编译 release 版本..."
cargo build --release --package veil-cli

echo ""
echo "✓ 打包完成！"
echo ""
echo "二进制文件位置: target/release/veil"
ls -lh target/release/veil
