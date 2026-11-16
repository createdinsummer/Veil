@echo off
REM Veil CLI Windows 安装脚本。
REM 所属模块：Windows 用户级 CLI 安装；检查 Cargo，调用构建脚本并安装 veil.exe。
REM 核心变量：SCRIPT_DIR / PROJECT_ROOT 定位项目；BINARY_PATH 是发布二进制；INSTALL_DIR 是 Cargo bin 目录。
REM 依赖 Windows CMD、Cargo、PATH 和 copy。目标是安装命令并验证其可用性。
REM 与 Unix 安装脚本承担相同业务角色，实际调用同目录 build-windows.bat，不调用 PowerShell 构建脚本。

REM 环境修改仅在脚本范围生效；延迟展开用于读取括号分支中设置的变量，含感叹号的路径可能受其影响。
setlocal enabledelayedexpansion

echo ========================================
echo   Veil Windows Installation
echo ========================================
echo.

REM 只读取第一个参数，help、--help、-h（忽略大小写）显示帮助；其他参数不报错，继续安装。
if /i "%~1"=="help" goto :show_help
if /i "%~1"=="--help" goto :show_help
if /i "%~1"=="-h" goto :show_help
goto :main

:show_help
echo Veil Windows Installation Script
echo.
echo Usage:
echo     install-windows.bat
echo.
echo What it does:
echo     1. Checks for Rust/Cargo
echo     2. Calls build-windows.bat to build veil
echo     3. Installs to user .cargo\bin directory
echo     4. Verifies installation
echo.
echo Requirements:
echo     - Rust toolchain
echo.
exit /b 0

:main

REM 先确认 Rust 工具链存在，缺失时停止以免后续命令产生误导错误。
echo [*] Checking for Rust/Cargo...
where cargo >nul 2>&1
if %errorlevel% neq 0 (
    echo [X] Cargo not found!
    echo.
    echo Please install Rust from: https://rustup.rs/
    exit /b 1
)

REM 读取 cargo --version 的每行输出作为展示文本，若有多行最终保留最后一行；不据此判断版本兼容性。
for /f "tokens=*" %%i in ('cargo --version') do set CARGO_VERSION=%%i
echo [+] Found Cargo: !CARGO_VERSION!

REM 从脚本路径定位仓库并切换盘符；后续构建和发布路径均以仓库目录解析。
set SCRIPT_DIR=%~dp0
cd /d "%SCRIPT_DIR%..\..\"
set PROJECT_ROOT=%CD%

REM 调用同目录的构建脚本，安装产物与发布产物保持一致。
echo [*] Calling build script...
set BUILD_SCRIPT=%SCRIPT_DIR%build-windows.bat

if not exist "%BUILD_SCRIPT%" (
    echo [X] Build script not found: %BUILD_SCRIPT%
    exit /b 1
)

call "%BUILD_SCRIPT%"
if %errorlevel% neq 0 (
    echo [X] Build failed
    exit /b 1
)

echo.

REM 确认预期发布文件存在，再继续安装。
set BINARY_PATH=%PROJECT_ROOT%\release\cli\bin\veil.exe
if not exist "%BINARY_PATH%" (
    echo [X] Binary not found: %BINARY_PATH%
    exit /b 1
)

REM 安装目标是非空 CARGO_HOME 下的 bin，否则为 USERPROFILE 下 .cargo\bin；目录必须已经存在，不自动创建。
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

REM 强制覆盖用户命令目录中的同名文件；复制失败返回 1，不进行原子替换或恢复旧版本。
echo [*] Installing veil to !INSTALL_DIR!...
copy /Y "%BINARY_PATH%" "!INSTALL_DIR!\veil.exe" >nul
if %errorlevel% neq 0 (
    echo [X] Installation failed
    exit /b 1
)

echo [+] Installation complete!

REM 在当前进程 PATH 文本中做区分大小写的子串查找，不按分号拆分规范化路径，因此可能误判。
REM 未匹配时仅打印 setx 示例，不执行、不修改用户或系统 PATH，也不刷新当前终端环境。
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

REM 从当前 PATH 查询 veil；命中的可能是更靠前的旧版本，没有比对安装目标。
REM --version 的退出码未检查，查询失败也只警告，最终返回 0 表示安装复制流程完成。
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
