@echo off
REM Veil CLI Build Script for Windows
REM Corresponds to scripts/unix/build.sh

setlocal enabledelayedexpansion

REM Get project root directory (scripts/windows -> scripts -> root)
set SCRIPT_DIR=%~dp0
cd /d "%SCRIPT_DIR%..\..\"
set PROJECT_ROOT=%CD%

echo Starting Veil CLI build...
echo.

REM Clean old build artifacts
echo [*] Cleaning old build artifacts...
cargo clean
if %errorlevel% neq 0 (
    echo [X] Cargo clean failed
    exit /b 1
)

REM Build release version
echo [*] Building release version...
cargo build --release --package veil-cli
if %errorlevel% neq 0 (
    echo [X] Build failed
    exit /b 1
)

REM Create release package
echo [*] Creating release package...
if exist release\bin rmdir /s /q release\bin
mkdir release\bin

REM Copy binary
copy target\release\veil.exe release\bin\veil.exe >nul
if %errorlevel% neq 0 (
    echo [X] Failed to copy binary
    exit /b 1
)

echo.
echo [+] Build complete!
echo.
echo Release directory: release\
dir release\bin\veil.exe | findstr veil.exe
echo.

exit /b 0
