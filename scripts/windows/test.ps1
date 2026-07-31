# Veil Windows 测试脚本
# 运行所有测试并生成报告

param(
    [switch]$Unit,
    [switch]$Integration,
    [switch]$Core,
    [switch]$CLI,
    [switch]$Verbose,
    [switch]$Coverage,
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
Veil Windows 测试脚本

用法:
    .\test.ps1 [选项]

选项:
    -Unit           只运行单元测试
    -Integration    只运行集成测试
    -Core           只测试 veil-core
    -CLI            只测试 veil-cli
    -Verbose        显示详细输出
    -Coverage       生成代码覆盖率报告（需要安装 tarpaulin）
    -Help           显示此帮助信息

示例:
    .\test.ps1                      # 运行所有测试
    .\test.ps1 -Core                # 只测试核心库
    .\test.ps1 -CLI -Verbose        # 测试 CLI 并显示详细输出
    .\test.ps1 -Unit                # 只运行单元测试

"@ -ForegroundColor White
}

if ($Help) {
    Show-Help
    exit 0
}

Write-Host @"
╔════════════════════════════════════════╗
║      Veil 测试套件 - Windows 版       ║
╚════════════════════════════════════════╝
"@ -ForegroundColor Cyan

Write-Host ""

# 检查 Rust 环境
Write-Info "检查 Rust 环境..."
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "未找到 Cargo！请先安装 Rust: https://rustup.rs/"
    exit 1
}

$rustVersion = cargo --version
Write-Success "Rust 版本: $rustVersion"
Write-Host ""

# 构建测试命令
$testCommands = @()

if ($Core) {
    # 只测试核心库
    if ($Unit) {
        $testCommands += "cargo test --package veil-core --lib"
    } elseif ($Integration) {
        $testCommands += "cargo test --package veil-core --test container_tests"
        $testCommands += "cargo test --package veil-core --test index_tests"
    } else {
        $testCommands += "cargo test --package veil-core"
    }
} elseif ($CLI) {
    # 只测试 CLI
    if ($Integration) {
        $testCommands += "cargo test --package veil-cli --test integration_tests"
        $testCommands += "cargo test --package veil-cli --test shell_tests"
        $testCommands += "cargo test --package veil-cli --test commands_tests"
    } else {
        $testCommands += "cargo test --package veil-cli"
    }
} else {
    # 测试所有组件
    $testCommands += "cargo test --workspace"
}

# 添加详细输出标志
$verboseFlag = if ($Verbose) { "--verbose" } else { "" }

# 运行测试
$totalTests = 0
$passedTests = 0
$failedTests = 0
$ignoredTests = 0
$startTime = Get-Date

foreach ($cmd in $testCommands) {
    $fullCmd = "$cmd $verboseFlag"
    Write-Info "执行: $fullCmd"
    Write-Host ""

    try {
        $output = Invoke-Expression $fullCmd 2>&1 | Out-String
        Write-Host $output

        # 解析测试结果
        if ($output -match "(\d+) passed.*?(\d+) failed.*?(\d+) ignored") {
            $passedTests += [int]$Matches[1]
            $failedTests += [int]$Matches[2]
            $ignoredTests += [int]$Matches[3]
            $totalTests += [int]$Matches[1] + [int]$Matches[2] + [int]$Matches[3]
        }

    } catch {
        Write-Error "测试失败: $_"
        $failedTests++
    }

    Write-Host ""
}

$testTime = (Get-Date) - $startTime

# 显示测试总结
Write-Host "╔════════════════════════════════════════╗" -ForegroundColor Cyan
Write-Host "║            测试总结                    ║" -ForegroundColor Cyan
Write-Host "╚════════════════════════════════════════╝" -ForegroundColor Cyan
Write-Host ""
Write-Host "  总测试数: $totalTests"
Write-Host "  通过: " -NoNewline
Write-Host "$passedTests" -ForegroundColor Green
Write-Host "  失败: " -NoNewline
if ($failedTests -eq 0) {
    Write-Host "$failedTests" -ForegroundColor Green
} else {
    Write-Host "$failedTests" -ForegroundColor Red
}
Write-Host "  忽略: " -NoNewline
Write-Host "$ignoredTests" -ForegroundColor Yellow
Write-Host "  耗时: $($testTime.TotalSeconds.ToString('0.00')) 秒"
Write-Host ""

# 显示通过率
if ($totalTests -gt 0) {
    $passRate = ($passedTests / $totalTests) * 100
    Write-Host "  通过率: " -NoNewline
    if ($passRate -eq 100) {
        Write-Host "$($passRate.ToString('0.0'))%" -ForegroundColor Green
    } elseif ($passRate -ge 90) {
        Write-Host "$($passRate.ToString('0.0'))%" -ForegroundColor Yellow
    } else {
        Write-Host "$($passRate.ToString('0.0'))%" -ForegroundColor Red
    }
}

Write-Host ""

# 代码覆盖率（可选）
if ($Coverage) {
    Write-Info "生成代码覆盖率报告..."

    if (Get-Command cargo-tarpaulin -ErrorAction SilentlyContinue) {
        try {
            cargo tarpaulin --out Html --output-dir coverage
            Write-Success "覆盖率报告已生成: coverage\tarpaulin-report.html"
        } catch {
            Write-Warning "生成覆盖率报告失败: $_"
        }
    } else {
        Write-Warning "未安装 cargo-tarpaulin，跳过覆盖率报告"
        Write-Info "安装: cargo install cargo-tarpaulin"
    }
}

# 退出码
if ($failedTests -eq 0) {
    Write-Success "所有测试通过！"
    exit 0
} else {
    Write-Error "有 $failedTests 个测试失败"
    exit 1
}
