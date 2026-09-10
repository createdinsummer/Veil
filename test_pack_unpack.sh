#!/bin/bash
# 工作区打包/解包功能测试脚本

set -e

VEIL_WS="./target/debug/veil-ws"
TEST_PASSWORD="test-password-123"

echo "================================"
echo "Veil Pack/Unpack 功能测试"
echo "================================"
echo

# 设置环境变量，避免交互式输入
export VEIL_PASSWORD="$TEST_PASSWORD"

# 清理之前的测试数据
rm -rf ~/.veil/workspaces/default/test-pack
rm -rf ~/.veil/workspaces/default/test-unpack
rm -f test-pack.veil

# 1. 初始化容器
echo "1. 初始化容器 'test-pack'..."
$VEIL_WS init test-pack
echo

# 2. 创建测试文件
echo "2. 创建测试文件..."
mkdir -p /tmp/veil-test
echo "Pack Test File 1" > /tmp/veil-test/file1.txt
echo "Pack Test File 2" > /tmp/veil-test/file2.txt
echo "打包测试文件 3" > /tmp/veil-test/file3.txt
dd if=/dev/urandom of=/tmp/veil-test/binary.dat bs=1024 count=10 2>/dev/null
echo

# 3. 添加文件到容器
echo "3. 添加文件到容器..."
$VEIL_WS add test-pack /tmp/veil-test/file1.txt
$VEIL_WS add test-pack /tmp/veil-test/file2.txt
$VEIL_WS add test-pack /tmp/veil-test/file3.txt
$VEIL_WS add test-pack /tmp/veil-test/binary.dat
echo

# 4. 列出文件
echo "4. 列出容器中的文件..."
$VEIL_WS list test-pack
echo

# 5. 打包容器
echo "5. 打包容器到 .veil 文件..."
$VEIL_WS pack test-pack --output test-pack.veil
echo

# 6. 验证打包文件
echo "6. 验证打包文件..."
if [ -f test-pack.veil ]; then
    SIZE=$(ls -lh test-pack.veil | awk '{print $5}')
    echo "✓ 打包文件已创建: test-pack.veil ($SIZE)"
else
    echo "✗ 打包文件未创建"
    exit 1
fi
echo

# 7. 解包到新容器
echo "7. 解包到新容器 'test-unpack'..."
$VEIL_WS unpack test-pack.veil --name test-unpack
echo

# 8. 列出解包后的文件
echo "8. 列出解包后的文件..."
$VEIL_WS list test-unpack
echo

# 9. 提取文件并验证
echo "9. 提取文件并验证内容..."
$VEIL_WS extract test-unpack file1.txt --output /tmp/veil-test/extracted1.txt
$VEIL_WS extract test-unpack file2.txt --output /tmp/veil-test/extracted2.txt
$VEIL_WS extract test-unpack file3.txt --output /tmp/veil-test/extracted3.txt
$VEIL_WS extract test-unpack binary.dat --output /tmp/veil-test/extracted-binary.dat

if diff /tmp/veil-test/file1.txt /tmp/veil-test/extracted1.txt > /dev/null; then
    echo "✓ file1.txt 内容正确"
else
    echo "✗ file1.txt 内容不匹配"
    exit 1
fi

if diff /tmp/veil-test/file2.txt /tmp/veil-test/extracted2.txt > /dev/null; then
    echo "✓ file2.txt 内容正确"
else
    echo "✗ file2.txt 内容不匹配"
    exit 1
fi

if diff /tmp/veil-test/file3.txt /tmp/veil-test/extracted3.txt > /dev/null; then
    echo "✓ file3.txt 内容正确"
else
    echo "✗ file3.txt 内容不匹配"
    exit 1
fi

if diff /tmp/veil-test/binary.dat /tmp/veil-test/extracted-binary.dat > /dev/null; then
    echo "✓ binary.dat 内容正确"
else
    echo "✗ binary.dat 内容不匹配"
    exit 1
fi
echo

# 10. 清理
echo "10. 清理测试文件..."
rm -rf /tmp/veil-test
rm -f test-pack.veil
echo

echo "================================"
echo "✓ Pack/Unpack 测试通过！"
echo "================================"
