# Windows Scripts - Directory Organization

## 📁 Directory Structure

```
Veil/
├── build.ps1                    # Wrapper: calls scripts/windows/build.ps1
├── build.bat                    # Wrapper: calls scripts/windows/build.bat
├── test.ps1                     # Wrapper: calls scripts/windows/test.ps1
│
├── scripts/
│   └── windows/
│       ├── README.md            # Detailed documentation
│       ├── build.ps1            # Actual PowerShell build script
│       ├── build.bat            # Actual batch build script
│       ├── test.ps1             # Actual PowerShell test script
│       ├── install.ps1          # Installation script
│       └── veil.bat             # Quick launcher
│
└── docs/ (Windows Documentation)
    ├── WINDOWS_TEST_SUMMARY.md
    ├── WINDOWS_COMPATIBILITY_TEST_COMPLETE.md
    ├── WINDOWS_SCRIPTS_GUIDE.md
    ├── WINDOWS_SCRIPTS_SUMMARY.md
    └── WINDOWS_PROJECT_FINAL_REPORT.md
```

## 🎯 Organization Benefits

### 1. Clean Project Root
- Only wrapper scripts in root (minimal)
- Easy to find and run: `.\build.ps1` or `.\test.ps1`
- No clutter from multiple scripts

### 2. Organized Scripts Directory
- All actual scripts in `scripts/windows/`
- Easy to maintain and update
- Clear separation of concerns

### 3. User-Friendly
- Users can run from root: `.\build.ps1 -Release`
- Wrappers forward all parameters automatically
- No need to navigate to scripts directory

## 🚀 Usage

### From Project Root (Recommended)

```powershell
# Build
.\build.ps1 -Release

# Test
.\test.ps1

# Or batch version
build.bat release
```

### From Scripts Directory (Direct)

```powershell
# Navigate to scripts
cd scripts\windows

# Build
.\build.ps1 -Release

# Test
.\test.ps1
```

## 📝 Script Types

### Root Level (Wrappers)
- **build.ps1** - Forwards to `scripts/windows/build.ps1`
- **build.bat** - Forwards to `scripts/windows/build.bat`
- **test.ps1** - Forwards to `scripts/windows/test.ps1`

**Purpose**: Convenience - users don't need to type long paths

### scripts/windows/ (Actual Scripts)
- **build.ps1** - Full-featured build script
- **build.bat** - Batch build alternative
- **test.ps1** - Comprehensive test script
- **install.ps1** - Installation automation
- **veil.bat** - Quick launcher for veil.exe
- **README.md** - Complete documentation

**Purpose**: Actual implementation and detailed docs

## 🔄 How Wrappers Work

### PowerShell Wrapper Example
```powershell
# Root: build.ps1
param([switch]$Release, [switch]$Test)

# Find actual script
$ScriptPath = Join-Path $PSScriptRoot "scripts\windows\build.ps1"

# Forward all parameters
& $ScriptPath @PSBoundParameters
```

### Batch Wrapper Example
```batch
REM Root: build.bat
call scripts\windows\build.bat %*
```

## ✅ Advantages

1. **User Experience**
   - Simple commands: `.\build.ps1`
   - No need to remember paths
   - Works from project root

2. **Maintainability**
   - Actual scripts organized in one place
   - Easy to find and update
   - Clear file structure

3. **Flexibility**
   - Can run wrappers from root
   - Can run actual scripts directly
   - Both approaches work

4. **Documentation**
   - README in scripts directory
   - Users can explore if needed
   - Clear separation

## 📚 Documentation Location

All Windows documentation moved/organized:

```
Veil/
├── WINDOWS_TEST_SUMMARY.md                      # Keep in root (summary)
├── WINDOWS_COMPATIBILITY_TEST_COMPLETE.md       # Keep in root (main report)
├── WINDOWS_SCRIPTS_GUIDE.md                     # Keep in root (user guide)
├── WINDOWS_SCRIPTS_SUMMARY.md                   # Keep in root
├── WINDOWS_PROJECT_FINAL_REPORT.md              # Keep in root (final report)
│
└── scripts/windows/
    └── README.md                                # Script-specific docs
```

## 🎓 Quick Start Guide

### For New Users

1. **Clone the repo**
   ```powershell
   git clone https://github.com/user/Veil.git
   cd Veil
   ```

2. **Build**
   ```powershell
   .\build.ps1 -Release
   ```

3. **Test**
   ```powershell
   .\test.ps1
   ```

4. **Done!** Binary at: `target\release\veil.exe`

### For Advanced Users

Navigate to `scripts/windows/` for:
- Installation script: `install.ps1`
- Quick launcher: `veil.bat`
- Detailed docs: `README.md`

## 🔧 Maintenance

### Adding New Scripts

1. Create script in `scripts/windows/`
2. Create wrapper in root (if user-facing)
3. Update `scripts/windows/README.md`
4. Update this file

### Updating Scripts

1. Edit actual script in `scripts/windows/`
2. Wrappers automatically use updated version
3. No need to modify wrappers

## 📋 Script Summary

| Script | Location | Type | Purpose |
|--------|----------|------|---------|
| build.ps1 | root | Wrapper | Easy build access |
| build.bat | root | Wrapper | Easy build access |
| test.ps1 | root | Wrapper | Easy test access |
| build.ps1 | scripts/windows/ | Actual | Build automation |
| build.bat | scripts/windows/ | Actual | Batch build |
| test.ps1 | scripts/windows/ | Actual | Test automation |
| install.ps1 | scripts/windows/ | Actual | Installation |
| veil.bat | scripts/windows/ | Actual | Quick launcher |

## 🎉 Result

**Before Organization**:
```
Veil/
├── build.ps1
├── build.bat
├── test.ps1
├── install.ps1
├── veil.bat
├── many other files...
```
❌ Cluttered, hard to manage

**After Organization**:
```
Veil/
├── build.ps1          (wrapper)
├── build.bat          (wrapper)
├── test.ps1           (wrapper)
└── scripts/windows/   (organized scripts + docs)
```
✅ Clean, organized, user-friendly

## 📞 Support

- **Script Documentation**: `scripts/windows/README.md`
- **Test Reports**: `WINDOWS_COMPATIBILITY_TEST_COMPLETE.md`
- **Usage Guide**: `WINDOWS_SCRIPTS_GUIDE.md`

---

**Organized**: 2026-07-31  
**Structure**: Clean and maintainable  
**User Experience**: Simplified ✅
