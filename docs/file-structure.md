# Veil 文件结构详解

本文档详细说明 Veil 运行时产生的所有文件、目录结构和用途。

---

## 1. 用户可见文件

### 1.1 链接文件（.veil-link）

**位置**：用户任意指定（通常在当前目录或项目目录）

**示例**：
```
~/Documents/myfiles.veil-link
~/projects/secrets.veil-link
/media/usb/backup.veil-link
```

**文件内容**（TOML 格式）：
```toml
# myfiles.veil-link
version = "1.0"
created_at = "2026-09-10T10:00:00Z"

[workspace]
# 相对路径（推荐，便于跨设备）
path = "~/.veil/workspaces/default/myfiles"
# 或绝对路径
# path = "/Users/mac/.veil/workspaces/default/myfiles"

# 工作区类型
workspace_type = "default"  # default | custom | dedicated

[encryption]
algorithm = "AES-256-GCM"
key_derivation = "Argon2id"
key_derivation_params = { memory = 65536, iterations = 3, parallelism = 4 }

[metadata]
description = "我的加密文件"
tags = ["personal", "documents"]
```

**大小**：通常 < 1 KB

**特点**：
- ✅ 可以删除和重建
- ✅ 可以重命名
- ✅ 可以拷贝（创建多个指向同一工作区的链接）
- ✅ 可以放入 Git 版本控制（如果使用相对路径）

---

### 1.2 容器文件（.veil）

**位置**：用户任意指定（用于分享/备份）

**示例**：
```
~/Downloads/myfiles.veil
~/Backups/backup-2026-09-10.veil
/media/usb/share.veil
```

**文件结构**：
```
[Header: 64 bytes]
  magic:        "VEIL" (4 bytes)
  version:      1 (4 bytes)
  file_count:   N (8 bytes)
  metadata_len: M (8 bytes)
  header_hash:  SHA-256 (32 bytes)
  reserved:     (8 bytes)

[Metadata: M bytes]
  JSON 格式：
  {
    "version": "1.0",
    "container_name": "myfiles",
    "created_at": "2026-09-10T10:00:00Z",
    "packed_at": "2026-09-10T14:30:00Z",
    "encryption": {
      "algorithm": "AES-256-GCM",
      "key_derivation": "Argon2id"
    },
    "files": [
      {
        "name": "photo.jpg",
        "size": 1024000,
        "encrypted_size": 1024032,
        "offset": 4096,
        "hash": "sha256:..."
      },
      ...
    ]
  }

[File Data: variable]
  File 1: encrypted data
  File 2: encrypted data
  ...
```

**大小**：= 所有加密文件的总和 + 元数据

**特点**：
- ✅ 自包含，可独立解包
- ✅ 可以压缩（.veil.gz）
- ✅ 可以分割（未来功能）

---

## 2. 系统文件（~/.veil/）

### 2.1 目录结构总览

```
~/.veil/
├── config.toml                    # 全局配置
├── workspaces/                    # 工作区目录
│   ├── default/                   # 默认工作区
│   │   ├── veil-7f3a9c2d1b4e/     # 容器 myfiles 的工作区
│   │   │   ├── .veil-meta         # 工作区元数据
│   │   │   ├── .veil-lock         # 锁文件（可选）
│   │   │   ├── a3f2c1d4.enc       # 加密文件 1
│   │   │   ├── b7e4f9a8.enc       # 加密文件 2
│   │   │   └── c9f1e8d7.enc       # 加密文件 3
│   │   │
│   │   └── veil-91c2e8a4f63d/     # 容器 backup 的工作区
│   │       ├── .veil-meta
│   │       └── ...
│   │
│   ├── work/                      # 自定义工作区 "work"
│   │   └── veil-1f8c2d3e4a5b/
│   │       ├── .veil-meta
│   │       └── ...
│   │
│   └── personal/                  # 自定义工作区 "personal"
│       └── veil-2d9b1e7c4a80/
│           ├── .veil-meta
│           └── ...
│
├── cache/                         # 缓存目录（可选）
│   ├── keys/                      # 派生密钥缓存
│   └── thumbnails/                # 缩略图缓存（未来）
│
├── logs/                          # 日志（可选）
│   └── veil.log
│
└── icons/                         # 图标文件（自动设置）
    ├── veil-link.icns             # macOS
    ├── veil-link.ico              # Windows
    ├── veil-link.png              # Linux
    ├── veil.icns
    ├── veil.ico
    └── veil.png
```

