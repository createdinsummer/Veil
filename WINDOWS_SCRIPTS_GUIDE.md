# Veil Windows Scripts Guide

This directory contains Windows-specific scripts for building, testing, and installing Veil on Windows.

## Available Scripts

### 1. PowerShell Scripts (Recommended)

#### build.ps1 - Build Script
Build Veil on Windows with various options.

**Usage:**
```powershell
# Debug build
.\build.ps1

# Release build (optimized)
.\build.ps1 -Release

# Build and run tests
.\build.ps1 -Release -Test

# Clean build cache
.\build.ps1 -Clean

# Show help
.\build.ps1 -Help
```

#### test.ps1 - Test Script
Run comprehensive tests with filtering options.

**Usage:**
```powershell
# Run all tests
.\test.ps1

# Run only core library tests
.\test.ps1 -Core

# Run only CLI tests
.\test.ps1 -CLI

# Run with verbose output
.\test.ps1 -Verbose

# Show help
.\test.ps1 -Help
```

#### install.ps1 - Installation Script
Automatically install Rust (if needed) and build Veil.

**Usage:**
```powershell
# Install with default settings
.\install.ps1

# Install and add to PATH (requires admin)
.\install.ps1 -AddToPath

# Custom installation path
.\install.ps1 -InstallPath "C:\Tools\Veil"

# Show help
.\install.ps1 -Help
```

### 2. Batch Scripts (Alternative)

#### build.bat - Simple Build Script
For environments where PowerShell is restricted.

**Usage:**
```batch
REM Debug build
build.bat

REM Release build
build.bat release

REM Build and test
build.bat release test

REM Clean cache
build.bat clean

REM Show help
build.bat help
```

#### veil.bat - Quick Launcher
Wrapper script to run veil.exe from anywhere.

**Usage:**
```batch
REM Run veil commands
veil.bat --help
veil.bat init my.veil
veil.bat add my.veil file.txt
```

## Quick Start

### Option 1: PowerShell (Recommended)

1. **Clone the repository:**
   ```powershell
   git clone https://github.com/yourusername/Veil.git
   cd Veil
   ```

2. **Build release version:**
   ```powershell
   .\build.ps1 -Release
   ```

3. **Run tests:**
   ```powershell
   .\test.ps1
   ```

4. **Use Veil:**
   ```powershell
   .\target\release\veil.exe --help
   ```

### Option 2: Batch Files

1. **Build:**
   ```batch
   build.bat release
   ```

2. **Run:**
   ```batch
   veil.bat --help
   ```

## Requirements

- **Windows 10/11** (recommended)
- **Rust 1.70+** (will be installed by install.ps1 if missing)
- **PowerShell 5.1+** (for .ps1 scripts)
- **CMD** (for .bat scripts)

## PowerShell Execution Policy

If you get an error about execution policy when running .ps1 scripts:

```powershell
# Option 1: Bypass for current session
powershell -ExecutionPolicy Bypass -File build.ps1

# Option 2: Set execution policy (requires admin)
Set-ExecutionPolicy RemoteSigned -Scope CurrentUser
```

## Environment Variables

Set these environment variables to customize behavior:

```powershell
# Set password to avoid interactive input
$env:VEIL_PASSWORD = "your_password"

# Set new password for passwd command
$env:VEIL_NEW_PASSWORD = "new_password"

# Enable debug logging
$env:RUST_LOG = "debug"

# Enable backtrace
$env:RUST_BACKTRACE = "1"
```

Or in CMD:
```batch
set VEIL_PASSWORD=your_password
set VEIL_NEW_PASSWORD=new_password
```

## Automated Installation

For a fully automated setup:

```powershell
# Install Rust, build Veil, and add to PATH
.\install.ps1 -AddToPath
```

This will:
1. Check for Rust and install if missing
2. Build Veil in release mode
3. Install to `%LOCALAPPDATA%\Veil`
4. Add to system PATH (requires admin)

## Troubleshooting

### "Cargo not found"

Install Rust from https://rustup.rs/ or run:
```powershell
.\install.ps1
```

### PowerShell script won't run

```powershell
# Unblock the script
Unblock-File .\build.ps1

# Or run with bypass
powershell -ExecutionPolicy Bypass -File build.ps1
```

### Build fails with missing dependencies

```powershell
# Update Rust
rustup update

# Clean and rebuild
.\build.ps1 -Clean
.\build.ps1 -Release
```

### Tests timeout

Some tests may timeout on slower systems. This is normal for:
- `command_without_password_fails` (ignored on Windows)

## Script Features

### build.ps1
- ✅ Colored output
- ✅ Progress indicators
- ✅ Error handling
- ✅ Build time reporting
- ✅ Binary size display

### test.ps1
- ✅ Test filtering (Core/CLI/Unit/Integration)
- ✅ Verbose mode
- ✅ Test statistics
- ✅ Pass rate calculation
- ✅ Coverage support (with tarpaulin)

### install.ps1
- ✅ Automatic Rust installation
- ✅ PATH management
- ✅ Custom install path
- ✅ Launcher script creation
- ✅ Documentation copying

## CI/CD Integration

For GitHub Actions or other CI systems:

```yaml
# .github/workflows/windows.yml
- name: Build
  run: .\build.ps1 -Release

- name: Test
  run: .\test.ps1
```

## Performance

Typical build times on Windows 11:
- **Debug build**: ~30 seconds (first time)
- **Release build**: ~35 seconds (first time)
- **Incremental build**: ~1-5 seconds
- **Full test suite**: ~18 seconds

## Support

- **Issues**: https://github.com/yourusername/Veil/issues
- **Documentation**: See main README.md
- **Windows Guide**: WINDOWS_COMPATIBILITY_TEST_COMPLETE.md

## License

Same as main project (Apache License 2.0)
