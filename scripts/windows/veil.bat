@echo off
REM Veil Windows 快速启动脚本 (批处理版本)
REM 用于不支持 PowerShell 的环境

setlocal enabledelayedexpansion

REM 检查是否有参数
if "%~1"=="" (
    echo Veil - 文件加密容器工具
    echo.
    echo 用法: veil.bat [命令] [选项]
    echo.
    echo 常用命令:
    echo   init      创建新容器
    echo   add       添加文件到容器
    echo   free      查看容器内容
    echo   ex        导出文件
    echo   rm        删除文件
    echo   mv        移动/重命名文件
    echo   info      显示容器信息
    echo   passwd    修改密码
    echo   shell     进入交互式 Shell
    echo.
    echo 使用 'veil.bat --help' 查看完整帮助
    exit /b 0
)

REM 查找 veil.exe
set VEIL_EXE=
if exist "%~dp0target\release\veil.exe" (
    set VEIL_EXE=%~dp0target\release\veil.exe
) else if exist "%~dp0target\debug\veil.exe" (
    set VEIL_EXE=%~dp0target\debug\veil.exe
) else if exist "%~dp0veil.exe" (
    set VEIL_EXE=%~dp0veil.exe
) else (
    where veil.exe >nul 2>&1
    if !errorlevel! equ 0 (
        set VEIL_EXE=veil.exe
    ) else (
        echo 错误: 未找到 veil.exe
        echo 请先运行 build.bat 编译项目
        exit /b 1
    )
)

REM 执行 veil.exe
"%VEIL_EXE%" %*
exit /b %errorlevel%
