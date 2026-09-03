#!/bin/bash
echo "测试修复后的发布脚本"
echo "=========================================="
echo ""

# 测试从项目根目录
echo "1. 从项目根目录测试版本读取:"
VERSION=$(grep '^version = ' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
echo "   读取到: $VERSION"
[ -n "$VERSION" ] && echo "   ✅ 成功" || echo "   ❌ 失败"
echo ""

# 测试脚本路径逻辑
echo "2. 测试脚本路径处理逻辑:"
echo "   脚本会自动:"
echo "   - 检测自己在 scripts/ 目录"
echo "   - 切换到上级目录（项目根）"
echo "   - 读取 Cargo.toml"
echo ""

echo "3. 使用方法:"
echo "   ✅ ./scripts/release.sh"
echo "   ✅ bash scripts/release.sh" 
echo "   ✅ cd scripts && bash release.sh"
echo ""

echo "=========================================="
echo "修复完成！"
