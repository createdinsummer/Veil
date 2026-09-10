# Veil 2.0 架构设计

## 核心概念

### 工作区（Workspace）
工作区是容器的工作目录，用于存储加密文件和元数据。

**路径结构：**
```
~/.veil/
├── config.toml              # 全局配置
└── workspaces/
    └── default/             # 默认工作区类型
        ├── photos/          # 容器名称
        │   ├── .veil-meta   # 加密的元数据文件
        │   ├── abc123.enc   # 加密文件1
        │   └── def456.enc   # 加密文件2
        └── documents/       # 另一个容器
            ├── .veil-meta
            └── ...
```

### 容器格式（.veil）
.veil 是单文件打包格式，便于分享和传输。

**文件结构：**
```
.veil 文件:
├── Header (22 bytes)
│   ├── magic: "VEILPKG\0" (8 bytes)
│   ├── version: u16
│   ├── header_size: u32
│   ├── metadata_size: u32
│   └── file_count: u32
├── Metadata Section (加密的 TLV 格式)
└── File Data Section
    ├── File 1 (name_len + name + encrypted_data)
    └── File 2 (...)
```

## 命令分类

### 工作区命令（操作容器名）
这些命令操作 `~/.veil/workspaces/` 中的工作区目录：

| 命令 | 参数 | 功能 |
|------|------|------|
| `init` | `<容器名>` | 创建工作区容器 |
| `add` | `<容器名> <文件路径>` | 添加文件到容器 |
| `rm` | `<容器名> <文件名>` | 从容器删除文件 |
| `ex` | `<容器名> <文件名> -o <输出路径>` | 导出文件 |
| `info` | `<容器名>` | 显示容器信息 |
| `mv` | `<容器名> <源名> <目标名>` | 重命名容器内文件 |
| `passwd` | `<容器名>` | 修改容器密码 |
| `free` | `<容器名>` | 树状显示内容 |
| `shell` | `<容器名>` | 交互式操作 |

**示例：**
```bash
# 创建容器（工作区）
veil init photos --password secret123

# 添加文件
veil add photos vacation.jpg --password secret123

# 查看信息
veil info photos --password secret123

# 重命名文件
veil mv photos vacation.jpg 2024-summer.jpg --password secret123
```

### 转换命令（工作区 ↔ .veil 文件）
用于在工作区和 .veil 打包格式之间转换：

| 命令 | 参数 | 功能 |
|------|------|------|
| `pack` | `<容器名> -o <输出.veil>` | 工作区 → .veil 文件 |
| `unpack` | `<输入.veil> -n <容器名>` | .veil 文件 → 工作区 |

**示例：**
```bash
# 打包工作区为 .veil 文件（便于分享）
veil pack photos -o photos-backup.veil --password secret123

# 解包 .veil 文件到工作区（便于编辑）
veil unpack photos-backup.veil -n photos-restored --password secret123
```

## 使用场景

### 场景 1：频繁编辑
**推荐：使用工作区命令**

```bash
# 1. 创建容器
veil init myproject

# 2. 逐步添加文件
veil add myproject file1.txt
veil add myproject file2.txt

# 3. 查看和编辑
veil info myproject
veil mv myproject file1.txt renamed.txt
veil ex myproject renamed.txt -o /tmp/edit.txt

# 编辑后重新添加
veil add myproject /tmp/edit.txt

# 4. 需要分享时打包
veil pack myproject -o myproject.veil
```

**优点：**
- ✅ 增量操作，不需要每次重写整个容器
- ✅ 独立文件加密，修改一个不影响其他
- ✅ 支持快速查看和管理

### 场景 2：一次性打包分享
**推荐：工作区 → pack → 分享**

```bash
# 1. 在工作区中准备文件
veil init archive
veil add archive doc1.pdf
veil add archive doc2.pdf
veil add archive photo.jpg

# 2. 打包为单文件
veil pack archive -o archive-2024.veil

# 3. 分享 archive-2024.veil 文件
# 接收方可以用 unpack 解包使用
```

### 场景 3：接收他人的 .veil 文件
**推荐：unpack → 使用工作区命令**

