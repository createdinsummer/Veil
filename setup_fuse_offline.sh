#!/bin/bash
# Veil FUSE 离线安装向导
#
# 使用本地的 macFUSE 安装包进行安装
# 适用于网络受限的环境

set -e

INSTALLER_DIR="installers"
MACFUSE_DMG=$(find "$INSTALLER_DIR" -name "macfuse-*.dmg" -type f 2>/dev/null | head -1)

echo ""
echo "╔══════════════════════════════════════════════════════════╗"
echo "║  🚀 Veil FUSE 挂载 - 离线安装向导                        ║"
echo "╚══════════════════════════════════════════════════════════╝"
echo ""

# 检查 macFUSE 是否已安装
if [ -d "/Library/Filesystems/macfuse.fs" ]; then
    echo "✅ macFUSE 已安装"
    echo ""

    # 检查内核扩展是否已加载
    if kextstat | grep -q "fuse\|osxfuse"; then
        echo "✅ macFUSE 内核扩展已加载"
        echo ""
        echo "🎉 一切就绪！正在编译 veil-fuse..."
        echo ""
        cargo build --release -p veil-fuse
        echo ""
        echo "✅ 编译完成！"
        echo ""
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo ""
        echo "🚀 快速开始："
        echo ""
        echo "  cargo run --release -p veil-fuse --example mount_with_guide dome.veil"
        echo ""
        exit 0
    else
        echo "⚠️  macFUSE 已安装，但内核扩展未加载"
        echo ""
        echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
        echo ""
        echo "📝 需要重启电脑来加载内核扩展："
        echo ""
        echo "  sudo reboot"
        echo ""
        echo "重启后再次运行此脚本。"
        echo ""
        exit 1
    fi
fi

# macFUSE 未安装，使用本地安装包
echo "📦 macFUSE 未安装"
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# 检查本地安装包
if [ -z "$MACFUSE_DMG" ]; then
    echo "❌ 未找到本地安装包"
    echo ""
    echo "请先运行以下命令下载安装包："
    echo "  ./download_macfuse.sh"
    echo ""
    echo "或手动下载并放置到 installers/ 目录："
    echo "  https://github.com/osxfuse/osxfuse/releases"
    echo ""
    exit 1
fi

echo "✅ 找到本地安装包："
echo "   $MACFUSE_DMG"
echo ""
file_size=$(ls -lh "$MACFUSE_DMG" | awk '{print $5}')
echo "   文件大小: $file_size"
echo ""

# 挂载 DMG
echo "📀 挂载安装镜像..."
MOUNT_POINT=$(hdiutil attach "$MACFUSE_DMG" | grep Volumes | awk '{print $3}')

if [ -z "$MOUNT_POINT" ]; then
    echo "❌ 挂载失败"
    exit 1
fi

echo "✅ 已挂载到: $MOUNT_POINT"
echo ""

# 查找 .pkg 文件
PKG_FILE=$(find "$MOUNT_POINT" -name "*.pkg" -type f | head -1)

if [ -z "$PKG_FILE" ]; then
    echo "❌ 未找到安装包文件"
    hdiutil detach "$MOUNT_POINT" 2>/dev/null
    exit 1
fi

echo "📦 找到安装包: $(basename "$PKG_FILE")"
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "💡 安装说明："
echo ""
echo "  1. 即将打开安装程序"
echo "  2. 按提示输入管理员密码"
echo "  3. 在"系统设置"中允许内核扩展（如提示）"
echo "  4. 安装完成后，重启电脑"
echo "  5. 重启后再次运行此脚本"
echo ""
read -p "按 Enter 键开始安装..."
echo ""

# 使用 open 打开安装包（会启动安装程序）
echo "🚀 正在打开安装程序..."
open "$PKG_FILE"

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "⚠️  请按照安装程序的提示完成安装"
echo ""
echo "📝 安装完成后："
echo "  1. 重启电脑：sudo reboot"
echo "  2. 重启后再次运行此脚本：./setup_fuse_offline.sh"
echo ""

# 等待用户确认后卸载 DMG
read -p "按 Enter 键卸载安装镜像..."
hdiutil detach "$MOUNT_POINT" 2>/dev/null || true

echo ""
echo "✅ 安装镜像已卸载"
echo ""
