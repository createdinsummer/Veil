# Veil CLI Build Script for Windows
# Corresponds to scripts/unix/build.sh

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
    .\build.ps1

What it does:
    1. Cleans old build artifacts
    2. Builds veil-cli in release mode
    3. Creates release/bin directory
    4. Copies veil.exe to release/bin

Output:
    release/bin/veil.exe

"@ -ForegroundColor White
    exit 0
}

# Get project root directory (scripts/windows -> scripts -> root)
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectRoot = Resolve-Path (Join-Path $ScriptDir "../..")
Set-Location $ProjectRoot

Write-Host "Starting Veil CLI build..." -ForegroundColor White
Write-Host ""

# Clean old build artifacts
Write-Info "Cleaning old build artifacts..."
try {
    cargo clean | Out-Null
} catch {
    Write-Error "Cargo clean failed: $_"
    exit 1
}

# Build release version
Write-Info "Building release version..."
try {
    cargo build --release --package veil-cli
} catch {
    Write-Error "Build failed: $_"
    exit 1
}

# Create release package
Write-Info "Creating release package..."
$ReleaseDir = Join-Path $ProjectRoot "release\bin"
if (Test-Path $ReleaseDir) {
    Remove-Item -Recurse -Force $ReleaseDir
}
New-Item -ItemType Directory -Path $ReleaseDir -Force | Out-Null

# Copy binary
$BinarySource = Join-Path $ProjectRoot "target\release\veil.exe"
$BinaryDest = Join-Path $ReleaseDir "veil.exe"

if (-not (Test-Path $BinarySource)) {
    Write-Error "Binary not found: $BinarySource"
    exit 1
}

Copy-Item $BinarySource $BinaryDest -Force

Write-Host ""
Write-Success "Build complete!"
Write-Host ""
Write-Host "Release directory: release\" -ForegroundColor White
Get-Item $BinaryDest | Format-Table Length, LastWriteTime, Name -AutoSize
Write-Host ""

exit 0
