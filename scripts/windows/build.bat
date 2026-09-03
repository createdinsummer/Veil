@echo off
REM Veil Build Script (Root Wrapper)
REM Convenience wrapper for scripts\windows\build.bat

if not exist "scripts\windows\build.bat" (
    echo [X] Script not found: scripts\windows\build.bat
    echo [*] Make sure you're in the project root directory
    exit /b 1
)

call scripts\windows\build.bat %*
