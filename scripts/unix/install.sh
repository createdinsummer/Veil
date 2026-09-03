#!/usr/bin/env bash

# Veil 安装脚本
# 编译并安装 veil 命令行工具到系统

set -e

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 打印带颜色的消息
info() {
    echo -e "${BLUE}→${NC} $1"
}

success() {
    echo -e "${GREEN}✓${NC} $1"
}

error() {
    echo -e "${RED}✗${NC} $1"
}

warn() {
    echo -e "${YELLOW}!${NC} $1"
}

# 检查 Rust 是否安装
if ! command -v cargo &> /dev/null; then
    error "未检测到 Cargo，请先安装 Rust 工具链"
    echo "访问 https://rustup.rs/ 安装 Rust"
    exit 1
fi

info "检测到 Cargo 版本: $(cargo --version)"

# 获取脚本所在目录
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

cd "$PROJECT_ROOT"

# 调用 build.sh 进行打包
info "调用构建脚本进行打包..."
"$SCRIPT_DIR/build.sh"

# 查找打包产物
BINARY_PATH="release/bin/veil"

if [ ! -f "$BINARY_PATH" ]; then
    error "找不到打包产物: $BINARY_PATH"
    exit 1
fi

# 获取安装目标路径
INSTALL_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"

if [ ! -d "$INSTALL_DIR" ]; then
    error "安装目录不存在: $INSTALL_DIR"
    exit 1
fi

info "安装目录: $INSTALL_DIR"

# 安装二进制文件
info "正在安装 veil 到 $INSTALL_DIR..."
cp "$BINARY_PATH" "$INSTALL_DIR/veil"
chmod +x "$INSTALL_DIR/veil"

success "安装完成！"

# 检查 PATH
if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
    warn "$INSTALL_DIR 不在 PATH 中"
    echo "请将以下内容添加到你的 shell 配置文件 (~/.bashrc 或 ~/.zshrc):"
    echo ""
    echo "  export PATH=\"\$PATH:$INSTALL_DIR\""
    echo ""
fi

# 验证安装
info "验证安装..."
if command -v veil &> /dev/null; then
    success "veil 已成功安装"
    echo ""
    veil --version
    echo ""
    echo "运行 'veil --help' 查看使用说明"
else
    warn "veil 命令未在 PATH 中找到"
    echo "请重新加载 shell 配置或手动添加 $INSTALL_DIR 到 PATH"
fi