---

### 2.2 全局配置文件（config.toml）

**路径**：`~/.veil/config.toml`

**内容示例**：
```toml
version = "1.0"

# 系统信息
[system]
icons_configured = true
icons_configured_at = "2026-09-10T10:30:00Z"
icons_version = "1.0"
first_run = false
install_id = "uuid-here"

# 默认工作目录
[workspace.default]
path = "~/.veil/workspaces/default"

# 自定义工作目录
[workspace.custom.work]
path = "~/Documents/veil-work"
description = "工作相关的加密文件"
created_at = "2026-09-10T10:30:00Z"

[workspace.custom.personal]
path = "~/Documents/veil-personal"
description = "个人文件"
created_at = "2026-09-10T10:35:00Z"

# 容器映射
[containers.myfiles]
workspace = "default"
created_at = "2026-09-10T11:00:00Z"
last_accessed = "2026-09-10T14:30:00Z"

[containers.project-a]
workspace = "work"
created_at = "2026-09-10T11:10:00Z"

[containers.secrets]
workspace_path = "~/EncryptedVolume/veil"  # 专属路径
dedicated = true
created_at = "2026-09-10T11:20:00Z"

# 用户偏好
[preferences]
hints_level = "full"               # full | brief | off
auto_sync = false
default_key_derivation = "Argon2id"
cache_keys = false                 # 是否缓存派生密钥
log_level = "info"                 # debug | info | warn | error

# 默认加密参数
[encryption.defaults]
algorithm = "AES-256-GCM"
key_derivation = "Argon2id"

[encryption.argon2id]
memory_kb = 65536                  # 64 MB
iterations = 3
parallelism = 4
```

**大小**：通常 2-5 KB

---

### 2.3 工作区元数据（.veil-meta）

**路径**：每个工作区目录内

**示例**：`~/.veil/workspaces/default/veil-7f3a9c2d1b4e/.veil-meta`

**内容示例**（JSON 格式）：
```json
{
  "version": "1.0",
  "container_name": "myfiles",
  "created_at": "2026-09-10T10:00:00Z",
  "last_modified": "2026-09-10T14:30:00Z",
  
  "encryption": {
    "algorithm": "AES-256-GCM",
    "key_derivation": "Argon2id",
    "key_derivation_params": {
      "memory_kb": 65536,
      "iterations": 3,
      "parallelism": 4
    },
    "salt": "base64-encoded-salt"
  },
  
  "files": [
    {
      "id": "a3f2c1d4e5f6",
      "encrypted_name": "a3f2c1d4e5f6.enc",
      "original_name": "photo.jpg",
      "mime_type": "image/jpeg",
      "size": 1024000,
      "encrypted_size": 1024032,
      "encrypted_at": "2026-09-10T10:05:00Z",
      "modified_at": "2026-09-09T15:30:00Z",
      "hash": "sha256:abcdef...",
      "nonce": "base64-encoded-nonce"
    },
    {
      "id": "b7e4f9a8c3d2",
      "encrypted_name": "b7e4f9a8c3d2.enc",
      "original_name": "notes.txt",
      "mime_type": "text/plain",
      "size": 2048,
      "encrypted_size": 2080,
      "encrypted_at": "2026-09-10T10:06:00Z",
      "modified_at": "2026-09-10T09:00:00Z",
      "hash": "sha256:123456...",
      "nonce": "base64-encoded-nonce"
    }
  ],
  
  "metadata": {
    "description": "我的文件",
    "tags": ["personal"],
    "total_files": 2,
    "total_size": 1026048,
    "total_encrypted_size": 1026112
  }
}
```

**大小**：取决于文件数量，通常几 KB 到几十 KB

**特点**：
- 记录文件名映射（加密文件名 ↔ 原始文件名）
- 记录加密参数
- 使用原子写入（写临时文件 + rename）

---

### 2.4 加密文件（.enc）

**路径**：工作区目录内

**命名**：随机 ID + `.enc`

**示例**：
```
~/.veil/workspaces/default/veil-7f3a9c2d1b4e/a3f2c1d4e5f6.enc
~/.veil/workspaces/default/veil-7f3a9c2d1b4e/b7e4f9a8c3d2.enc
```

