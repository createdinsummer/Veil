# Veil Windows 安装脚本
# 自动安装 Rust 和构建 Veil

param(
    [string]$InstallPath = "$env:LOCALAPPDATA\Veil",
    [switch]$AddToPath,
    [switch]$Help
)

$ErrorActionPreference = "Stop"

# 颜色输出函数
function Write-Success { Write-Host "✓ $args" -ForegroundColor Green }
function Write-Info { Write-Host "→ $args" -ForegroundColor Cyan }
function Write-Warning { Write-Host "⚠ $args" -ForegroundColor Yellow }
function Write-Error { Write-Host "✗ $args" -ForegroundColor Red }

# 显示帮助信息
function Show-Help {
    Write-Host @"
Veil Windows 安装脚本

用法:
    .\install.ps1 [选项]

选项:
    -InstallPath <路径>    指定安装路径（默认: %LOCALAPPDATA%\Veil）
    -AddToPath             自动添加到系统 PATH
    -Help                  显示此帮助信息

示例:
    .\install.ps1                              # 使用默认设置安装
    .\install.ps1 -AddToPath                   # 安装并添加到 PATH
    .\install.ps1 -InstallPath "C:\Tools\Veil" # 自定义安装路径

"@ -ForegroundColor White
}

if ($Help) {
    Show-Help
    exit 0
}

Write-Host @"
╔════════════════════════════════════════╗
║   Veil - 文件加密容器 Windows 安装器   ║
╚════════════════════════════════════════╝
"@ -ForegroundColor Cyan

Write-Host ""

# 检查管理员权限（如果需要添加到 PATH）
if ($AddToPath) {
    $isAdmin = ([Security.Principal.WindowsPrincipal] [Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    if (-not $isAdmin) {
        Write-Warning "添加到 PATH 需要管理员权限"
        Write-Info "请以管理员身份重新运行此脚本，或者手动添加到 PATH"
        $AddToPath = $false
    }
}

# 步骤 1: 检查 Rust
Write-Info "步骤 1/4: 检查 Rust 环境..."

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Warning "未找到 Rust，开始安装..."

    Write-Info "下载 Rustup 安装器..."
    $rustupUrl = "https://win.rustup.rs/x86_64"
    $rustupInstaller = "$env:TEMP\rustup-init.exe"

    try {
        Invoke-WebRequest -Uri $rustupUrl -OutFile $rustupInstaller
        Write-Success "下载完成"

        Write-Info "运行 Rustup 安装器..."
        Write-Host "按照提示完成 Rust 安装..." -ForegroundColor Yellow
        & $rustupInstaller

        # 刷新环境变量
        $env:Path = [System.Environment]::GetEnvironmentVariable("Path", "Machine") + ";" + [System.Environment]::GetEnvironmentVariable("Path", "User")

        if (Get-Command cargo -ErrorAction SilentlyContinue) {
            Write-Success "Rust 安装成功"
        } else {
            Write-Error "Rust 安装失败，请手动安装: https://rustup.rs/"
            exit 1
        }
    } catch {
        Write-Error "下载 Rustup 失败: $_"
        exit 1
    }
} else {
    $rustVersion = cargo --version
    Write-Success "已安装 Rust: $rustVersion"
}

# 步骤 2: 编译 Veil
Write-Host ""
Write-Info "步骤 2/4: 编译 Veil..."

try {
    cargo build --release --package veil-cli
    Write-Success "编译成功"
} catch {
    Write-Error "编译失败: $_"
    exit 1
}

# 步骤 3: 安装文件
Write-Host ""
Write-Info "步骤 3/4: 安装到 $InstallPath..."

# 创建安装目录
if (-not (Test-Path $InstallPath)) {
    New-Item -ItemType Directory -Path $InstallPath -Force | Out-Null
}

# 复制二进制文件
$sourceBinary = "target\release\veil.exe"
$targetBinary = Join-Path $InstallPath "veil.exe"

if (-not (Test-Path $sourceBinary)) {
    Write-Error "未找到编译后的二进制文件: $sourceBinary"
    exit 1
}

Copy-Item $sourceBinary $targetBinary -Force
Write-Success "已安装: $targetBinary"

# 复制文档
$docs = @("README.md", "LICENSE")
foreach ($doc in $docs) {
    if (Test-Path $doc) {
        Copy-Item $doc $InstallPath -Force
    }
}

# 步骤 4: 添加到 PATH（可选）
Write-Host ""
Write-Info "步骤 4/4: 配置环境..."

if ($AddToPath) {
    try {
        $currentPath = [Environment]::GetEnvironmentVariable("Path", "Machine")
        if ($currentPath -notlike "*$InstallPath*") {
            [Environment]::SetEnvironmentVariable("Path", "$currentPath;$InstallPath", "Machine")
            Write-Success "已添加到系统 PATH"
            Write-Info "请重新打开终端使 PATH 生效"
        } else {
            Write-Info "安装路径已在 PATH 中"
        }
    } catch {
        Write-Warning "添加到 PATH 失败: $_"
        Write-Info "请手动添加到 PATH: $InstallPath"
    }
} else {
    Write-Info "未添加到 PATH，请手动添加："
    Write-Host "  setx PATH `"%PATH%;$InstallPath`"" -ForegroundColor Yellow
}

# 创建快捷启动脚本
$launcherScript = @"
@echo off
REM Veil 快速启动脚本
"$targetBinary" %*
"@

$launcherPath = Join-Path $InstallPath "veil.bat"
$launcherScript | Out-File -FilePath $launcherPath -Encoding ASCII
Write-Success "已创建启动脚本: $launcherPath"

# 完成
Write-Host ""
Write-Success "安装完成！"
Write-Host ""
Write-Host "╔════════════════════════════════════════╗" -ForegroundColor Green
Write-Host "║          安装信息                      ║" -ForegroundColor Green
Write-Host "╚════════════════════════════════════════╝" -ForegroundColor Green
Write-Host ""
Write-Host "  安装路径: $InstallPath"
Write-Host "  可执行文件: veil.exe"
Write-Host ""
Write-Host "快速开始:" -ForegroundColor Cyan
Write-Host "  1. 查看帮助: veil --help" -ForegroundColor White
Write-Host "  2. 创建容器: veil init my.veil" -ForegroundColor White
Write-Host "  3. 添加文件: veil add my.veil file.txt" -ForegroundColor White
Write-Host ""
Write-Host "环境变量:" -ForegroundColor Cyan
Write-Host "  VEIL_PASSWORD     - 设置密码避免交互输入" -ForegroundColor White
Write-Host "  VEIL_NEW_PASSWORD - 修改密码时使用" -ForegroundColor White
Write-Host ""

if (-not $AddToPath) {
    Write-Warning "提示: 需要使用完整路径运行 veil，或将其添加到 PATH"
    Write-Host "  添加到 PATH: " -NoNewline
    Write-Host ".\install.ps1 -AddToPath" -ForegroundColor Yellow
}

Write-Host ""
