@echo off
REM Veil CLI Windows 构建脚本。
REM 所属模块：CLI 发布工具；构建 veil-cli 并将 veil.exe 放入 release/cli/bin。
REM 核心变量：SCRIPT_DIR 和 PROJECT_ROOT 定位仓库，TARGET_DIR 指向 Cargo 二进制产物目录。
REM 依赖 Cargo 与 Windows 批处理；业务目标与 Unix 构建脚本一致。

REM 将环境变量修改限制在脚本范围；脚本结束不保留这些变量，但工作目录不会由此自动恢复。
setlocal enabledelayedexpansion

REM 从脚本目录切到仓库根目录，并允许切换盘符；后续发布路径相对该目录解析。
set SCRIPT_DIR=%~dp0
cd /d "%SCRIPT_DIR%..\..\"
set PROJECT_ROOT=%CD%

echo Starting Veil CLI build...
echo.

REM 生成可发布的优化版本；Cargo 失败时立即返回错误码。
echo [*] Building release version...
cargo build --release --package veil-cli
if %errorlevel% neq 0 (
    echo [X] Build failed
    exit /b 1
)

REM 建立独立 CLI 发布目录，不改写其他客户端产物。
echo [*] Creating release package...
if not exist release\cli\bin mkdir release\cli\bin

REM 读取 CARGO_TARGET_DIR 或默认 target 中的 release 可执行文件，覆盖发布目录同名文件。
REM 未显式指定 Rust target；若 Cargo 配置使用其他 target-dir 或目标三元组，这个拼接路径可能找不到实际产物。
REM 复制不是原子替换，失败不会恢复已有发布文件；目录创建没有独立错误检查，复制步骤才显式判定失败。
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
REM 下面只展示文件信息，未检查管道退出状态；最终固定返回 0 不代表此展示命令一定成功。
dir release\cli\bin\veil.exe | findstr veil.exe
echo.

exit /b 0
