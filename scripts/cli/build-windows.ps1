# Veil CLI Build Script for Windows
# Corresponds to scripts/cli/build-unix.sh

param(
    [switch]$Help
)

$ErrorActionPreference = "Stop"

# Color output functions
function Write-Success { Write-Host "[+] $args" -ForegroundColor Green }
function Write-Info { Write-Host "[*] $args" -ForegroundColor Cyan }
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

# Get project root directory (scripts/cli -> scripts -> root)
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectRoot = Resolve-Path (Join-Path $ScriptDir "../..")
Set-Location $ProjectRoot

Write-Host "Starting Veil CLI build..." -ForegroundColor White
Write-Host ""

# Build release version
Write-Info "Building release version..."
try {
    cargo build --release --package veil-cli
    if ($LASTEXITCODE -ne 0) { throw "Cargo build failed" }
} catch {
    Write-Error "Build failed: $_"
    exit 1
}

# Create release package
Write-Info "Creating release package..."
$ReleaseDir = Join-Path $ProjectRoot "release\cli\bin"
New-Item -ItemType Directory -Path $ReleaseDir -Force | Out-Null

# Copy binary
$TargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $ProjectRoot "target" }
$BinarySource = Join-Path $TargetDir "release\veil.exe"
$BinaryDest = Join-Path $ReleaseDir "veil.exe"

if (-not (Test-Path $BinarySource)) {
    Write-Error "Binary not found: $BinarySource"
    exit 1
}

Copy-Item $BinarySource $BinaryDest -Force

Write-Host ""
Write-Success "Build complete!"
Write-Host ""
Write-Host "Release directory: release\cli\" -ForegroundColor White
Get-Item $BinaryDest | Format-Table Length, LastWriteTime, Name -AutoSize
Write-Host ""

exit 0
