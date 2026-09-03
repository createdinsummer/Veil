# Veil Windows Install Script
# Corresponds to scripts/unix/install.sh
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
    2. Calls build.ps1 to build veil
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

# Check for Cargo
Write-Info "Checking for Rust/Cargo..."
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "Cargo not found!"
    Write-Host ""
    Write-Host "Please install Rust from: https://rustup.rs/" -ForegroundColor Yellow
    exit 1
}

$cargoVersion = cargo --version
Write-Success "Found Cargo: $cargoVersion"

# Get script and project directories
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ProjectRoot = Resolve-Path (Join-Path $ScriptDir "../..")
Set-Location $ProjectRoot

# Call build script
Write-Info "Calling build script..."
$BuildScript = Join-Path $ScriptDir "build.ps1"

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

# Find build artifact
$BinaryPath = Join-Path $ProjectRoot "release\bin\veil.exe"
if (-not (Test-Path $BinaryPath)) {
    Write-Error "Binary not found: $BinaryPath"
    exit 1
}

# Get install directory
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

# Install binary
Write-Info "Installing veil to $InstallDir..."
Copy-Item $BinaryPath "$InstallDir\veil.exe" -Force
Write-Success "Installation complete!"

# Check PATH
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
    Write-Host "The binary is installed at: $InstallDir\veil.exe" -ForegroundColor Yellow
    Write-Host "Please restart your terminal or add $InstallDir to PATH" -ForegroundColor Yellow
}

Write-Host ""
Write-Host "Installation complete!" -ForegroundColor Green
Write-Host ""

exit 0
