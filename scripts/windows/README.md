# Windows Scripts for Veil

Windows-specific scripts for building, testing, and installing Veil.

## Directory Structure

```
scripts/windows/
├── README.md           # This file
├── build.ps1           # PowerShell build script (recommended)
├── build.bat           # Batch build script (alternative)
├── test.ps1            # PowerShell test script
├── install.ps1         # PowerShell installation script
└── veil.bat            # Quick launcher
```

## Quick Start

### For Most Users (PowerShell - Recommended)

```powershell
# Navigate to project root
cd D:\project\Veil

# Build release version
.\scripts\windows\build.ps1 -Release

# Run tests
.\scripts\windows\test.ps1

# Install system-wide
.\scripts\windows\install.ps1 -AddToPath
```

### For Restricted Environments (Batch)

```batch
cd D:\project\Veil

REM Build release version
scripts\windows\build.bat release

REM Run with launcher
scripts\windows\veil.bat --help
```

## Available Scripts

### 1. build.ps1 (PowerShell Build Script)

**Purpose**: Automate building Veil with various options

**Usage**:
```powershell
.\scripts\windows\build.ps1 [Options]

Options:
  -Release      Build optimized release version
  -Test         Run tests after build
  -Clean        Clean build cache
  -Help         Show help
```

**Examples**:
```powershell
# Debug build
.\scripts\windows\build.ps1

# Release build
.\scripts\windows\build.ps1 -Release

# Build and test
.\scripts\windows\build.ps1 -Release -Test

# Clean cache
.\scripts\windows\build.ps1 -Clean
```

**Features**:
- ✅ Colored output
- ✅ Build time tracking
- ✅ Binary size reporting
- ✅ Integrated testing
- ✅ Error handling

---

### 2. test.ps1 (PowerShell Test Script)

**Purpose**: Run comprehensive test suites with filtering

**Usage**:
```powershell
.\scripts\windows\test.ps1 [Options]

Options:
  -Core         Test only veil-core library
  -CLI          Test only veil-cli
  -Unit         Test only unit tests
  -Integration  Test only integration tests
  -Verbose      Show detailed output
  -Help         Show help
```

**Examples**:
```powershell
# Run all tests
.\scripts\windows\test.ps1

# Test core library only
.\scripts\windows\test.ps1 -Core

# Test CLI with verbose output
.\scripts\windows\test.ps1 -CLI -Verbose
```

**Features**:
- ✅ Component filtering
- ✅ Test statistics
- ✅ Pass rate calculation
- ✅ Verbose mode

---

### 3. install.ps1 (PowerShell Installation Script)

**Purpose**: Automated installation with Rust setup

**Usage**:
```powershell
.\scripts\windows\install.ps1 [Options]

Options:
  -InstallPath <path>  Custom installation path
  -AddToPath           Add to system PATH (requires admin)
  -Help                Show help
```

**Examples**:
```powershell
# Default installation
.\scripts\windows\install.ps1

# Install to custom location
.\scripts\windows\install.ps1 -InstallPath "C:\Tools\Veil"

# Install and add to PATH
.\scripts\windows\install.ps1 -AddToPath
```

**Features**:
- ✅ Automatic Rust installation
- ✅ PATH management
- ✅ Custom install location
- ✅ Launcher creation

---

### 4. build.bat (Batch Build Script)

**Purpose**: Simple build script for restricted environments

**Usage**:
```batch
scripts\windows\build.bat [options]

Options:
  release       Build release version
  test          Run tests after build
  clean         Clean build cache
  help          Show help
```

**Examples**:
```batch
REM Debug build
scripts\windows\build.bat

REM Release build
scripts\windows\build.bat release

REM Build and test
scripts\windows\build.bat release test
```

---

### 5. veil.bat (Quick Launcher)

**Purpose**: Wrapper to run veil.exe from anywhere

**Usage**:
```batch
scripts\windows\veil.bat [veil commands]
```

**Examples**:
```batch
scripts\windows\veil.bat --help
scripts\windows\veil.bat init my.veil
scripts\windows\veil.bat add my.veil file.txt
```

