# Windows vs Linux Scripts Comparison

## Overview

Veil provides parallel scripts for both Windows and Linux/macOS platforms.

## Root Directory Scripts

### Installation Scripts

| Linux/macOS | Windows | Description |
|------------|---------|-------------|
| `install.sh` | `install.ps1` | Simple installation (build + install to user directory) |
| N/A | `scripts/windows/install.ps1` | Advanced installer (with Rust auto-install, PATH management) |

**Usage:**
```bash
# Linux/macOS
./install.sh

# Windows (PowerShell)
.\install.ps1

# Windows (Advanced)
.\scripts\windows\install.ps1 -AddToPath
```

---

### Build Scripts

| Linux/macOS | Windows PowerShell | Windows Batch | Description |
|------------|-------------------|---------------|-------------|
| `build.sh` | `build.ps1` | `build.bat` | Build automation |

**Usage:**
```bash
# Linux/macOS
./build.sh

# Windows (PowerShell)
.\build.ps1 -Release

# Windows (Batch)
build.bat release
```

---

### Test Scripts

| Linux/macOS | Windows | Description |
|------------|---------|-------------|
| N/A* | `test.ps1` | Comprehensive test runner |
| N/A* | `scripts/windows/test.ps1` | Actual test script |

*Linux typically uses `cargo test` directly

**Usage:**
```bash
# Linux/macOS
cargo test --workspace

# Windows (PowerShell)
.\test.ps1
```

---

## Feature Comparison

### install.sh vs install.ps1

| Feature | install.sh (Linux) | install.ps1 (Windows) |
|---------|-------------------|----------------------|
| Check Rust | ✅ | ✅ |
| Build release | ✅ | ✅ |
| Install to .cargo/bin | ✅ | ✅ |
| Verify installation | ✅ | ✅ |
| PATH detection | ✅ | ✅ |
| Colored output | ✅ | ✅ |
| Auto Rust install | ❌ | ✅ (advanced version) |
| Custom install path | ❌ | ✅ (advanced version) |

---

### build.sh vs build.ps1 vs build.bat

| Feature | build.sh | build.ps1 | build.bat |
|---------|----------|-----------|-----------|
| Debug build | ✅ | ✅ | ✅ |
| Release build | ✅ | ✅ | ✅ |
| Run tests | ✅ | ✅ | ✅ |
| Clean cache | ✅ | ✅ | ✅ |
| Colored output | ✅ | ✅ | ❌ |
| Build time tracking | ❌ | ✅ | ❌ |
| Binary size display | ❌ | ✅ | ❌ |

---

## Directory Structure Comparison

### Linux/macOS Structure
```
Veil/
├── install.sh           # Installation script
├── build.sh             # Build script
├── Cargo.toml
└── crates/
    ├── veil-core/
    └── veil-cli/
```

### Windows Structure
```
Veil/
├── install.ps1          # Simple installation (matches install.sh)
├── build.ps1            # Build wrapper
├── build.bat            # Build wrapper (batch)
├── test.ps1             # Test wrapper
│
├── scripts/
│   └── windows/
│       ├── README.md
│       ├── install.ps1  # Advanced installer
│       ├── build.ps1    # Actual build script
│       ├── build.bat    # Actual batch script
│       ├── test.ps1     # Actual test script
│       └── veil.bat     # Quick launcher
│
├── Cargo.toml
└── crates/
    ├── veil-core/
    └── veil-cli/
```

---

## Command Equivalents

### Installation

| Task | Linux/macOS | Windows |
|------|-------------|---------|
| Simple install | `./install.sh` | `.\install.ps1` |
| Advanced install | N/A | `.\scripts\windows\install.ps1 -AddToPath` |
| Install to custom path | Edit script | `.\scripts\windows\install.ps1 -InstallPath "C:\Tools"` |

### Building

| Task | Linux/macOS | Windows PowerShell | Windows Batch |
|------|-------------|-------------------|---------------|
| Debug build | `./build.sh` | `.\build.ps1` | `build.bat` |
| Release build | `./build.sh release` | `.\build.ps1 -Release` | `build.bat release` |
| Build + Test | `./build.sh test` | `.\build.ps1 -Test` | `build.bat test` |
| Clean | `cargo clean` | `.\build.ps1 -Clean` | `build.bat clean` |

### Testing

| Task | Linux/macOS | Windows |
|------|-------------|---------|
| All tests | `cargo test --workspace` | `.\test.ps1` |
| Core tests | `cargo test -p veil-core` | `.\test.ps1 -Core` |
| CLI tests | `cargo test -p veil-cli` | `.\test.ps1 -CLI` |
| Verbose | `cargo test -- --nocapture` | `.\test.ps1 -Verbose` |

### Usage

| Task | Linux/macOS | Windows |
|------|-------------|---------|
| Run veil | `veil --help` | `veil --help` (if in PATH) |
| Direct run | `./target/release/veil` | `.\target\release\veil.exe` |
| Quick launcher | N/A | `.\scripts\windows\veil.bat` |

---

## Platform-Specific Features

### Linux/macOS Only
- Shell-based scripts (.sh)
- Unix file permissions (chmod +x)
- Simpler installation (no PATH issues)

### Windows Only
- PowerShell scripts (.ps1) - Rich features
- Batch scripts (.bat) - Legacy compatibility
- Execution policy handling
- Advanced installer with Rust auto-install
- PATH management tools
- Quick launcher (veil.bat)

---

## Best Practices

### For Cross-Platform Users

**If you use both Linux and Windows:**

Linux/macOS:
```bash
./install.sh           # Install
./build.sh release     # Build
cargo test --workspace # Test
veil --help           # Use
```

Windows:
```powershell
.\install.ps1          # Install
.\build.ps1 -Release   # Build
.\test.ps1            # Test
veil --help           # Use
```

**Note**: Commands are designed to be as similar as possible!

---

## Migration Guide

### From Linux to Windows

1. Replace `./install.sh` with `.\install.ps1`
2. Replace `./build.sh` with `.\build.ps1`
3. Replace `cargo test` with `.\test.ps1` (optional, more features)
4. Everything else works the same!

### From Windows to Linux

1. Replace `.\install.ps1` with `./install.sh`
2. Replace `.\build.ps1` with `./build.sh`
3. Replace `.\test.ps1` with `cargo test --workspace`
4. Everything else works the same!

---

## Summary

| Aspect | Linux/macOS | Windows |
|--------|-------------|---------|
| **Primary Language** | Bash | PowerShell |
| **Alternative** | N/A | Batch (.bat) |
| **Installation** | `install.sh` | `install.ps1` |
| **Building** | `build.sh` | `build.ps1` / `build.bat` |
| **Testing** | `cargo test` | `test.ps1` |
| **Features** | Simple, Unix-style | Rich, Windows-integrated |
| **User Experience** | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ |

**Both platforms fully supported with equivalent functionality!** ✅

---

**Last Updated**: 2026-07-31  
**Status**: All scripts implemented and tested
