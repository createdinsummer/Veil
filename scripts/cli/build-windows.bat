@echo off
REM Veil CLI Build Script for Windows
REM Corresponds to scripts/cli/build-unix.sh

setlocal enabledelayedexpansion

REM Get project root directory (scripts/cli -> scripts -> root)
set SCRIPT_DIR=%~dp0
cd /d "%SCRIPT_DIR%..\..\"
set PROJECT_ROOT=%CD%

echo Starting Veil CLI build...
echo.

REM Build release version
echo [*] Building release version...
cargo build --release --package veil-cli
if %errorlevel% neq 0 (
    echo [X] Build failed
    exit /b 1
)

REM Create release package
echo [*] Creating release package...
if not exist release\cli\bin mkdir release\cli\bin

REM Copy binary
set "TARGET_DIR=%PROJECT_ROOT%\target"
if defined CARGO_TARGET_DIR set "TARGET_DIR=%CARGO_TARGET_DIR%"
copy /Y "%TARGET_DIR%\release\veil.exe" "release\cli\bin\veil.exe" >nul
if %errorlevel% neq 0 (
    echo [X] Failed to copy binary
    exit /b 1
)

echo.
echo [+] Build complete!
echo.
echo Release directory: release\cli\
dir release\cli\bin\veil.exe | findstr veil.exe
echo.

exit /b 0