**Auto-detection**: Finds veil.exe in:
1. `target\release\veil.exe`
2. `target\debug\veil.exe`
3. Same directory as veil.bat
4. System PATH

---

## Which Script Should I Use?

### Recommended: PowerShell Scripts (.ps1)

**Use PowerShell if**:
- ✅ You're on Windows 10/11 (has PowerShell built-in)
- ✅ You want colored output and better error messages
- ✅ You need advanced features (filtering, statistics)
- ✅ Your organization allows PowerShell

**Scripts**: `build.ps1`, `test.ps1`, `install.ps1`

### Alternative: Batch Scripts (.bat)

**Use Batch if**:
- ⚠️ PowerShell is disabled by company policy
- ⚠️ You're on very old Windows versions
- ⚠️ You only need basic build functionality

**Scripts**: `build.bat`, `veil.bat`

---

## PowerShell Execution Policy

If you get an error about execution policy:

### Quick Fix (One-time)
```powershell
powershell -ExecutionPolicy Bypass -File scripts\windows\build.ps1
```

### Permanent Fix (Recommended)
```powershell
# Run as Administrator
Set-ExecutionPolicy RemoteSigned -Scope CurrentUser
```

### Verify Policy
```powershell
Get-ExecutionPolicy -List
```

---

## Environment Variables

Set these to customize behavior:

```powershell
# PowerShell
$env:VEIL_PASSWORD = "your_password"
$env:VEIL_NEW_PASSWORD = "new_password"
$env:RUST_LOG = "debug"
$env:RUST_BACKTRACE = "1"
```

```batch
REM Batch/CMD
set VEIL_PASSWORD=your_password
set VEIL_NEW_PASSWORD=new_password
set RUST_LOG=debug
set RUST_BACKTRACE=1
```

---

## Troubleshooting

### "Cargo not found"
**Solution**: Install Rust from https://rustup.rs/ or run:
```powershell
.\scripts\windows\install.ps1
```

### PowerShell script won't run
**Solution**: Check execution policy
```powershell
Get-ExecutionPolicy
Set-ExecutionPolicy RemoteSigned -Scope CurrentUser
```

### Build fails
**Solution**: Clean and rebuild
```powershell
.\scripts\windows\build.ps1 -Clean
.\scripts\windows\build.ps1 -Release
```

### Tests timeout
**Solution**: This is normal for some tests on Windows (they are marked as ignored)

---

## Integration with Project Root

These scripts can be run from the project root:

```powershell
# From D:\project\Veil
.\scripts\windows\build.ps1 -Release
.\scripts\windows\test.ps1
```

Or create shortcuts in project root (optional):

```powershell
# Create convenient aliases in project root
New-Item -ItemType SymbolicLink -Path "build.ps1" -Target "scripts\windows\build.ps1"
New-Item -ItemType SymbolicLink -Path "test.ps1" -Target "scripts\windows\test.ps1"
```

---

## Requirements

- **Windows 10/11** (recommended)
- **Rust 1.70+** (auto-installed by install.ps1)
- **PowerShell 5.1+** (for .ps1 scripts)
- **CMD** (for .bat scripts)

---

## Performance

Typical execution times on Windows 11:
- **Debug build**: ~30 seconds (first time)
- **Release build**: ~35 seconds (first time)
- **Incremental build**: ~1-5 seconds
- **Full test suite**: ~18 seconds

---

## Contributing

When adding new Windows scripts:
1. Place them in `scripts/windows/`
2. Update this README.md
3. Test on both PowerShell and CMD (if applicable)
4. Include help text with `-Help` parameter

---

## Related Documentation

- **Complete Test Report**: `../../WINDOWS_COMPATIBILITY_TEST_COMPLETE.md`
- **Scripts Guide**: `../../WINDOWS_SCRIPTS_GUIDE.md`
- **Test Summary**: `../../WINDOWS_TEST_SUMMARY.md`
- **Main README**: `../../README.md`

---

## License

Same as main project (Apache License 2.0)

---

**Last Updated**: 2026-07-31  
**Tested On**: Windows 11 Pro 10.0.26200  
**Maintained By**: Veil Project Team
