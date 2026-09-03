# Veil Test Script (Root Wrapper)
# This is a convenience wrapper that calls the actual script in scripts/windows/

param(
    [switch]$Unit,
    [switch]$Integration,
    [switch]$Core,
    [switch]$CLI,
    [switch]$Verbose,
    [switch]$Coverage,
    [switch]$Help
)

$ScriptPath = Join-Path $PSScriptRoot "scripts\windows\test.ps1"

if (-not (Test-Path $ScriptPath)) {
    Write-Host "[X] Script not found: $ScriptPath" -ForegroundColor Red
    Write-Host "[*] Make sure you're in the project root directory" -ForegroundColor Yellow
    exit 1
}

# Forward all parameters to the actual script
& $ScriptPath @PSBoundParameters
