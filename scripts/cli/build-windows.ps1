<#
Veil CLI Windows 构建脚本。
所属模块：CLI 发布工具；编译 veil-cli 并将 veil.exe 放入 release/cli/bin。
核心变量：ScriptDir / ProjectRoot 定位仓库，TargetDir 是 Cargo 产物来源，ReleaseDir 是分发目录。
依赖 Cargo 与 PowerShell 文件操作。目标是生成与 Unix 脚本一致的 Windows 发布产物。
#>

param(
    [switch]$Help
)

# PowerShell 命令的非终止错误提升为终止错误；外部 Cargo 退出码仍需显式读取 LASTEXITCODE。
$ErrorActionPreference = "Stop"

# 统一输出成功、进度和失败信息，便于用户区分构建阶段。
# 功能：向终端显示成功信息，不执行构建决策。
# 入参：$args 为未命名参数集合，每项是要展示的文本或可转为文本的对象，插值后合成一条输出。
# 返回：无业务数据，Write-Host 写入宿主显示；错误由 PowerShell 输出环境决定。
# 异常：不主动 throw；注意依赖终端支持颜色，内容不做脱敏。
function Write-Success { Write-Host "[+] $args" -ForegroundColor Green }
# 功能：向终端显示进度信息，不执行构建决策。
# 入参：$args 为未命名参数集合，每项是要展示的文本或可转为文本的对象，插值后合成一条输出。
# 返回：无业务数据，Write-Host 写入宿主显示；错误由 PowerShell 输出环境决定。
# 异常：不主动 throw；注意依赖终端支持颜色，内容不做脱敏。
function Write-Info { Write-Host "[*] $args" -ForegroundColor Cyan }
# 功能：向终端显示失败信息，不执行构建决策。
# 入参：$args 为未命名参数集合，每项是要展示的文本或可转为文本的对象，插值后合成一条输出。
# 返回：无业务数据，Write-Host 写入宿主显示；错误由 PowerShell 输出环境决定。
# 异常：不主动 throw；注意依赖终端支持颜色，内容不做脱敏。
# 此本地同名函数覆盖 PowerShell 内置 Write-Error，仅打印红字，不自行创建终止错误；失败退出由调用处 exit 1 保证。
function Write-Error { Write-Host "[X] $args" -ForegroundColor Red }

if ($Help) {
    Write-Host @"
Veil CLI Build Script

Usage:
    .\build-windows.ps1

What it does:
    1. Builds veil-cli in release mode
    2. Creates release/cli/bin directory
    3. Copies veil.exe to release/cli/bin

Output:
    release/cli/bin/veil.exe

"@ -ForegroundColor White
    exit 0
}

# 从当前脚本位置反推仓库根目录，不依赖调用者的当前路径。
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectRoot = Resolve-Path (Join-Path $ScriptDir "../..")
Set-Location $ProjectRoot

Write-Host "Starting Veil CLI build..." -ForegroundColor White
Write-Host ""

# 构建优化后的 CLI 发布版本；Cargo 非零退出码会进入失败处理。
Write-Info "Building release version..."
try {
    cargo build --release --package veil-cli
    if ($LASTEXITCODE -ne 0) { throw "Cargo build failed" }
} catch {
    Write-Error "Build failed: $_"
    exit 1
}

# 创建 CLI 专属发布目录，避免改动桌面端产物。
Write-Info "Creating release package..."
$ReleaseDir = Join-Path $ProjectRoot "release\cli\bin"
New-Item -ItemType Directory -Path $ReleaseDir -Force | Out-Null

# 复制来源只按环境 CARGO_TARGET_DIR 或仓库 target 推导；不解析 Cargo 配置中的 target-dir 或目标三元组子目录。
# BinarySource 为预期构建文件，BinaryDest 为固定发布文件；存在性检查并不验证来源是本次新生成的版本。
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $ProjectRoot "target" }
$BinarySource = Join-Path $TargetDir "release\veil.exe"
$BinaryDest = Join-Path $ReleaseDir "veil.exe"

if (-not (Test-Path $BinarySource)) {
    Write-Error "Binary not found: $BinarySource"
    exit 1
}

# 强制覆盖同名发布文件；没有临时文件原子发布或并发锁，复制失败不会恢复旧版本。
Copy-Item $BinarySource $BinaryDest -Force

Write-Host ""
Write-Success "Build complete!"
Write-Host ""
Write-Host "Release directory: release\cli\" -ForegroundColor White
Get-Item $BinaryDest | Format-Table Length, LastWriteTime, Name -AutoSize
Write-Host ""

exit 0
