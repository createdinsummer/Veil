@echo off
REM Veil 版本发布脚本（Windows）
REM 用法: scripts\release.bat [版本号]

setlocal enabledelayedexpansion

REM 获取脚本所在目录
set "SCRIPT_DIR=%~dp0"
REM 切换到项目根目录
cd /d "%SCRIPT_DIR%\.."

REM 检查是否在 git 仓库中
git rev-parse --git-dir >nul 2>&1
if errorlevel 1 (
    echo [错误] 当前目录不是 Git 仓库
    exit /b 1
)

REM 获取版本号
if "%~1"=="" (
    REM 从 Cargo.toml 读取版本
    for /f "tokens=3 delims== " %%a in ('findstr /r "^version = " Cargo.toml') do (
        set VERSION=%%~a
        goto :version_found
    )
    :version_found
    echo [信息] 从 Cargo.toml 读取到版本: !VERSION!
    echo.
    set /p "confirm=使用此版本发布? (y/n) [y]: "
    if "!confirm!"=="" set confirm=y
    if /i not "!confirm!"=="y" (
        set /p "VERSION=请输入新版本号 (如 1.2.0): "
    )
) else (
    set VERSION=%~1
)

REM 验证版本号格式
echo !VERSION! | findstr /r "^[0-9]*\.[0-9]*\.[0-9]*" >nul
if errorlevel 1 (
    echo [错误] 无效的版本号格式: !VERSION!
    echo [信息] 格式应为: X.Y.Z 或 X.Y.Z-suffix
    exit /b 1
)

set TAG=v!VERSION!

echo.
echo ==========================================
echo   Veil 版本发布
echo ==========================================
echo.
echo [信息] 版本号: !VERSION!
echo [信息] Git 标签: !TAG!
echo.

REM 检查标签是否已存在
git rev-parse !TAG! >nul 2>&1
if not errorlevel 1 (
    echo [错误] 标签 !TAG! 已存在
    echo.
    set /p "recreate=是否删除旧标签并重新创建? (y/n) [n]: "
    if /i "!recreate!"=="y" (
        echo [信息] 删除本地标签...
        git tag -d !TAG!
        echo [信息] 删除远程标签...
        git push origin --delete !TAG! 2>nul
        echo [成功] 旧标签已删除
    ) else (
        echo [错误] 发布已取消
        exit /b 1
    )
)

REM 检查工作区状态
git diff-index --quiet HEAD -- >nul 2>&1
if errorlevel 1 (
    echo [警告] 工作区有未提交的更改
    git status --short
    echo.
    set /p "continue=是否继续? (y/n) [n]: "
    if /i not "!continue!"=="y" (
        echo [错误] 发布已取消
        exit /b 1
    )
)

REM 获取当前分支
for /f "tokens=*" %%a in ('git branch --show-current') do set BRANCH=%%a
echo.
echo [信息] 当前分支: !BRANCH!
echo.

REM 确认发布
set /p "confirm=确认发布版本 !VERSION!? (y/n) [n]: "
if /i not "!confirm!"=="y" (
    echo [错误] 发布已取消
    exit /b 1
)

echo.
echo ==========================================
echo   开始发布流程
echo ==========================================
echo.

REM 步骤 1: 运行测试
echo [信息] 步骤 1/5: 运行测试...
cargo test --all --quiet
if errorlevel 1 (
    echo [错误] 测试失败
    exit /b 1
)
echo [成功] 所有测试通过

REM 步骤 2: 构建 Release 版本
echo [信息] 步骤 2/5: 构建 Release 版本...
cargo build --release --all --quiet
if errorlevel 1 (
    echo [错误] 构建失败
    exit /b 1
)
echo [成功] 构建成功

REM 步骤 3: 创建 Git 标签
echo [信息] 步骤 3/5: 创建 Git 标签...
git tag -a !TAG! -m "Release !VERSION!"
echo [成功] 标签 !TAG! 已创建

REM 步骤 4: 推送到远程
echo [信息] 步骤 4/5: 推送到远程仓库...
echo.

REM 获取所有远程仓库
git remote >nul 2>&1
if errorlevel 1 (
    echo [错误] 没有配置远程仓库
    echo [警告] 标签 !TAG! 已创建但未推送
    echo [信息] 请先配置远程仓库: git remote add ^<name^> ^<url^>
    exit /b 1
)

REM 显示远程仓库列表
echo [信息] 检测到以下远程仓库:
for /f "tokens=*" %%r in ('git remote') do (
    for /f "tokens=*" %%u in ('git remote get-url %%r') do (
        echo   - %%r: %%u
    )
)
echo.

echo [警告] 即将推送到所有远程仓库:
echo   分支: !BRANCH!
echo   标签: !TAG!
echo.
set /p "push_confirm=确认推送? (y/n) [y]: "
if "!push_confirm!"=="" set push_confirm=y

if /i "!push_confirm!"=="y" (
    set "failed_remotes="

    for /f "tokens=*" %%r in ('git remote') do (
        echo [信息] 推送到 %%r...

        REM 推送代码
        git push %%r !BRANCH! >nul 2>&1
        if errorlevel 1 (
            echo   [错误] 代码推送到 %%r 失败
            set "failed_remotes=!failed_remotes! %%r"
        ) else (
            echo   [成功] 代码已推送到 %%r

            REM 推送标签
            git push %%r !TAG! >nul 2>&1
            if errorlevel 1 (
                echo   [错误] 标签推送到 %%r 失败
                set "failed_remotes=!failed_remotes! %%r"
            ) else (
                echo   [成功] 标签已推送到 %%r
            )
        )
        echo.
    )

    if not "!failed_remotes!"=="" (
        echo [警告] 部分远程仓库推送失败:!failed_remotes!
        echo [信息] 可手动重试:
        for %%r in (!failed_remotes!) do (
            echo   git push %%r !BRANCH!
            echo   git push %%r !TAG!
        )
    ) else (
        echo [成功] 所有远程仓库推送成功
    )
) else (
    echo [警告] 推送已跳过
    echo [信息] 可稍后手动推送到所有远程仓库:
    for /f "tokens=*" %%r in ('git remote') do (
        echo   git push %%r !BRANCH!
        echo   git push %%r !TAG!
    )
)

REM 步骤 5: 完成
echo.
echo ==========================================
echo [成功] 发布完成！
echo ==========================================
echo.
echo [信息] 版本: !VERSION!
echo [信息] 标签: !TAG!
echo.
echo [信息] GitHub Actions 将自动构建并发布二进制包
echo [信息] 查看进度: https://github.com/YOUR_USERNAME/Veil/actions
echo.
echo [信息] 发布页面: https://github.com/YOUR_USERNAME/Veil/releases/tag/!TAG!
echo.

endlocal