**文件结构**：
```
[Nonce: 12 bytes]
  随机生成的 nonce（用于 AES-GCM）

[Encrypted Data: variable]
  原始文件内容加密后的数据

[Authentication Tag: 16 bytes]
  AES-GCM 认证标签
```

**大小**：原始文件大小 + 28 bytes（nonce + tag）

**特点**：
- 每个文件独立加密
- 使用随机文件名（防止文件名泄露信息）
- 支持并发读写

---

### 2.5 锁文件（.veil-lock）可选

**路径**：工作区目录内

**示例**：`~/.veil/workspaces/default/veil-7f3a9c2d1b4e/.veil-lock`

**内容示例**（JSON 格式）：
```json
{
  "locked_at": "2026-09-10T14:30:00Z",
  "locked_by": "veil-cli",
  "process_id": 12345,
  "hostname": "macbook-pro.local",
  "reason": "packing"
}
```

**用途**：
- 防止多个 veil 进程同时修改同一工作区
- 可选功能（默认不启用）

---

## 3. 临时文件

### 3.1 操作临时文件

**路径**：`/tmp/veil-{session_id}/` 或 `~/.veil/tmp/`

**用途**：
- 解密临时文件（用于 `veil edit`）
- 打包过程中的临时数据
- 导入/导出缓冲区

**示例**：
```
/tmp/veil-abc123/
├── decrypted_photo.jpg.tmp
└── packing_buffer.tmp
```

**特点**：
- 操作完成后自动清理
- 异常退出时，下次启动清理残留

---

### 3.2 缓存文件

**路径**：`~/.veil/cache/`

**用途**：
- 派生密钥缓存（可选，需用户启用）
- 缩略图缓存（未来功能）

**示例**：
```
~/.veil/cache/
├── keys/
│   └── myfiles.key.cache        # 缓存的派生密钥
└── thumbnails/
    └── a3f2c1d4e5f6.jpg         # 缩略图
```

**特点**：
- 可以安全删除（会重新生成）
- 密钥缓存需要额外的密码保护

---

## 4. 日志文件（可选）

**路径**：`~/.veil/logs/veil.log`

**内容示例**：
```
2026-09-10T10:00:00Z [INFO] veil init myfiles
2026-09-10T10:05:00Z [INFO] veil add photo.jpg myfiles.veil-link
2026-09-10T10:05:02Z [DEBUG] Encrypted photo.jpg -> a3f2c1d4e5f6.enc
2026-09-10T10:05:02Z [INFO] File added successfully
2026-09-10T14:30:00Z [INFO] veil pack myfiles.veil-link -o myfiles.veil
2026-09-10T14:30:05Z [INFO] Packed 3 files (1.2 MB)
```

**特点**：
- 默认不启用（通过配置开启）
- 自动轮转（按大小或时间）
- 不记录敏感信息（密码、密钥）

---

## 5. 平台特定文件

### 5.1 macOS

**图标注册**：
```
~/Library/Application Support/veil/
└── veil.plist                    # LaunchServices 配置
```

**图标文件**：
```
~/.veil/icons/
├── veil-link.icns
└── veil.icns
```

---

### 5.2 Windows

**注册表项**：
```
HKEY_CURRENT_USER\Software\Classes\.veil-link
HKEY_CURRENT_USER\Software\Classes\.veil
```

**图标文件**：
```
%LOCALAPPDATA%\veil\icons\
├── veil-link.ico
└── veil.ico
```

---

### 5.3 Linux

**MIME 类型定义**：
```
~/.local/share/mime/packages/veil.xml
```

**图标文件**：
```
~/.local/share/icons/hicolor/256x256/apps/
├── veil-link.png
└── veil-container.png
```

**桌面文件**（可选）：
```
~/.local/share/applications/veil.desktop
```

---

## 6. 文件权限

### 6.1 推荐权限

```bash
# 配置文件
~/.veil/config.toml              # 600 (-rw-------)

# 工作区目录
~/.veil/workspaces/              # 700 (drwx------)

# 工作区元数据
.veil-meta                        # 600 (-rw-------)

# 加密文件
*.enc                             # 600 (-rw-------)

# 链接文件（用户目录）
*.veil-link                       # 644 (-rw-r--r--)

# 容器文件
*.veil                            # 644 (-rw-r--r--)
```

