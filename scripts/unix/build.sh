#!/bin/bash
# Veil CLI 打包脚本

set -e

# 获取项目根目录
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

cd "$PROJECT_ROOT"

echo "开始打包 Veil CLI..."

# 清理旧的编译产物
echo "清理旧的编译产物..."
cargo clean

# 编译（仅 CLI，跳过测试节省空间）
echo "编译 release 版本..."
cargo build --release --package veil-cli

# 打包
echo "创建发布包..."
rm -rf release
mkdir -p release/bin

# 复制文件
cp target/release/veil release/bin/

echo ""
echo "✓ 打包完成！"
echo ""
echo "发布目录: release/"
ls -lh release/bin/veil
