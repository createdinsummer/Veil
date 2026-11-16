#!/usr/bin/env bash
# Veil CLI Unix 安装脚本。
# 所属模块：命令行客户端本机安装；先构建，再复制二进制到 Cargo 用户 bin 目录。
# 核心变量：SCRIPT_DIR / PROJECT_ROOT 定位项目；BINARY_PATH 指向发布产物；INSTALL_DIR 指向最终安装目录。
# 依赖 Bash、Cargo、PATH、cp 和 chmod。目标是安装用户级 veil 命令并提示如何让 shell 找到它。

# 简单命令失败时退出，但不自动撤销已经复制的文件；没有启用未定义变量检查或管道整体失败检查。
set -e

# 终端显示用 ANSI 转义序列，不保存安装配置；输出到不支持颜色的终端时仍会原样发送。
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 以下函数仅统一命令行反馈颜色，不参与构建或安装决策。
# 功能：输出构建或安装进度，只负责终端展示，不改变安装流程。
# 入参：$1 为说明文本（字符串），仅使用第一个参数；echo -e 会解释其中的反斜杠转义。
# 返回：echo 的退出状态，正常为 0；无数据返回。
# 异常：Shell 不抛出语言异常，输出失败可返回非零状态。
# 注意：不校验或脱敏输入；当前脚本的 set -e 可使输出失败终止安装。
info() {
    echo -e "${BLUE}→${NC} $1"
}

# 功能：输出成功提示，只负责终端展示，不改变安装流程。
# 入参：$1 为说明文本（字符串），仅使用第一个参数；echo -e 会解释其中的反斜杠转义。
# 返回：echo 的退出状态，正常为 0；无数据返回。
# 异常：Shell 不抛出语言异常，输出失败可返回非零状态。
# 注意：不校验或脱敏输入；当前脚本的 set -e 可使输出失败终止安装。
success() {
    echo -e "${GREEN}✓${NC} $1"
}

# 功能：输出失败提示，只负责终端展示，不改变安装流程。
# 入参：$1 为说明文本（字符串），仅使用第一个参数；echo -e 会解释其中的反斜杠转义。
# 返回：echo 的退出状态，正常为 0；无数据返回。
# 异常：Shell 不抛出语言异常，输出失败可返回非零状态。
# 注意：不校验或脱敏输入；当前脚本的 set -e 可使输出失败终止安装。
error() {
    echo -e "${RED}✗${NC} $1"
}

# 功能：输出需用户注意的提示，只负责终端展示，不改变安装流程。
# 入参：$1 为说明文本（字符串），仅使用第一个参数；echo -e 会解释其中的反斜杠转义。
# 返回：echo 的退出状态，正常为 0；无数据返回。
# 异常：Shell 不抛出语言异常，输出失败可返回非零状态。
# 注意：不校验或脱敏输入；当前脚本的 set -e 可使输出失败终止安装。
warn() {
    echo -e "${YELLOW}!${NC} $1"
}

# 先确认 Cargo 可用，避免开始构建后才因缺少工具链失败。
if ! command -v cargo &> /dev/null; then
    error "未检测到 Cargo，请先安装 Rust 工具链"
    echo "访问 https://rustup.rs/ 安装 Rust"
    exit 1
fi

info "检测到 Cargo 版本: $(cargo --version)"

# 从脚本位置定位仓库，随后以仓库为工作目录；调用者可从其他目录启动。
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

cd "$PROJECT_ROOT"

# 复用统一构建脚本，确保安装使用和发布目录中的同一产物。
info "调用构建脚本进行打包..."
"$SCRIPT_DIR/build-unix.sh"

# 统一构建脚本发布的相对路径，在上方切换到仓库目录后解析。
BINARY_PATH="release/cli/bin/veil"

if [ ! -f "$BINARY_PATH" ]; then
    error "找不到打包产物: $BINARY_PATH"
    exit 1
fi

# 优先使用非空 CARGO_HOME，否则使用用户 .cargo；脚本不创建 bin，目录缺失时直接失败。
INSTALL_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"

if [ ! -d "$INSTALL_DIR" ]; then
    error "安装目录不存在: $INSTALL_DIR"
    exit 1
fi

info "安装目录: $INSTALL_DIR"

# 覆盖用户 bin 中同名 veil，再添加执行位；不是原子替换，复制或 chmod 失败不会恢复旧版本。
info "正在安装 veil 到 $INSTALL_DIR..."
cp "$BINARY_PATH" "$INSTALL_DIR/veil"
chmod +x "$INSTALL_DIR/veil"

success "安装完成！"

# 安装目录未加入 PATH 时给出 shell 配置示例，安装本身仍保留成功结果。
if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
    warn "$INSTALL_DIR 不在 PATH 中"
    echo "请将以下内容添加到你的 shell 配置文件 (~/.bashrc 或 ~/.zshrc):"
    echo ""
    echo "  export PATH=\"\$PATH:$INSTALL_DIR\""
    echo ""
fi

# 按当前 PATH 解析 veil 并执行 --version；没有对比解析结果与本次安装位置，可能命中 PATH 中更靠前的旧版本。
# 未命中只警告，不修改当前 PATH 或 shell 配置文件；本脚本不接收安装目录位置参数。
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