```bash
# 1. 解包到工作区
veil unpack received.veil -n received-data

# 2. 使用工作区命令查看和提取
veil info received-data
veil free received-data
veil ex received-data important.pdf -o ~/Documents/
```

## 工作区 vs .veil 文件

| 特性 | 工作区 | .veil 文件 |
|------|--------|-----------|
| **存储位置** | `~/.veil/workspaces/` | 任意位置 |
| **文件结构** | 目录 + 独立加密文件 | 单个打包文件 |
| **操作方式** | 增量修改 | 整体读写 |
| **适用场景** | 频繁编辑 | 分享传输 |
| **性能** | 快速增量操作 | 需要完整读写 |
| **便携性** | 需要工具 | 单文件可分享 |

## 元数据格式（.veil-meta）

工作区中的 `.veil-meta` 文件存储容器的加密元数据：

```
TLV Header (可变长度):
├── Magic: "VEILMETA" (8 bytes)
├── Header Length: u16
└── TLV Fields:
    ├── Version (1 byte tag + 2 bytes len + 2 bytes value)
    ├── Salt (1 byte tag + 2 bytes len + 32 bytes value)
    ├── Algorithm ID (1 byte tag + 2 bytes len + 1 byte value)
    └── Nonce (1 byte tag + 2 bytes len + 12 bytes value)

Encrypted JSON Data:
{
  "container_name": "photos",
  "workspace_type": "default",
  "created_at": "2024-01-01T00:00:00Z",
  "files": [
    {
      "encrypted_name": "abc123.enc",
      "original_name": "vacation.jpg",
      "size": 1024,
      "nonce": [12 bytes],
      "encrypted_at": "2024-01-01T00:00:00Z",
      "hash": null
    }
  ]
}
```

## 加密流程

### 文件加密
```
原始文件
  ↓
读取内容
  ↓
生成唯一 nonce (12 bytes)
  ↓
使用 ChaCha20-Poly1305 加密
  (key = Argon2id(password, container_salt))
  ↓
写入 <随机名>.enc
  ↓
更新 .veil-meta
```

### 元数据加密
```
元数据 JSON
  ↓
序列化为字节
  ↓
使用 AES-256-GCM 加密
  (key = Argon2id(password, salt))
  (nonce = 12 bytes from header)
  ↓
TLV Header + 加密数据
  ↓
写入 .veil-meta
```

## 密钥派生

使用 Argon2id 从密码派生密钥：

```rust
Argon2id {
    memory: 64 MB,
    iterations: 3,
    parallelism: 4,
    output: 32 bytes (256 bits)
}

master_key = Argon2id(password, salt)
```

**安全特性：**
- ✅ 抗 GPU/ASIC 暴力破解
- ✅ 抗侧信道攻击
- ✅ 每个容器独立 salt
- ✅ 每个文件独立 nonce

## 全局配置（config.toml）

```toml
version = "2.0.0"

[workspaces]
default = "/Users/username/.veil/workspaces/default"

[containers.photos]
workspace_type = "default"
container_dir = "photos"
created_at = "2024-01-01T00:00:00Z"

[containers.documents]
workspace_type = "default"
container_dir = "documents"
created_at = "2024-01-02T00:00:00Z"
```

**作用：**
- 记录所有容器的位置
- 支持多工作区类型
- 容器名称到路径的映射

## 设计优势

### 1. 灵活性
- 工作区支持增量操作
- .veil 格式支持整体分享
- 两种方式可自由转换

### 2. 安全性
- 独立文件加密
- 独立 nonce 防重放
- Argon2id 抗暴力破解

### 3. 易用性
- 简单的容器名称
- 全局配置自动管理
- 交互式 shell 批量操作

### 4. 性能
- 增量修改不重写全部
- 独立文件并发访问
- 原子写入保证一致性

## 后续扩展

### 计划功能
- [ ] 支持多工作区类型
- [ ] 容器快照和版本管理
- [ ] 压缩支持（可选）
- [ ] 文件完整性校验
- [ ] 目录递归添加

### 兼容性
- v2.0 工作区格式向前兼容
- pack/unpack 保持 .veil 格式稳定
- 元数据 TLV 格式易于扩展
