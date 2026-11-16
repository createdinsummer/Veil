#!/usr/bin/env bash
# Veil CLI Unix 构建脚本。
# 所属模块：跨平台命令行发布工具；编译 veil-cli 并将可执行文件放入 release/cli/bin。
# 核心变量：SCRIPT_DIR / PROJECT_ROOT 定位仓库；OUTPUT 是发布文件位置；TARGET_DIR 支持自定义 Cargo 构建目录。
# 依赖 Bash、Cargo 和文件系统复制能力；目标是把当前环境的 Release 可执行文件复制到固定发布目录。
# 不接收或转发命令行参数，也不显式指定 target；Cargo 配置或环境设置仍可能影响编译目标。
# 命令失败或未定义变量导致脚本退出；已生成的缓存和旧发布文件不会自动清理或回滚。
set -euo pipefail

# 从脚本实际位置推导仓库目录，调用者可从任意工作目录启动；不依赖当前 pwd。
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$PROJECT_ROOT"

printf '构建 Veil CLI 发布版…\n'
# 仅构建命令行包及其依赖，不运行测试，不构建 macOS 或 iOS 客户端。
cargo build --release --package veil-cli

# Cargo 可以把构建缓存放到自定义目录；发布位置固定在仓库 release/cli/bin。
OUTPUT="$PROJECT_ROOT/release/cli/bin"
# 只读取 CARGO_TARGET_DIR 环境变量决定复制来源；若 Cargo 配置单独指定 target-dir/target，本路径不一定与实际产物一致。
TARGET_DIR="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}"
mkdir -p "$OUTPUT"
# 复制到固定名称，已有 veil 会被覆盖；没有临时文件原子替换或并发互斥，失败时发布目录可能保留旧文件。
cp "$TARGET_DIR/release/veil" "$OUTPUT/veil"
printf '\n构建成功：%s/veil\n' "$OUTPUT"
