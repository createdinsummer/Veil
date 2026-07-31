@echo off
REM Veil Windows Advanced Installation Script (Batch version)
REM Full-featured installer with error handling

setlocal enabledelayedexpansion

REM Default install path
set DEFAULT_INSTALL_PATH=%LOCALAPPDATA%\Veil
set INSTALL_PATH=%DEFAULT_INSTALL_PATH%
set ADD_TO_PATH=0

REM Parse arguments
:parse_args
if "%~1"=="" goto :end_parse
if /i "%~1"=="help" goto :show_help
if /i "%~1"=="--help" goto :show_help
if /i "%~1"=="-h" goto :show_help
if /i "%~1"=="--path" (
    set INSTALL_PATH=%~2
    shift
    shift
    goto :parse_args
)
if /i "%~1"=="--add-path" (
    set ADD_TO_PATH=1
    shift
    goto :parse_args
)
shift
goto :parse_args
:end_parse

goto :main

:show_help
echo Veil Windows Advanced Installation Script
echo.
echo Usage:
echo     install.bat [options]
echo.
echo Options:
echo     --path ^<directory^>    Custom installation path
echo     --add-path            Add to system PATH (requires admin)
echo     help, --help, -h      Show this help
echo.
echo Examples:
echo     install.bat                              # Default installation
echo     install.bat --path "C:\Tools\Veil"       # Custom path
echo     install.bat --add-path                   # Add to PATH
echo.
echo Default install path: %LOCALAPPDATA%\Veil
echo.
exit /b 0

:main

echo ========================================
echo   Veil - Advanced Windows Installer
echo ========================================
echo.

REM Check for Rust
echo [*] Step 1/4: Checking Rust environment...
where cargo >nul 2>&1
if %errorlevel% neq 0 (
    echo [!] Rust not found!
    echo.
    echo Please install Rust from: https://rustup.rs/
    echo After installing, run this script again.
    exit /b 1
)

for /f "tokens=*" %%i in ('cargo --version') do set CARGO_VERSION=%%i
echo [+] Found Rust: !CARGO_VERSION!

REM Build project
echo.
echo [*] Step 2/4: Building Veil...
echo.

cargo build --release --package veil-cli
if %errorlevel% neq 0 (
    echo.
    echo [X] Build failed
    exit /b 1
)

echo.
echo [+] Build successful

REM Install files
echo.
echo [*] Step 3/4: Installing to %INSTALL_PATH%...

if not exist "%INSTALL_PATH%" (
    mkdir "%INSTALL_PATH%"
)

set SOURCE_BINARY=target\release\veil.exe
set TARGET_BINARY=%INSTALL_PATH%\veil.exe

if not exist "%SOURCE_BINARY%" (
    echo [X] Binary not found: %SOURCE_BINARY%
    exit /b 1
)

copy /Y "%SOURCE_BINARY%" "%TARGET_BINARY%" >nul
if %errorlevel% neq 0 (
    echo [X] Installation failed
    exit /b 1
)

echo [+] Installed: %TARGET_BINARY%

REM Copy documentation
if exist "README.md" copy /Y "README.md" "%INSTALL_PATH%\" >nul 2>&1
if exist "LICENSE" copy /Y "LICENSE" "%INSTALL_PATH%\" >nul 2>&1

REM Create launcher batch file
echo @echo off > "%INSTALL_PATH%\veil.bat"
echo REM Veil Quick Launcher >> "%INSTALL_PATH%\veil.bat"
echo "%TARGET_BINARY%" %%* >> "%INSTALL_PATH%\veil.bat"

echo [+] Created launcher: %INSTALL_PATH%\veil.bat

REM Add to PATH (optional)
echo.
echo [*] Step 4/4: Configuring environment...

if %ADD_TO_PATH%==1 (
    echo [*] Adding to system PATH...
    setx PATH "%PATH%;%INSTALL_PATH%" >nul 2>&1
    if %errorlevel% equ 0 (
        echo [+] Added to PATH successfully
        echo [!] Please restart your terminal for changes to take effect
    ) else (
        echo [!] Failed to add to PATH automatically
        echo [*] Please add manually: %INSTALL_PATH%
    )
) else (
    echo [*] Not adding to PATH (use --add-path to add)
    echo.
    echo To add to PATH manually, run:
    echo   setx PATH "%%PATH%%;%INSTALL_PATH%"
)

REM Complete
echo.
echo ========================================
echo   Installation Complete!
echo ========================================
echo.
echo Install location: %INSTALL_PATH%
echo Executable: veil.exe
echo.
echo Quick start:
echo   1. View help: veil --help
echo   2. Create container: veil init my.veil
echo   3. Add file: veil add my.veil file.txt
echo.
echo Environment variables:
echo   VEIL_PASSWORD     - Set password to avoid prompts
echo   VEIL_NEW_PASSWORD - For passwd command
echo.

if %ADD_TO_PATH%==0 (
    echo [!] Note: To use 'veil' from anywhere, add to PATH:
    echo     setx PATH "%%PATH%%;%INSTALL_PATH%"
    echo.
)

exit /b 0
