#!/bin/bash
# macFUSE 离线安装包下载脚本
#
# 这个脚本会下载 macFUSE 安装包到 installers/ 目录
# 供网络受限的用户使用

set -e

INSTALLER_DIR="installers"
MACFUSE_VERSION="4.7.1"
MACFUSE_URL="https://github.com/osxfuse/osxfuse/releases/download/macfuse-${MACFUSE_VERSION}/macfuse-${MACFUSE_VERSION}.dmg"
MACFUSE_FILE="${INSTALLER_DIR}/macfuse-${MACFUSE_VERSION}.dmg"

echo ""
echo "╔══════════════════════════════════════════════════════════╗"
echo "║  📦 macFUSE 离线安装包下载工具                          ║"
echo "╚══════════════════════════════════════════════════════════╝"
echo ""

# 创建 installers 目录
if [ ! -d "$INSTALLER_DIR" ]; then
    echo "📁 创建 $INSTALLER_DIR 目录..."
    mkdir -p "$INSTALLER_DIR"
fi

# 检查是否已存在
if [ -f "$MACFUSE_FILE" ]; then
    echo "✅ 安装包已存在: $MACFUSE_FILE"
    echo ""
    file_size=$(ls -lh "$MACFUSE_FILE" | awk '{print $5}')
    echo "   文件大小: $file_size"
    echo ""
    read -p "是否重新下载？[y/n]: " -n 1 -r
    echo ""
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        echo "跳过下载。"
        exit 0
    fi
    rm "$MACFUSE_FILE"
fi

# 下载安装包
echo "⬇️  正在下载 macFUSE ${MACFUSE_VERSION}..."
echo "   URL: $MACFUSE_URL"
echo ""

if command -v curl &> /dev/null; then
    curl -L -o "$MACFUSE_FILE" "$MACFUSE_URL" \
        --progress-bar \
        --fail \
        --retry 3 \
        --retry-delay 2
elif command -v wget &> /dev/null; then
    wget -O "$MACFUSE_FILE" "$MACFUSE_URL" \
        --show-progress \
        --tries=3 \
        --waitretry=2
else
    echo "❌ 错误: 未找到 curl 或 wget"
    echo "   请手动下载："
    echo "   $MACFUSE_URL"
    echo ""
    exit 1
fi

# 验证下载
if [ -f "$MACFUSE_FILE" ]; then
    file_size=$(ls -lh "$MACFUSE_FILE" | awk '{print $5}')
    echo ""
    echo "✅ 下载完成！"
    echo "   文件: $MACFUSE_FILE"
    echo "   大小: $file_size"
    echo ""

    # 添加到 .gitignore
    if ! grep -q "^installers/" .gitignore 2>/dev/null; then
        echo "installers/" >> .gitignore
        echo "📝 已添加 installers/ 到 .gitignore"
    fi

    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    echo "📦 离线安装包已准备好！"
    echo ""
    echo "🎁 分发给用户时，包含以下文件："
    echo "   • installers/macfuse-${MACFUSE_VERSION}.dmg"
    echo "   • setup_fuse_offline.sh"
    echo ""
    echo "💡 用户使用方法："
    echo "   ./setup_fuse_offline.sh"
    echo ""
else
    echo "❌ 下载失败"
    exit 1
fi
