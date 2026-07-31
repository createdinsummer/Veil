@echo off
REM Veil Windows 构建脚本 (批处理版本)
REM 简单易用的批处理构建脚本

setlocal enabledelayedexpansion

echo ========================================
echo   Veil Windows 构建脚本
echo ========================================
echo.

REM 检查参数
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

REM 显示帮助
:show_help
echo 用法: build.bat [选项]
echo.
echo 选项:
echo   release, -r        编译 Release 版本
echo   test, -t           编译后运行测试
echo   clean, -c          清理构建缓存
echo   help, -h           显示此帮助
echo.
echo 示例:
echo   build.bat              # Debug 编译
echo   build.bat release      # Release 编译
echo   build.bat release test # Release 编译并测试
echo   build.bat clean        # 清理缓存
exit /b 0

REM 清理
if %CLEAN%==1 (
    echo [*] 清理构建缓存...
    if exist target (
        rmdir /s /q target
        echo [+] 构建缓存已清理
    ) else (
        echo [*] 无需清理，构建缓存不存在
    )
    exit /b 0
)

REM 检查 Rust
echo [*] 检查 Rust 环境...
where cargo >nul 2>&1
if %errorlevel% neq 0 (
    echo [!] 未找到 Cargo！
    echo [!] 请先安装 Rust: https://rustup.rs/
    exit /b 1
)

for /f "tokens=*" %%i in ('cargo --version') do set RUST_VERSION=%%i
echo [+] 找到 Rust: %RUST_VERSION%
echo.

REM 开始构建
echo [*] 开始构建 Veil (%BUILD_TYPE% 模式)...
echo.

set START_TIME=%time%

if "%BUILD_TYPE%"=="release" (
    cargo build --release --package veil-cli
) else (
    cargo build --package veil-cli
)

if %errorlevel% neq 0 (
    echo.
    echo [!] 构建失败
    exit /b 1
)

echo.
echo [+] 构建成功！

REM 显示二进制文件信息
set BINARY_PATH=target\%BUILD_TYPE%\veil.exe
if exist "%BINARY_PATH%" (
    for %%A in ("%BINARY_PATH%") do set SIZE=%%~zA
    set /a SIZE_MB=!SIZE! / 1048576
    echo [*] 二进制文件: %BINARY_PATH% (!SIZE_MB! MB)
)

REM 运行测试
if %RUN_TESTS%==1 (
    echo.
    echo [*] 运行测试...
    echo.
    cargo test --workspace
    if %errorlevel% neq 0 (
        echo.
        echo [!] 测试失败
        exit /b 1
    )
    echo.
    echo [+] 所有测试通过！
)

REM 显示下一步
echo.
echo [*] 下一步操作:
echo   1. 运行程序: .\target\%BUILD_TYPE%\veil.exe --help
echo   2. 运行测试: .\build.bat test
if "%BUILD_TYPE%"=="debug" (
    echo   3. 编译优化版本: .\build.bat release
)
echo.

exit /b 0
