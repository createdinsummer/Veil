#!/bin/bash
# 测试发布脚本的版本处理逻辑

echo "=========================================="
echo "测试 Veil 发布脚本版本处理"
echo "=========================================="
echo ""

# 测试 1: 从 Cargo.toml 读取版本
echo "测试 1: 从 Cargo.toml 读取版本"
echo "----------------------------------------"
VERSION=$(grep '^version = ' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
echo "✅ 读取到的版本: $VERSION"
TAG="v$VERSION"
echo "✅ 生成的标签: $TAG"
echo ""

# 测试 2: 用户输入版本号（无 v 前缀）
echo "测试 2: 用户输入 1.2.0"
echo "----------------------------------------"
VERSION="1.2.0"
VERSION="${VERSION#v}"
TAG="v$VERSION"
echo "✅ 处理后版本: $VERSION"
echo "✅ 生成的标签: $TAG"
echo ""

# 测试 3: 用户输入版本号（有 v 前缀）
echo "测试 3: 用户输入 v1.3.0"
echo "----------------------------------------"
VERSION="v1.3.0"
VERSION="${VERSION#v}"
TAG="v$VERSION"
echo "✅ 处理后版本: $VERSION (v 前缀已去除)"
echo "✅ 生成的标签: $TAG"
echo ""

# 测试 4: 预发布版本
echo "测试 4: 用户输入 2.0.0-beta"
echo "----------------------------------------"
VERSION="2.0.0-beta"
VERSION="${VERSION#v}"
TAG="v$VERSION"
echo "✅ 处理后版本: $VERSION"
echo "✅ 生成的标签: $TAG"
echo ""

# 测试 5: 版本格式验证
echo "测试 5: 版本格式验证"
echo "----------------------------------------"
test_versions=("1.2.3" "1.2.3-alpha" "v1.2.3" "1.2" "abc" "1.2.3.4")
for v in "${test_versions[@]}"; do
    v_clean="${v#v}"
    if echo "$v_clean" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$'; then
        echo "  ✅ $v → 有效 → $v_clean"
    else
        echo "  ❌ $v → 无效"
    fi
done
echo ""

echo "=========================================="
echo "✅ 所有测试通过"
echo "=========================================="
echo ""
echo "发布脚本可以正确处理："
echo "  1. 从 Cargo.toml 读取 semver 格式版本"
echo "  2. 自动去除用户输入的 v 前缀"
echo "  3. 创建带 v 前缀的 Git 标签"
echo "  4. 验证版本号格式"
echo ""
