#!/bin/bash
# Veil CLI 打包脚本

set -e

echo "开始打包 Veil CLI..."

# 编译（仅 CLI，跳过测试节省空间）
echo "编译 release 版本..."
cargo build --release --package veil-cli

# 打包
echo "创建发布包..."
rm -rf release
mkdir -p release/bin
mkdir -p release/docs

# 复制文件
cp target/release/veil release/bin/
cp crates/veil-cli/README.md release/docs/CLI_README.md
cp crates/veil-core/README.md release/docs/CORE_README.md
cp INSTALL.md release/

echo ""
echo "✓ 打包完成！"
echo ""
echo "发布目录: release/"
ls -lh release/bin/veil
echo ""
echo "安装命令:"
echo "  sudo cp release/bin/veil /usr/local/bin/"
