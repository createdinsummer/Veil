#!/bin/bash
# 工作区功能测试脚本

set -e

VEIL_WS="./target/debug/veil-ws"
TEST_PASSWORD="test-password-123"

echo "================================"
echo "Veil 工作区功能测试"
echo "================================"
echo

# 设置环境变量，避免交互式输入
export VEIL_PASSWORD="$TEST_PASSWORD"

# 1. 初始化容器
echo "1. 初始化容器 'test-container'..."
$VEIL_WS init test-container
echo

# 2. 创建测试文件
echo "2. 创建测试文件..."
echo "Hello, Veil Workspace!" > /tmp/test1.txt
echo "This is a test file." > /tmp/test2.txt
echo "测试中文内容" > /tmp/test3.txt
echo

# 3. 添加文件
echo "3. 添加文件到容器..."
$VEIL_WS add test-container /tmp/test1.txt
$VEIL_WS add test-container /tmp/test2.txt
$VEIL_WS add test-container /tmp/test3.txt
echo

# 4. 列出文件
echo "4. 列出容器中的文件..."
$VEIL_WS list test-container
echo

# 5. 提取文件
echo "5. 提取文件..."
$VEIL_WS extract test-container test1.txt --output /tmp/extracted1.txt
$VEIL_WS extract test-container test2.txt --output /tmp/extracted2.txt
$VEIL_WS extract test-container test3.txt --output /tmp/extracted3.txt
echo

# 6. 验证内容
echo "6. 验证提取的文件内容..."
if diff /tmp/test1.txt /tmp/extracted1.txt > /dev/null; then
    echo "✓ test1.txt 内容正确"
else
    echo "✗ test1.txt 内容不匹配"
    exit 1
fi

if diff /tmp/test2.txt /tmp/extracted2.txt > /dev/null; then
    echo "✓ test2.txt 内容正确"
else
    echo "✗ test2.txt 内容不匹配"
    exit 1
fi

if diff /tmp/test3.txt /tmp/extracted3.txt > /dev/null; then
    echo "✓ test3.txt 内容正确"
else
    echo "✗ test3.txt 内容不匹配"
    exit 1
fi
echo

# 7. 删除文件
echo "7. 删除文件..."
$VEIL_WS rm test-container test2.txt
echo

# 8. 再次列出文件
echo "8. 删除后列出文件..."
$VEIL_WS list test-container
echo

# 9. 查看工作区目录
echo "9. 查看工作区目录结构..."
echo "工作区路径: ~/.veil/workspaces/default/test-container/"
ls -lah ~/.veil/workspaces/default/test-container/ || echo "目录不存在"
echo

# 10. 清理
echo "10. 清理测试文件..."
rm -f /tmp/test1.txt /tmp/test2.txt /tmp/test3.txt
rm -f /tmp/extracted1.txt /tmp/extracted2.txt /tmp/extracted3.txt
echo

echo "================================"
echo "✓ 所有测试通过！"
echo "================================"
