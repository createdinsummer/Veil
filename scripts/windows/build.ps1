# Veil Windows Build Script
# Build Veil project on Windows

param(
    [switch]$Release,
    [switch]$Test,
    [switch]$Clean,
    [switch]$Help
)

$ErrorActionPreference = "Stop"

# Color output functions
function Write-Success { Write-Host "[+] $args" -ForegroundColor Green }
function Write-Info { Write-Host "[*] $args" -ForegroundColor Cyan }
function Write-Warning { Write-Host "[!] $args" -ForegroundColor Yellow }
function Write-Error { Write-Host "[X] $args" -ForegroundColor Red }

# Show help
function Show-Help {
    Write-Host @"
Veil Windows Build Script

Usage:
    .\build.ps1 [Options]

Options:
    -Release        Build release version (optimized)
    -Test           Run tests after build
    -Clean          Clean build cache
    -Help           Show this help

Examples:
    .\build.ps1                    # Debug build
    .\build.ps1 -Release           # Release build
    .\build.ps1 -Release -Test     # Release build and test
    .\build.ps1 -Clean             # Clean cache

"@ -ForegroundColor White
}

if ($Help) {
    Show-Help
    exit 0
}

# Check Rust installation
Write-Info "Checking Rust environment..."
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "Cargo not found! Please install Rust: https://rustup.rs/"
    exit 1
}

$rustVersion = cargo --version
Write-Success "Found Rust: $rustVersion"

# Clean build cache
if ($Clean) {
    Write-Info "Cleaning build cache..."
    if (Test-Path "target") {
        Remove-Item -Recurse -Force target
        Write-Success "Build cache cleaned"
    } else {
        Write-Info "No cache to clean"
    }
    exit 0
}

# Determine build type
$buildType = if ($Release) { "release" } else { "debug" }
$buildFlag = if ($Release) { "--release" } else { "" }

Write-Info "Building Veil ($buildType mode)..."
Write-Host ""

# Build project
$startTime = Get-Date

try {
    if ($Release) {
        cargo build --release --package veil-cli
    } else {
        cargo build --package veil-cli
    }

    $buildTime = (Get-Date) - $startTime
    Write-Host ""
    Write-Success "Build successful! Time: $($buildTime.TotalSeconds.ToString('0.00'))s"

    # Show binary location
    $binaryPath = "target\$buildType\veil.exe"
    if (Test-Path $binaryPath) {
        $fileSize = (Get-Item $binaryPath).Length / 1MB
        Write-Info "Binary: $binaryPath ($($fileSize.ToString('0.00')) MB)"
    }

} catch {
    Write-Error "Build failed: $_"
    exit 1
}

# Run tests
if ($Test) {
    Write-Host ""
    Write-Info "Running tests..."
    Write-Host ""

    try {
        cargo test --workspace
        Write-Host ""
        Write-Success "All tests passed!"
    } catch {
        Write-Error "Tests failed: $_"
        exit 1
    }
}

# Show next steps
Write-Host ""
Write-Info "Next steps:"
Write-Host "  1. Run program: .\target\$buildType\veil.exe --help"
Write-Host "  2. Run tests: .\build.ps1 -Test"
if (-not $Release) {
    Write-Host "  3. Build release: .\build.ps1 -Release"
}
Write-Host ""
