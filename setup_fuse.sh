#!/bin/bash
# Veil FUSE 安装向导
#
# 这个脚本会：
# 1. 检查 macFUSE 是否已安装
# 2. 如果未安装，引导用户安装
# 3. 安装完成后编译 veil-fuse

set -e

echo ""
echo "╔══════════════════════════════════════════════════════════╗"
echo "║  🚀 Veil FUSE 挂载 - 安装向导                            ║"
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
        echo "  1. 保存所有工作"
        echo "  2. 在终端执行："
        echo "     sudo reboot"
        echo "  3. 重启后再次运行此脚本"
        echo ""
        echo "❓ 为什么需要重启？"
        echo "   macFUSE 是内核扩展，需要重启才能加载到系统中。"
        echo ""
        exit 1
    fi
fi

# macFUSE 未安装，显示安装指南
echo "📦 macFUSE 未安装"
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "💡 什么是 macFUSE？"
echo ""
echo "   macFUSE 允许 Veil 将加密容器挂载为虚拟磁盘。"
echo "   安装后可以："
echo "   • 双击视频秒开（< 1 秒）"
echo "   • 拖动进度条流畅"
echo "   • 内存占用恒定（~64KB）"
echo ""
echo "📥 安装方法："
echo ""

# 检查是否有 Homebrew
if command -v brew &> /dev/null; then
    echo "  ✅ 检测到 Homebrew"
    echo ""
    echo "  推荐使用 Homebrew 安装（一键安装）："
    echo ""
    echo "    brew install --cask macfuse"
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
    read -p "是否现在安装？[y/n]: " -n 1 -r
    echo ""

    if [[ $REPLY =~ ^[Yy]$ ]]; then
        echo ""
        echo "🚀 开始安装 macFUSE..."
        echo ""

        brew install --cask macfuse

        if [ $? -eq 0 ]; then
            echo ""
            echo "✅ macFUSE 安装成功！"
            echo ""
            echo "⚠️  现在需要重启电脑来加载内核扩展"
            echo ""
            read -p "是否现在重启？[y/n]: " -n 1 -r
            echo ""

            if [[ $REPLY =~ ^[Yy]$ ]]; then
                echo "正在重启..."
                sudo reboot
            else
                echo ""
                echo "请手动重启电脑，然后再次运行此脚本："
                echo "  ./setup_fuse.sh"
                echo ""
            fi
        else
            echo ""
            echo "❌ 安装失败，请尝试手动安装"
            echo ""
        fi
    else
        echo ""
        echo "请手动安装后再运行此脚本："
        echo "  brew install --cask macfuse"
        echo "  sudo reboot"
        echo "  ./setup_fuse.sh"
        echo ""
    fi
else
    echo "  ⚠️  未检测到 Homebrew"
    echo ""
    echo "  方法 1：手动下载安装"
    echo ""
    echo "    1. 访问：https://osxfuse.github.io/"
    echo "    2. 下载 macFUSE-4.x.dmg"
    echo "    3. 双击安装"
    echo "    4. 重启电脑"
    echo "    5. 再次运行此脚本"
    echo ""
    echo "  方法 2：先安装 Homebrew，再安装 macFUSE"
    echo ""
    echo "    1. 安装 Homebrew："
    echo "       /bin/bash -c \"\$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\""
    echo ""
    echo "    2. 安装 macFUSE："
    echo "       brew install --cask macfuse"
    echo ""
    echo "    3. 重启电脑"
    echo ""
    echo "    4. 再次运行此脚本："
    echo "       ./setup_fuse.sh"
    echo ""
fi

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "⚠️  安装注意事项："
echo ""
echo "  • 安装过程中需要输入管理员密码"
echo "  • 首次安装需要在"系统设置"中允许内核扩展"
echo "  • 必须重启电脑才能生效"
echo ""
