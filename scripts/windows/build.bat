@echo off
REM Veil Windows Build Script (Batch version)
REM Simple batch build script

setlocal enabledelayedexpansion

echo ========================================
echo   Veil Windows Build Script
echo ========================================
echo.

REM Check arguments
set BUILD_TYPE=debug
set RUN_TESTS=0
set CLEAN=0

:parse_args
if "%~1"=="" goto :end_parse
if /i "%~1"=="release" set BUILD_TYPE=release
if /i "%~1"=="--release" set BUILD_TYPE=release
if /i "%~1"=="-r" set BUILD_TYPE=release
if /i "%~1"=="test" set RUN_TESTS=1
if /i "%~1"=="--test" set RUN_TESTS=1
if /i "%~1"=="-t" set RUN_TESTS=1
if /i "%~1"=="clean" set CLEAN=1
if /i "%~1"=="--clean" set CLEAN=1
if /i "%~1"=="-c" set CLEAN=1
if /i "%~1"=="help" goto :show_help
if /i "%~1"=="--help" goto :show_help
if /i "%~1"=="-h" goto :show_help
shift
goto :parse_args
:end_parse

goto :main

REM Show help
:show_help
echo Usage: build.bat [options]
echo.
echo Options:
echo   release, -r        Build release version
echo   test, -t           Run tests after build
echo   clean, -c          Clean build cache
echo   help, -h           Show this help
echo.
echo Examples:
echo   build.bat              # Debug build
echo   build.bat release      # Release build
echo   build.bat release test # Release build and test
echo   build.bat clean        # Clean cache
exit /b 0

:main
REM Clean
if %CLEAN%==1 (
    echo [*] Cleaning build cache...
    if exist target (
        rmdir /s /q target
        echo [+] Build cache cleaned
    ) else (
        echo [*] No cache to clean
    )
    exit /b 0
)

REM Check Rust
echo [*] Checking Rust environment...
where cargo >nul 2>&1
if %errorlevel% neq 0 (
    echo [X] Cargo not found!
    echo [X] Please install Rust: https://rustup.rs/
    exit /b 1
)

for /f "tokens=*" %%i in ('cargo --version') do set RUST_VERSION=%%i
echo [+] Found Rust: %RUST_VERSION%
echo.

REM Start build
echo [*] Building Veil (%BUILD_TYPE% mode)...
echo.

set START_TIME=%time%

if "%BUILD_TYPE%"=="release" (
    cargo build --release --package veil-cli
) else (
    cargo build --package veil-cli
)

if %errorlevel% neq 0 (
    echo.
    echo [X] Build failed
    exit /b 1
)

echo.
echo [+] Build successful!

REM Show binary info
set BINARY_PATH=target\%BUILD_TYPE%\veil.exe
if exist "%BINARY_PATH%" (
    echo [*] Binary: %BINARY_PATH%
)

REM Run tests
if %RUN_TESTS%==1 (
    echo.
    echo [*] Running tests...
    echo.
    cargo test --workspace
    if %errorlevel% neq 0 (
        echo.
        echo [X] Tests failed
        exit /b 1
    )
    echo.
    echo [+] All tests passed!
)

REM Show next steps
echo.
echo [*] Next steps:
echo   1. Run program: .\target\%BUILD_TYPE%\veil.exe --help
echo   2. Run tests: .\build.bat test
if "%BUILD_TYPE%"=="debug" (
    echo   3. Build release: .\build.bat release
)
echo.

exit /b 0
