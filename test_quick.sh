#!/bin/bash
# 简化测试：只测试一个文件的打包和解包

set -e

VEIL_WS="./target/debug/veil-ws"
export VEIL_PASSWORD="test123"

echo "=== 简化测试 ==="

# 清理
rm -rf ~/.veil/workspaces/default/quicktest*
rm -f quicktest.veil

# 1. 初始化
echo "1. 初始化容器..."
$VEIL_WS init quicktest

# 2. 添加一个文件
echo "2. 添加文件..."
echo "Quick test" > /tmp/quick.txt
$VEIL_WS add quicktest /tmp/quick.txt

# 3. 列出
echo "3. 列出文件..."
$VEIL_WS list quicktest

# 4. 打包
echo "4. 打包..."
$VEIL_WS pack quicktest --output quicktest.veil

# 5. 检查文件
if [ -f quicktest.veil ]; then
    echo "✓ 打包成功: $(ls -lh quicktest.veil | awk '{print $5}')"
else
    echo "✗ 打包失败"
    exit 1
fi

# 6. 解包
echo "5. 解包到新容器..."
$VEIL_WS unpack quicktest.veil --name quicktest-unpacked

# 7. 列出解包的文件
echo "6. 列出解包的文件..."
$VEIL_WS list quicktest-unpacked

# 8. 提取并验证
echo "7. 提取并验证..."
$VEIL_WS extract quicktest-unpacked quick.txt --output /tmp/quick-extracted.txt

if diff /tmp/quick.txt /tmp/quick-extracted.txt; then
    echo "✓ 内容验证成功"
else
    echo "✗ 内容不匹配"
    exit 1
fi

# 清理
rm -f /tmp/quick.txt /tmp/quick-extracted.txt quicktest.veil

echo "✓ 所有测试通过！"
