# Veil Windows Install Script (Simplified)
# Corresponds to install.sh for Linux/macOS
# Compiles and installs veil to user directory

param(
    [switch]$Help
)

$ErrorActionPreference = "Stop"

# Color output functions
function Write-Success { Write-Host "[+] $args" -ForegroundColor Green }
function Write-Info { Write-Host "[*] $args" -ForegroundColor Cyan }
function Write-Error { Write-Host "[X] $args" -ForegroundColor Red }
function Write-Warning { Write-Host "[!] $args" -ForegroundColor Yellow }

if ($Help) {
    Write-Host @"
Veil Windows Installation Script

Usage:
    .\install.ps1

What it does:
    1. Checks for Rust/Cargo
    2. Builds veil in release mode
    3. Installs to user .cargo\bin directory
    4. Verifies installation

Requirements:
    - Rust toolchain (will prompt to install if missing)

For advanced options, use:
    .\scripts\windows\install.ps1 -Help

"@ -ForegroundColor White
    exit 0
}

Write-Host "========================================"
Write-Host "  Veil Windows Installation"
Write-Host "========================================"
Write-Host ""

# Check for Cargo
Write-Info "Checking for Rust/Cargo..."
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "Cargo not found!"
    Write-Host ""
    Write-Host "Please install Rust from: https://rustup.rs/" -ForegroundColor Yellow
    Write-Host "Or run the advanced installer:" -ForegroundColor Yellow
    Write-Host "  .\scripts\windows\install.ps1" -ForegroundColor Cyan
    exit 1
}

$cargoVersion = cargo --version
Write-Success "Found Cargo: $cargoVersion"

# Get install directory
$installDir = if ($env:CARGO_HOME) {
    "$env:CARGO_HOME\bin"
} else {
    "$env:USERPROFILE\.cargo\bin"
}

if (-not (Test-Path $installDir)) {
    Write-Error "Install directory not found: $installDir"
    exit 1
}

Write-Info "Install directory: $installDir"

# Clean old artifacts
Write-Info "Cleaning old build artifacts..."
cargo clean | Out-Null

# Build project
Write-Info "Building Veil (Release mode)..."
Write-Host ""

try {
    cargo build --release --package veil-cli
    Write-Host ""
    Write-Success "Build completed"
} catch {
    Write-Host ""
    Write-Error "Build failed: $_"
    exit 1
}

# Find binary
$binaryPath = "target\release\veil.exe"
if (-not (Test-Path $binaryPath)) {
    Write-Error "Binary not found: $binaryPath"
    exit 1
}

# Install binary
Write-Info "Installing veil to $installDir..."
Copy-Item $binaryPath "$installDir\veil.exe" -Force
Write-Success "Installation complete!"

# Check PATH
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*$installDir*") {
    Write-Warning "$installDir is not in PATH"
    Write-Host ""
    Write-Host "To add to PATH, run:" -ForegroundColor Yellow
    Write-Host "  `$env:Path += `";$installDir`"" -ForegroundColor Cyan
    Write-Host ""
    Write-Host "To make it permanent, add to your PowerShell profile or run:" -ForegroundColor Yellow
    Write-Host "  [Environment]::SetEnvironmentVariable('Path', `$env:Path + ';$installDir', 'User')" -ForegroundColor Cyan
    Write-Host ""
}

# Verify installation
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
    Write-Host "The binary is installed at: $installDir\veil.exe" -ForegroundColor Yellow
    Write-Host "Please restart your terminal or add $installDir to PATH" -ForegroundColor Yellow
}

Write-Host ""
Write-Host "Installation complete!" -ForegroundColor Green
Write-Host ""
