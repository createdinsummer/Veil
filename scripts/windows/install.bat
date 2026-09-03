@echo off
REM Veil Windows Installation Script (Batch version)
REM Corresponds to install.sh for Linux/macOS
REM Compiles and installs veil to user directory

setlocal enabledelayedexpansion

echo ========================================
echo   Veil Windows Installation
echo ========================================
echo.

REM Check for help
if /i "%~1"=="help" goto :show_help
if /i "%~1"=="--help" goto :show_help
if /i "%~1"=="-h" goto :show_help
goto :main

:show_help
echo Veil Windows Installation Script
echo.
echo Usage:
echo     install.bat
echo.
echo What it does:
echo     1. Checks for Rust/Cargo
echo     2. Builds veil in release mode
echo     3. Installs to user .cargo\bin directory
echo     4. Verifies installation
echo.
echo Requirements:
echo     - Rust toolchain
echo.
echo For advanced options, use PowerShell version:
echo     .\install.ps1 -Help
echo.
exit /b 0

:main

REM Check for Cargo
echo [*] Checking for Rust/Cargo...
where cargo >nul 2>&1
if %errorlevel% neq 0 (
    echo [X] Cargo not found!
    echo.
    echo Please install Rust from: https://rustup.rs/
    echo.
    echo Or use the PowerShell installer for automatic setup:
    echo   .\scripts\windows\install.ps1
    exit /b 1
)

for /f "tokens=*" %%i in ('cargo --version') do set CARGO_VERSION=%%i
echo [+] Found Cargo: !CARGO_VERSION!

REM Get install directory
if defined CARGO_HOME (
    set INSTALL_DIR=%CARGO_HOME%\bin
) else (
    set INSTALL_DIR=%USERPROFILE%\.cargo\bin
)

if not exist "!INSTALL_DIR!" (
    echo [X] Install directory not found: !INSTALL_DIR!
    exit /b 1
)

echo [*] Install directory: !INSTALL_DIR!

REM Clean old artifacts
echo [*] Cleaning old build artifacts...
cargo clean >nul 2>&1

REM Build project
echo [*] Building Veil (Release mode)...
echo.

cargo build --release --package veil-cli
if %errorlevel% neq 0 (
    echo.
    echo [X] Build failed
    exit /b 1
)

echo.
echo [+] Build completed

REM Find binary
set BINARY_PATH=target\release\veil.exe
if not exist "%BINARY_PATH%" (
    echo [X] Binary not found: %BINARY_PATH%
    exit /b 1
)

REM Install binary
echo [*] Installing veil to !INSTALL_DIR!...
copy /Y "%BINARY_PATH%" "!INSTALL_DIR!\veil.exe" >nul
if %errorlevel% neq 0 (
    echo [X] Installation failed
    exit /b 1
)

echo [+] Installation complete!

REM Check PATH
echo !PATH! | findstr /C:"!INSTALL_DIR!" >nul
if %errorlevel% neq 0 (
    echo.
    echo [!] !INSTALL_DIR! is not in PATH
    echo.
    echo To add to PATH, run:
    echo   setx PATH "%%PATH%%;!INSTALL_DIR!"
    echo.
    echo Then restart your terminal.
    echo.
)

REM Verify installation
echo [*] Verifying installation...
where veil >nul 2>&1
if %errorlevel% equ 0 (
    echo [+] veil successfully installed!
    echo.
    veil --version
    echo.
    echo Run 'veil --help' to see usage information
) else (
    echo.
    echo [!] veil command not found in PATH
    echo.
    echo The binary is installed at: !INSTALL_DIR!\veil.exe
    echo Please restart your terminal or add !INSTALL_DIR! to PATH
)

echo.
echo [+] Installation complete!
echo.

exit /b 0
