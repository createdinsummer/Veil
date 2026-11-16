<#
Veil CLI Windows 安装脚本。
所属模块：Windows 用户级命令行安装；检查 Cargo，调用构建脚本，复制 veil.exe 并验证。
核心变量：ScriptDir / ProjectRoot 定位仓库，BinaryPath 是构建产物，InstallDir 是 Cargo bin 目录。
依赖 PowerShell、Cargo、PATH 查询和文件复制。目标是完成安装并清楚提示用户如何在终端使用。
#>
# 与 Unix 安装入口用途相同；本脚本调用 PowerShell 构建文件，不调用批处理版本。

param(
    [switch]$Help
)

# PowerShell 命令错误按终止错误处理；外部程序的非零状态仍需显式检查，不会只因本设置自动失败。
$ErrorActionPreference = "Stop"

# 以下函数只负责以固定颜色显示进度、成功、失败和提醒，不改变安装状态。
# 功能：以固定颜色显示成功文案，只负责宿主输出。
# 入参：$args 为未命名参数数组，每项是文本或可转为文本的值，插值后合并显示。
# 返回：无业务数据，不向调用方返回安装状态。
# 异常：不主动 throw；输出环境错误由 PowerShell 处理。
# 注意：不校验或脱敏文本；同名本地函数覆盖系统命令时也只打印，退出和失败分支由调用方负责。
function Write-Success { Write-Host "[+] $args" -ForegroundColor Green }
# 功能：以固定颜色显示进度文案，只负责宿主输出。
# 入参：$args 为未命名参数数组，每项是文本或可转为文本的值，插值后合并显示。
# 返回：无业务数据，不向调用方返回安装状态。
# 异常：不主动 throw；输出环境错误由 PowerShell 处理。
# 注意：不校验或脱敏文本；同名本地函数覆盖系统命令时也只打印，退出和失败分支由调用方负责。
function Write-Info { Write-Host "[*] $args" -ForegroundColor Cyan }
# 功能：以固定颜色显示失败文案，只负责宿主输出。
# 入参：$args 为未命名参数数组，每项是文本或可转为文本的值，插值后合并显示。
# 返回：无业务数据，不向调用方返回安装状态。
# 异常：不主动 throw；输出环境错误由 PowerShell 处理。
# 注意：不校验或脱敏文本；同名本地函数覆盖系统命令时也只打印，退出和失败分支由调用方负责。
function Write-Error { Write-Host "[X] $args" -ForegroundColor Red }
# 功能：以固定颜色显示提醒文案，只负责宿主输出。
# 入参：$args 为未命名参数数组，每项是文本或可转为文本的值，插值后合并显示。
# 返回：无业务数据，不向调用方返回安装状态。
# 异常：不主动 throw；输出环境错误由 PowerShell 处理。
# 注意：不校验或脱敏文本；同名本地函数覆盖系统命令时也只打印，退出和失败分支由调用方负责。
function Write-Warning { Write-Host "[!] $args" -ForegroundColor Yellow }

if ($Help) {
    Write-Host @"
Veil Windows Installation Script

Usage:
    .\install-windows.ps1

What it does:
    1. Checks for Rust/Cargo
    2. Calls build-windows.ps1 to build veil
    3. Installs to user .cargo\bin directory
    4. Verifies installation

Requirements:
    - Rust toolchain (will prompt to install if missing)

"@ -ForegroundColor White
    exit 0
}

Write-Host "========================================"
Write-Host "  Veil Windows Installation"
Write-Host "========================================"
Write-Host ""

# 检查 Rust 工具链；找不到 Cargo 时直接停止，避免执行不完整安装。
Write-Info "Checking for Rust/Cargo..."
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "Cargo not found!"
    Write-Host ""
    Write-Host "Please install Rust from: https://rustup.rs/" -ForegroundColor Yellow
    exit 1
}

$cargoVersion = cargo --version
Write-Success "Found Cargo: $cargoVersion"

# 从脚本位置定位仓库并改变当前 PowerShell 工作目录，结束时没有恢复调用者原位置。
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectRoot = Resolve-Path (Join-Path $ScriptDir "../..")
Set-Location $ProjectRoot

# 调用本项目 Windows 构建脚本，安装其 release/cli/bin 下的文件。
Write-Info "Calling build script..."
$BuildScript = Join-Path $ScriptDir "build-windows.ps1"

if (-not (Test-Path $BuildScript)) {
    Write-Error "Build script not found: $BuildScript"
    exit 1
}

try {
    & $BuildScript
    if ($LASTEXITCODE -ne 0) {
        throw "Build script failed"
    }
} catch {
    Write-Error "Build failed: $_"
    exit 1
}

Write-Host ""

# 预期发布文件固定为仓库 release\cli\bin\veil.exe；只检查路径存在，不核验时间戳或二进制版本。
$BinaryPath = Join-Path $ProjectRoot "release\cli\bin\veil.exe"
if (-not (Test-Path $BinaryPath)) {
    Write-Error "Binary not found: $BinaryPath"
    exit 1
}

# 使用 CARGO_HOME 覆盖默认位置，否则安装到当前用户的 .cargo/bin。
$InstallDir = if ($env:CARGO_HOME) {
    "$env:CARGO_HOME\bin"
} else {
    "$env:USERPROFILE\.cargo\bin"
}

if (-not (Test-Path $InstallDir)) {
    Write-Error "Install directory not found: $InstallDir"
    exit 1
}

Write-Info "Install directory: $InstallDir"

# 强制覆盖用户命令目录中的 veil.exe，不做原子替换或备份；复制失败会终止，但不会自动还原旧文件。
Write-Info "Installing veil to $InstallDir..."
Copy-Item $BinaryPath "$InstallDir\veil.exe" -Force
Write-Success "Installation complete!"

# 读取持久化的用户级 Path，与当前进程实际 PATH 不一定相同；用通配符做子串匹配，不拆分路径项。
# 仅打印当前会话和永久配置示例，不执行，也不修改用户或系统环境变量。
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*$InstallDir*") {
    Write-Warning "$InstallDir is not in PATH"
    Write-Host ""
    Write-Host "To add to PATH, run:" -ForegroundColor Yellow
    Write-Host "  `$env:Path += `";$InstallDir`"" -ForegroundColor Cyan
    Write-Host ""
    Write-Host "To make it permanent, run:" -ForegroundColor Yellow
    Write-Host "  [Environment]::SetEnvironmentVariable('Path', `$env:Path + ';$InstallDir', 'User')" -ForegroundColor Cyan
    Write-Host ""
}

# 查询当前 PowerShell 的命令解析结果，可能命中别名、函数或 PATH 中更靠前的旧程序，不与 InstallDir 比较。
# --version 的 LASTEXITCODE 未检查，未找到命令也只警告；末尾固定 exit 0 不代表新安装版本已成功运行。
Write-Info "Verifying installation..."
$veilPath = Get-Command veil -ErrorAction SilentlyContinue

if ($veilPath) {
    Write-Success "veil successfully installed!"
    Write-Host ""
    & veil --version
    Write-Host ""
    Write-Host "Run 'veil --help' to see usage information" -ForegroundColor Green
} else {
    Write-Warning "veil command not found in PATH"
    Write-Host ""
    Write-Host "The binary is installed at: $InstallDir\veil.exe" -ForegroundColor Yellow
    Write-Host "Please restart your terminal or add $InstallDir to PATH" -ForegroundColor Yellow
}

Write-Host ""
Write-Host "Installation complete!" -ForegroundColor Green
Write-Host ""

exit 0
