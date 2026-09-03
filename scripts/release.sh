#!/bin/bash
# Veil 版本发布脚本（macOS/Linux）
# 用法: ./scripts/release.sh [版本号]

set -e

# 获取脚本所在目录的绝对路径
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# 项目根目录
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# 切换到项目根目录
cd "$PROJECT_ROOT"

# 颜色输出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 打印函数
print_info() {
    echo -e "${BLUE}ℹ️  $1${NC}"
}

print_success() {
    echo -e "${GREEN}✅ $1${NC}"
}

print_warning() {
    echo -e "${YELLOW}⚠️  $1${NC}"
}

print_error() {
    echo -e "${RED}❌ $1${NC}"
}

# 检查是否在 git 仓库中
if ! git rev-parse --git-dir > /dev/null 2>&1; then
    print_error "当前目录不是 Git 仓库"
    exit 1
fi

# 获取版本号
if [ -n "$1" ]; then
    VERSION="$1"
    # 如果用户输入了 v 前缀，去掉它
    VERSION="${VERSION#v}"
else
    # 从 Cargo.toml 读取当前版本
    VERSION=$(grep '^version = ' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
    print_info "从 Cargo.toml 读取到版本: $VERSION"
    echo ""
    read -p "使用此版本发布? (y/n) [y]: " confirm
    confirm=${confirm:-y}
    if [ "$confirm" != "y" ] && [ "$confirm" != "Y" ]; then
        echo ""
        read -p "请输入新版本号 (如 1.2.0): " VERSION
        # 去掉可能的 v 前缀
        VERSION="${VERSION#v}"
    fi
fi

# 验证版本号格式
if ! echo "$VERSION" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$'; then
    print_error "无效的版本号格式: $VERSION"
    print_info "格式应为: X.Y.Z 或 X.Y.Z-suffix (如 1.2.0 或 1.2.0-beta)"
    exit 1
fi

TAG="v$VERSION"

echo ""
echo "=========================================="
echo "  Veil 版本发布"
echo "=========================================="
echo ""
print_info "版本号: $VERSION"
print_info "Git 标签: $TAG"
echo ""

# 检查标签是否已存在
if git rev-parse "$TAG" >/dev/null 2>&1; then
    print_error "标签 $TAG 已存在"
    echo ""
    read -p "是否删除旧标签并重新创建? (y/n) [n]: " recreate
    recreate=${recreate:-n}
    if [ "$recreate" == "y" ] || [ "$recreate" == "Y" ]; then
        print_info "删除本地标签..."
        git tag -d "$TAG"
        print_info "删除远程标签..."
        git push origin --delete "$TAG" 2>/dev/null || true
        print_success "旧标签已删除"
    else
        print_error "发布已取消"
        exit 1
    fi
fi

# 检查工作区状态
if ! git diff-index --quiet HEAD --; then
    print_warning "工作区有未提交的更改"
    git status --short
    echo ""
    read -p "是否继续? (y/n) [n]: " continue
    continue=${continue:-n}
    if [ "$continue" != "y" ] && [ "$continue" != "Y" ]; then
        print_error "发布已取消"
        exit 1
    fi
fi

echo ""
print_info "当前分支: $(git branch --show-current)"
echo ""

# 确认发布
read -p "确认发布版本 $VERSION? (y/n) [n]: " confirm
confirm=${confirm:-n}
if [ "$confirm" != "y" ] && [ "$confirm" != "Y" ]; then
    print_error "发布已取消"
    exit 1
fi

echo ""
echo "=========================================="
echo "  开始发布流程"
echo "=========================================="
echo ""

# 步骤 1: 运行测试
print_info "步骤 1/5: 运行测试..."
if cargo test --all --quiet; then
    print_success "所有测试通过"
else
    print_error "测试失败"
    exit 1
fi

# 步骤 2: 构建 Release 版本
print_info "步骤 2/5: 构建 Release 版本..."
if cargo build --release --all --quiet; then
    print_success "构建成功"
else
    print_error "构建失败"
    exit 1
fi

# 步骤 3: 创建 Git 标签
print_info "步骤 3/5: 创建 Git 标签..."
git tag -a "$TAG" -m "Release $VERSION"
print_success "标签 $TAG 已创建"

# 步骤 4: 推送到远程
print_info "步骤 4/5: 推送到远程仓库..."
echo ""

# 获取所有远程仓库
REMOTES=$(git remote)
if [ -z "$REMOTES" ]; then
    print_error "没有配置远程仓库"
    print_warning "标签 $TAG 已创建但未推送"
    print_info "请先配置远程仓库: git remote add <name> <url>"
    exit 1
fi

# 显示远程仓库列表
print_info "检测到以下远程仓库:"
for remote in $REMOTES; do
    url=$(git remote get-url "$remote")
    echo "  - $remote: $url"
done
echo ""

BRANCH=$(git branch --show-current)
print_warning "即将推送到所有远程仓库:"
echo "  分支: $BRANCH"
echo "  标签: $TAG"
echo ""
read -p "确认推送? (y/n) [y]: " push_confirm
push_confirm=${push_confirm:-y}

if [ "$push_confirm" == "y" ] || [ "$push_confirm" == "Y" ]; then
    failed_remotes=()

    for remote in $REMOTES; do
        print_info "推送到 $remote..."

        # 推送代码
        if git push "$remote" "$BRANCH" 2>&1; then
            print_success "  代码已推送到 $remote"
        else
            print_error "  代码推送到 $remote 失败"
            failed_remotes+=("$remote")
            continue
        fi

        # 推送标签
        if git push "$remote" "$TAG" 2>&1; then
            print_success "  标签已推送到 $remote"
        else
            print_error "  标签推送到 $remote 失败"
            failed_remotes+=("$remote")
        fi

        echo ""
    done

    # 检查是否有失败的
    if [ ${#failed_remotes[@]} -gt 0 ]; then
        print_warning "部分远程仓库推送失败: ${failed_remotes[*]}"
        print_info "可手动重试:"
        for remote in "${failed_remotes[@]}"; do
            echo "  git push $remote $BRANCH"
            echo "  git push $remote $TAG"
        done
    else
        print_success "所有远程仓库推送成功"
    fi
else
    print_warning "推送已跳过"
    print_info "可稍后手动推送到所有远程仓库:"
    for remote in $REMOTES; do
        echo "  git push $remote $BRANCH"
        echo "  git push $remote $TAG"
    done
fi

# 步骤 5: 完成
echo ""
echo "=========================================="
print_success "发布完成！"
echo "=========================================="
echo ""
print_info "版本: $VERSION"
print_info "标签: $TAG"
echo ""
print_info "GitHub Actions 将自动构建并发布二进制包"
print_info "查看进度: https://github.com/YOUR_USERNAME/Veil/actions"
echo ""
print_info "发布页面: https://github.com/YOUR_USERNAME/Veil/releases/tag/$TAG"
echo ""