### 6.2 自动权限修复

Veil 启动时会自动检查并修复权限：
```rust
fn fix_permissions() {
    let config_path = get_config_path();
    if config_path.metadata()?.permissions().mode() != 0o600 {
        fs::set_permissions(&config_path, Permissions::from_mode(0o600))?;
        warn!("Fixed permissions for {:?}", config_path);
    }
}
```

---

## 7. 磁盘空间估算

### 7.1 系统文件

```
~/.veil/
├── config.toml          ~5 KB
├── icons/               ~100 KB（6 个图标文件）
├── logs/                ~1-10 MB（可选）
└── cache/               0-100 MB（可选）

总计：~0.1-110 MB（取决于是否启用日志和缓存）
```

### 7.2 工作区

```
每个工作区：
├── .veil-meta           ~5-50 KB（取决于文件数量）
└── *.enc 文件           = 原始文件总和 + (文件数 × 28 bytes)

示例：
100 个文件，每个 1 MB
= 100 MB + (100 × 28 bytes)
≈ 100 MB
```

### 7.3 容器文件

```
.veil 容器：
= Header (64 bytes) 
  + Metadata (~几 KB) 
  + 所有加密文件的总和

示例：
100 个文件，每个 1 MB
= 64 bytes + 5 KB + 100 MB
≈ 100 MB
```

---

## 8. 文件清理

### 8.1 自动清理

```bash
# Veil 自动清理：
- 临时文件（操作完成后）
- 过期的日志（超过 30 天）
- 孤立的工作区（容器已删除）
```

### 8.2 手动清理

```bash
# 清理缓存
veil clean cache

# 清理日志
veil clean logs

# 清理孤立工作区
veil clean orphans

# 清理所有临时文件
veil clean all
```

### 8.3 完全卸载

```bash
# 删除所有 Veil 文件
rm -rf ~/.veil/
rm -f ~/.local/share/mime/packages/veil.xml  # Linux
rm -f ~/Library/Application\ Support/veil/   # macOS

# Windows
# 删除 %LOCALAPPDATA%\veil
# 删除注册表项
```

---

## 9. 备份建议

### 9.1 必须备份

```
✅ 工作区目录
   ~/.veil/workspaces/
   
✅ 容器文件
   *.veil
   
✅ 配置文件
   ~/.veil/config.toml
```

### 9.2 可选备份

```
⚠️ 链接文件
   *.veil-link（可重建）
   
⚠️ 缓存
   ~/.veil/cache/（可重建）
```

### 9.3 不需要备份

```
❌ 临时文件
   /tmp/veil-*/
   
❌ 日志
   ~/.veil/logs/
   
❌ 图标
   ~/.veil/icons/（可重新设置）
```

---

## 10. 文件版本兼容性

### 10.1 版本标识

所有关键文件都包含版本号：
```toml
version = "1.0"
```

### 10.2 向后兼容

- Veil 2.0 可以读取 1.0 格式的文件
- 读取时自动转换到新格式
- 写入时使用新格式

### 10.3 迁移工具

```bash
# 迁移旧版本文件到新格式
veil migrate --from 1.0 --to 2.0
```

---

## 11. 文件对比总览

| 文件类型 | 位置 | 大小 | 可删除 | 可分享 | 用途 |
|----------|------|------|--------|--------|------|
| **.veil-link** | 用户目录 | <1 KB | ✅ | ⚠️ 相对路径可 | 指向工作区 |
| **.veil** | 用户目录 | = 数据 | ✅ | ✅ | 打包容器 |
| **config.toml** | ~/.veil/ | 2-5 KB | ❌ | ❌ | 全局配置 |
| **.veil-meta** | 工作区 | 5-50 KB | ❌ | ❌ | 工作区元数据 |
| **\*.enc** | 工作区 | = 原文件 | ❌ | ❌ | 加密文件 |
| **icons/** | ~/.veil/ | ~100 KB | ✅ | ❌ | 图标文件 |
| **cache/** | ~/.veil/ | 0-100 MB | ✅ | ❌ | 缓存 |
| **logs/** | ~/.veil/ | 1-10 MB | ✅ | ❌ | 日志 |

---

**文档版本**：1.0  
**创建日期**：2026-09-10  
**作者**：Veil 开发团队
