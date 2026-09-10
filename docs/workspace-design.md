# Veil 工作区架构设计文档

## 1. 背景与问题

### 1.1 当前问题

Veil 当前的容器设计是将多个加密文件拼接成单一容器文件。这导致：

- **无法并发读写**：系统层面不支持对同一文件的多个位置同时写入
- **性能瓶颈**：每次添加/删除文件都需要重写整个容器
- **可用性受限**：无法支持多线程操作，限制了实际使用场景

### 1.2 解决方案

引入**工作区（Workspace）**概念：

- 工作区存储独立的加密文件（每个文件单独加密）
- 支持并发读写（文件系统原生支持）
- 容器文件（`.veil`）作为打包格式，用于分享和备份

---

## 2. 核心概念

### 2.1 层级关系

```
工作区（Workspace）
  └── 容器目录（Container Directory）
      ├── .veil-meta（容器元数据）
      └── *.enc（加密文件）
```

**示例**：
```
~/.veil/workspaces/default/          ← 工作区（可包含多个容器）
  ├── myfiles/                       ← 容器 myfiles
  │   ├── .veil-meta
  │   ├── a3f2c1d4.enc
  │   └── b7e4f9a8.enc
  ├── backup/                        ← 容器 backup
  │   ├── .veil-meta
  │   └── c9f1e8d7.enc
  └── photos/                        ← 容器 photos
      ├── .veil-meta
      └── e6f8a9b2.enc
```

### 2.2 工作区（Workspace）

**定义**：一个物理目录，可以包含多个容器

**类型**：
- **默认工作区**：`~/.veil/workspaces/default/`（开箱即用）
- **自定义工作区**：用户创建的命名工作区（如 `work`、`personal`）
- **专属工作区**：某个容器独占的工作区（敏感数据隔离）

**特点**：
- 多个容器可共享一个工作区
- 便于统一管理和备份
- 工作区之间完全独立

#### 工作区类型详解

**A. 默认工作区（共享）**

```
~/.veil/workspaces/default/
  ├── myfiles/           ← 容器 myfiles
  ├── backup/            ← 容器 backup
  └── photos/            ← 容器 photos
```

- 自动创建，无需配置
- 多个容器共享同一工作区目录
- 适合大多数使用场景

**B. 自定义工作区（共享）**

```
~/Documents/veil-work/      ← 自定义工作区 "work"
  ├── project-a/            ← 容器 project-a
  ├── project-b/            ← 容器 project-b
  └── credentials/          ← 容器 credentials

~/Documents/veil-personal/  ← 自定义工作区 "personal"
  ├── documents/            ← 容器 documents
  └── photos/               ← 容器 photos
```

- 用户自定义位置
- 按项目/类别组织容器
- 多个容器共享同一工作区目录

**C. 专属工作区（独占）**

```
~/EncryptedVolume/veil/     ← 专属工作区
  ├── .veil-meta            ← 容器 secrets 的元数据（直接在根目录）
  └── *.enc                 ← 加密文件
```

- 一个容器独占整个工作区
- 无需子目录（根目录即容器目录）
- 适合敏感数据隔离（可放在加密分区）

### 2.3 容器目录（Container Directory）

**定义**：工作区下的子目录，存储一个容器的加密文件

```
~/.veil/workspaces/default/myfiles/  ← 容器目录
  ├── .veil-meta                     ← 容器元数据
  ├── a3f2c1d4.enc                   ← 加密文件（原名：photo.jpg）
  └── b7e4f9a8.enc                   ← 加密文件（原名：notes.txt）
```

**特点**：
- 每个文件独立加密，支持并发读写
- 文件名加密（使用随机 ID）
- 元数据记录在 `.veil-meta` 中

### 2.4 链接文件（.veil-link）

**定义**：指向容器目录的配置文件/快捷方式

**共享工作区示例**：
```toml
# myfiles.veil-link 内容示例
version = "1.0"
created_at = "2026-09-10T10:00:00Z"

[workspace]
veil_id = "veil-..."
container_name = "myfiles"
# 相对于 volume_id 对应卷根目录的路径
path = ".veil/workspaces/default/myfiles"
volume_id = "fs-uuid:..."
volume_label = "MyUSB"
created_at = "2026-09-10T10:00:00Z"

[encryption]
algorithm = "AES-256-GCM"
key_derivation = "Argon2id"
```

**本地工作区示例**：
```toml
# secrets.veil-link 内容示例
[workspace]
veil_id = "veil-..."
container_name = "secrets"
path = ".veil/workspaces/default/secrets"
volume_id = "fs-uuid:..."
volume_label = "local-system"
created_at = "2026-09-10T11:00:00Z"

[encryption]
algorithm = "AES-256-GCM"
key_derivation = "Argon2id"
```

**特点**：
- 文件很小（< 1KB）
- 用户日常操作时使用
- 可以删除和重新生成，不影响数据
- 可以创建多个链接指向同一容器目录
- 支持相对路径（便于跨设备）

### 2.5 容器元数据（.veil-meta）

**定义**：每个容器目录内的元数据文件

**位置示例**：
- 共享工作区：`~/.veil/workspaces/default/myfiles/.veil-meta`
- 自定义工作区：`~/Documents/veil-work/project-a/.veil-meta`
- 专属工作区：`~/EncryptedVolume/veil/.veil-meta`（直接在根目录）

**作用**：
- 明文恢复区记录 `veil_id`、容器名称和工作区类型，不包含任何路径
- 记录文件名映射（加密文件名 ↔ 原始文件名）
- 记录加密参数（salt、nonce 等）
- 记录容器统计信息

**内容示例**（JSON 格式）：
```json
{
  "version": "1.0",
  "veil_id": "veil-...",
  "container_name": "myfiles",
  "workspace_type": "default",
  "created_at": "2026-09-10T10:00:00Z",
  "encryption": {
    "algorithm": "AES-256-GCM",
    "key_derivation": "Argon2id",
    "salt": "base64-encoded-salt"
  },
  "files": [
    {
      "encrypted_name": "a3f2c1d4.enc",
      "original_name": "photo.jpg",
      "size": 1024000,
      "hash": "sha256:..."
    }
  ]
}
```

**特点**：
- 每个容器有且仅有一个 `.veil-meta`
- 与容器目录在同一位置
- 使用原子写入保证一致性

### 2.6 容器文件（.veil）

**定义**：容器目录的打包格式

```
myfiles.veil 结构：
  [Header: 元数据]
  [File 1: photo.jpg.enc]
  [File 2: notes.txt.enc]
  [File 3: music.mp3.enc]
  ...
```

**特点**：
- 包含所有加密数据
- 自包含，可独立解包
- 用于分享、备份、跨设备传输
- 文件大小 = 所有加密文件的总和

---

## 3. 配置文件层级

### 3.1 全局配置（~/.veil/config.toml）

**定义**：用户级别的全局配置，统管所有工作区和容器

**路径**：`~/.veil/config.toml`

**作用**：
- 记录所有工作区定义
- 记录所有容器到工作区的映射
- 全局偏好设置
- 系统状态

**内容示例**：
```toml
version = "1.0"

# 系统信息
[system]
icons_configured = true
icons_configured_at = "2026-09-10T10:30:00Z"
icons_version = "1.0"
first_run = false

# 默认工作区
[workspace.default]
path = "~/.veil/workspaces/default"

# 自定义工作区
[workspace.custom.work]
path = "~/Documents/veil-work"
description = "工作相关的加密文件"
created_at = "2026-09-10T10:30:00Z"

[workspace.custom.personal]
path = "~/Documents/veil-personal"
description = "个人文件"
created_at = "2026-09-10T10:35:00Z"

# 容器映射（容器名 → 工作区）
[containers.myfiles]
workspace = "default"           # 使用默认工作区
container_dir = "myfiles"       # 在工作区中的子目录名
created_at = "2026-09-10T11:00:00Z"
last_accessed = "2026-09-10T14:30:00Z"

[containers.backup]
workspace = "default"
container_dir = "backup"
created_at = "2026-09-10T11:05:00Z"

[containers.project-a]
workspace = "work"              # 使用自定义工作区 work
container_dir = "project-a"
created_at = "2026-09-10T11:10:00Z"

[containers.secrets]
workspace_path = "~/EncryptedVolume/veil"  # 专属工作区（绝对路径）
dedicated = true                           # 标记为专属
created_at = "2026-09-10T11:20:00Z"

# 用户偏好
[preferences]
hints_level = "full"               # full | brief | off
auto_sync = false
default_key_derivation = "Argon2id"
cache_keys = false
log_level = "info"

# 默认加密参数
[encryption.defaults]
algorithm = "AES-256-GCM"
key_derivation = "Argon2id"

[encryption.argon2id]
memory_kb = 65536
iterations = 3
parallelism = 4
```

**查找逻辑**：
```
用户操作 myfiles.veil-link
  ↓
读取 .veil-link 获取容器名称 "myfiles"
  ↓
查询 config.toml 中的 [containers.myfiles]
  ↓
找到 workspace = "default"
  ↓
查询 [workspace.default]
  ↓
获得工作区路径：~/.veil/workspaces/default/
  ↓
拼接容器目录：~/.veil/workspaces/default/myfiles/
```

---

### 3.2 配置文件对比

| 文件 | 作用范围 | 数量 | 内容 |
|------|----------|------|------|
| **config.toml** | 全局 | 1 个 | 所有工作区、所有容器映射、全局设置 |
| **.veil-link** | 单个容器 | 每个容器 1 个 | 指向容器目录、加密参数模板 |
| **.veil-meta** | 单个容器 | 每个容器 1 个 | 文件列表、加密参数、统计信息 |

**配置优先级**：
```
.veil-meta（实际使用）> .veil-link（模板）> config.toml（全局默认）
```

---

## 4. 工作区类型详解

### 4.1 默认工作目录

**路径**：`~/.veil/workspaces/default/`

**用途**：
- 新用户不需要配置，开箱即用
- 未指定工作目录时的默认位置

**示例**：
```bash
veil init myfiles
# 自动创建在 ~/.veil/workspaces/default/myfiles/
```

### 4.2 自定义工作目录

**定义**：用户创建的命名工作目录

**用途**：
- 组织分类（工作、个人、项目等）
- 放在不同磁盘分区
- 便于备份管理

**示例**：
```bash
# 创建自定义工作目录
veil workspace add work ~/Documents/veil-work --desc "工作相关文件"
veil workspace add personal ~/Documents/veil-personal --desc "个人文件"

# 使用自定义工作目录
veil init project-a --workspace work
veil init photos --workspace personal
```

### 4.3 容器专属工作目录

**定义**：某个容器独占的工作目录

**用途**：
- 敏感数据隔离
- 放在加密分区（如 VeraCrypt 卷）
- 不与其他容器混在一起

**示例**：
```bash
# 创建专属工作目录的容器
veil init secrets \
  --workspace ~/EncryptedVolume/veil \
  --dedicated

# 结果：
# - secrets 独占 ~/EncryptedVolume/veil/
# - 不会和其他容器共享目录
```

---

## 4. 全局配置

### 4.1 配置文件位置

`~/.veil/config.toml`

### 4.2 配置文件结构

```toml
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

# 容器映射（容器名 → 工作目录配置）
[containers.myfiles]
workspace = "default"
created_at = "2026-09-10T11:00:00Z"

[containers.project-a]
workspace = "work"
created_at = "2026-09-10T11:10:00Z"

[containers.secrets]
workspace_path = "~/EncryptedVolume/veil"
dedicated = true
created_at = "2026-09-10T11:20:00Z"

# 用户偏好
[preferences]
hints_level = "full"  # full | brief | off
auto_sync = false
```

---

## 5. 命令行接口设计

### 5.1 工作区管理

```bash
# 列出所有工作目录
veil workspace list

# 添加自定义工作目录
veil workspace add <name> <path> [--desc <description>]

# 删除工作目录
veil workspace rm <name>

# 查看容器的工作目录
veil workspace show <container>

# 打印工作目录路径（用于脚本）
veil workspace path <container>
```

### 5.2 容器操作

```bash
# 创建容器（自动创建 .veil-link）
veil init <name>                           # 使用默认工作目录
veil init <name> --workspace <workspace>   # 使用指定工作目录
veil init <name> --workspace <path> --dedicated  # 专属工作目录

# 添加/删除文件
veil add <file> <container.veil-link>
veil rm <file> <container.veil-link>

# 列出文件
veil list <container.veil-link>

# 提取文件
veil extract <file> <container.veil-link> [-o output]

# 打包成容器文件
veil pack <container.veil-link> [-o output.veil]

# 解包容器文件
veil unpack <container.veil>

# 同步（从 .veil 更新到工作区）
veil sync <container.veil-link> <container.veil>

# 查看信息
veil info <file>  # 自动识别 .veil-link 或 .veil

# 重建链接文件
veil link <workspace-path> [-o output.veil-link]
```

### 5.3 容器迁移

```bash
# 迁移容器到不同工作目录
veil move <container> --to <workspace>

# 迁移到专属目录
veil move <container> --to <path> --dedicated
```

### 5.4 配置管理

```bash
# 设置提示级别
veil config --hints=full|brief|off

# 设置默认工作目录
veil config --default-workspace <path>

# 查看配置
veil config show
```

---

## 6. 用户引导策略

### 6.1 设计原则

**渐进式揭示复杂度**：
1. **新手**：不需要知道工作区，以为操作的就是 `.veil-link`
2. **进阶**：理解工作区存在，知道可以直接操作工作区
3. **高级**：完全理解架构，直接操作工作区目录

### 6.2 首次使用提示

```bash
$ veil init myfiles
✓ 已创建加密工作区 myfiles
✓ 已生成 myfiles.veil-link

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
📌 myfiles.veil-link 是什么？
• 这是指向加密工作区的"快捷方式"
• 文件很小（<1KB），可以随意复制或删除
• 删除它不会丢失数据，可以随时重新创建
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

💡 提示：不想每次看到这个？运行 veil config --hints=brief
```

### 6.3 首次打包提示

```bash
$ veil pack myfiles.veil-link
正在打包工作区...
✓ 已生成 myfiles.veil (156 MB)

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
📦 .veil 和 .veil-link 的区别：

myfiles.veil-link (1KB)
  ↳ 快捷方式，指向本地工作区
  ↳ 只能在这台电脑上使用

myfiles.veil (156MB)
  ↳ 完整数据包，包含所有加密文件
  ↳ 可以分享给别人或传到其他设备
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

### 6.4 文件类型对比表

在 `veil help files` 中显示：

```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
          📌 .veil-link      📦 .veil
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
大小      < 1KB              与数据量相同
作用      指向本地工作区      完整数据包
用途      日常操作            分享/备份/传输
删除后    数据不丢失          数据丢失（除非工作区还在）
可分享    ✗ 不能              ✓ 可以
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

类比：
  .veil-link  ←→  快捷方式 / 书签
  .veil       ←→  ZIP 压缩包
```

### 6.5 关键时机提示

#### 解包时

```bash
$ veil unpack photos.veil
正在解包...
✓ 已解包到工作区（458 个文件）
✓ 已生成 photos.veil-link

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
📦 photos.veil → 📌 photos.veil-link

• photos.veil 是别人分享给你的数据包
• 已解包到本地工作区，可以删除 photos.veil 了
• 以后用 photos.veil-link 操作文件即可
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

#### 误删 .veil-link 恢复

```bash
$ veil list myfiles.veil-link
✗ 错误：找不到 myfiles.veil-link

检测到工作区还存在，是否重新创建链接？[Y/n] y
✓ 已重新生成 myfiles.veil-link

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
💡 没关系！

• .veil-link 只是快捷方式，删除不影响数据
• 你的文件还在工作区中
• 已为你重新创建链接文件
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

#### 同时存在两种文件

```bash
$ veil list myfiles
检测到两个文件：
  1. myfiles.veil-link (1KB)  - 本地快捷方式
  2. myfiles.veil (156MB)     - 数据包

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
🤔 你可能想要：

• 查看本地文件       → veil list myfiles.veil-link
• 解包 myfiles.veil  → veil unpack myfiles.veil
• 同步数据包的更新   → veil sync myfiles.veil-link myfiles.veil
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

默认使用 myfiles.veil-link [回车继续，或 Ctrl+C 取消]
```

### 6.6 隐藏文件扩展名问题

**问题**：macOS/Windows 默认隐藏扩展名，`myfiles.veil-link` 和 `myfiles.veil` 都显示为 `myfiles`

**解决方案**：

1. **不同基础名称**：打包时使用不同命名
   ```bash
   veil pack myfiles.veil-link
   → 默认输出：myfiles.vault.veil
   # 隐藏扩展名后显示为：myfiles 和 myfiles.vault
   ```

2. **CLI 自动识别**：
   ```bash
   veil list myfiles  # 优先查找 myfiles.veil-link
   ```

3. **推荐开启扩展名显示**：
   ```bash
   首次运行提示：
   💡 建议开启文件扩展名显示，方便区分文件类型
   • macOS: 访达 → 设置 → 高级 → 显示所有文件扩展名
   • Windows: 文件资源管理器 → 查看 → 文件扩展名
   ```

---

## 7. 使用场景

### 7.1 本地使用

```bash
# 创建
veil init photos
→ photos.veil-link + 工作区

# 日常操作
veil add vacation/*.jpg photos.veil-link
veil list photos.veil-link
veil extract photo.jpg photos.veil-link

# 不需要生成 .veil 文件
```

### 7.2 分享文件

```bash
# 电脑 A：打包
veil pack photos.veil-link -o photos.veil
# 发送 photos.veil 给朋友

# 电脑 B：接收
veil unpack photos.veil
→ 自动生成 photos.veil-link
→ 可以正常使用
```

### 7.3 跨设备同步

```bash
# 设备 A
veil add newfile.txt myfiles.veil-link
veil pack myfiles.veil-link -o myfiles.veil
# 拷贝 myfiles.veil 到设备 B

# 设备 B
veil sync myfiles.veil-link myfiles.veil
# 或：veil unpack myfiles.veil --force
```

### 7.4 敏感数据隔离

```bash
# 创建在加密分区
veil init secrets \
  --workspace /Volumes/EncryptedDisk/veil \
  --dedicated

# 操作
veil add sensitive.doc secrets.veil-link

# 锁定时断开加密分区
# 解锁时重新挂载加密分区
```

### 7.5 项目分类管理

```bash
# 创建工作目录
veil workspace add work ~/Documents/veil-work
veil workspace add personal ~/Documents/veil-personal

# 按类别创建容器
veil init project-a --workspace work
veil init project-b --workspace work
veil init family-photos --workspace personal

# 查看
veil workspace list
→ work: 2 个容器
→ personal: 1 个容器
```

---

## 8. 技术实现要点

### 8.1 工作区元数据（.veil-meta）

存储在每个工作区目录中：

```json
{
  "version": "1.0",
  "container_name": "myfiles",
  "created_at": "2026-09-10T10:00:00Z",
  "encryption": {
    "algorithm": "AES-256-GCM",
    "key_derivation": "Argon2id"
  },
  "files": [
    {
      "encrypted_name": "a3f2c1d.enc",
      "original_name": "photo.jpg",
      "size": 1024000,
      "encrypted_at": "2026-09-10T10:05:00Z"
    },
    {
      "encrypted_name": "b7e4f9a.enc",
      "original_name": "notes.txt",
      "size": 2048,
      "encrypted_at": "2026-09-10T10:06:00Z"
    }
  ]
}
```

### 8.2 文件名加密

为了安全性，实际存储的文件名应该加密或使用随机 ID：

```
工作区目录：
  ├── .veil-meta           # 元数据（文件名映射）
  ├── a3f2c1d4e5f6.enc     # photo.jpg 加密后
  ├── b7e4f9a8c3d2.enc     # notes.txt 加密后
  └── c9f1e8d7a6b5.enc     # music.mp3 加密后
```

### 8.3 并发读写支持

- 每个文件独立加密，可以并发操作
- 使用文件系统锁防止冲突
- `.veil-meta` 使用原子写入（写临时文件 + rename）

### 8.4 容器格式（.veil）

```
[Header]
  magic: "VEIL"
  version: 1
  file_count: 3
  metadata_size: 1024
  
[Metadata]
  JSON 格式的元数据
  
[File 1 Header]
  name_length: 9
  name: photo.jpg
  size: 1024000
  
[File 1 Data]
  加密数据...
  
[File 2 Header]
  ...
```

### 8.5 智能文件识别

```rust
fn resolve_container(input: &str) -> Result<ContainerType> {
    let path = Path::new(input);
    
    // 1. 检查扩展名
    match path.extension() {
        Some("veil-link") => return Ok(ContainerType::Link(path)),
        Some("veil") => return Ok(ContainerType::Container(path)),
        _ => {}
    }
    
    // 2. 尝试添加 .veil-link
    let link_path = path.with_extension("veil-link");
    if link_path.exists() {
        return Ok(ContainerType::Link(link_path));
    }
    
    // 3. 尝试添加 .veil
    let veil_path = path.with_extension("veil");
    if veil_path.exists() {
        return Ok(ContainerType::Container(veil_path));
    }
    
    // 4. 在配置中查找容器名
    if let Some(workspace) = find_workspace_by_name(input)? {
        return Ok(ContainerType::Workspace(workspace));
    }
    
    Err(Error::ContainerNotFound(input.to_string()))
}
```

---

## 9. 配置提示系统

### 9.1 提示级别

```rust
enum HintLevel {
    Full,   // 显示所有提示（新手）
    Brief,  // 只显示重要提示（熟悉后）
    Off,    // 关闭所有提示（高级用户）
}
```

### 9.2 提示类型

```rust
enum HintType {
    FirstInit,           // 首次创建容器
    FirstPack,           // 首次打包
    FirstUnpack,         // 首次解包
    LinkRecovery,        // 恢复链接文件
    FileTypeAmbiguity,   // 同时存在两种文件
    ExtensionHidden,     // 检测到隐藏扩展名
}
```

### 9.3 提示渲染

```rust
fn render_hint(hint_type: HintType, context: &Context) {
    let level = get_hint_level();
    
    if level == HintLevel::Off {
        return;
    }
    
    match hint_type {
        HintType::FirstInit if level >= HintLevel::Full => {
            print_box(
                "📌 myfiles.veil-link 是什么？",
                "• 这是指向加密工作区的\"快捷方式\"\n\
                 • 文件很小（<1KB），可以随意复制或删除\n\
                 • 删除它不会丢失数据，可以随时重新创建"
            );
        }
        // ... 其他提示
    }
}
```

---

## 10. 迁移方案

### 10.1 从旧版本迁移

```bash
# 自动检测旧版本容器
veil migrate old-container.veil
→ 解析旧格式
→ 创建工作区
→ 解密文件到工作区
→ 生成新的 .veil-link

# 批量迁移
veil migrate --all
→ 扫描当前目录的所有 .veil 文件
→ 逐个迁移
```

### 10.2 兼容性

- 保留旧格式读取能力
- 新版本只写新格式
- 提供转换工具

---

## 11. 安全考虑

### 11.1 工作区安全

- 工作区中的文件已加密（`.enc` 文件）
- 不存储明文数据
- 文件名加密或使用随机 ID

### 11.2 密钥管理

- 密钥不存储在工作区
- 每次操作时从密码派生密钥
- 支持密钥缓存（可选）

### 11.3 权限控制

- 工作区目录权限：700（仅所有者）
- 配置文件权限：600
- 自动检查并修复权限

---

## 12. 性能优化

### 12.1 并发操作

- 支持多线程同时加密不同文件
- 文件系统原生支持，无需额外同步

### 12.2 增量更新

- 只操作变化的文件
- 不需要重写整个容器

### 12.3 延迟打包

- 日常操作只在工作区进行
- 需要分享时才打包成 `.veil`
- 避免频繁的加密/解密开销

---

## 13. 文件图标自动设置

### 13.1 设计目标

**完全静默、无感设置**：
- 用户首次运行任何命令时自动设置图标
- 后台静默完成，不显示任何提示
- 失败不影响核心功能继续运行
- 记录状态，避免每次都检查

### 13.2 实现方案

#### macOS

```rust
#[cfg(target_os = "macos")]
fn setup_file_icons() -> Result<()> {
    // 1. 准备图标文件（从二进制提取）
    let icon_dir = dirs::data_local_dir()
        .unwrap()
        .join("veil/icons");
    std::fs::create_dir_all(&icon_dir)?;
    
    std::fs::write(
        icon_dir.join("veil-link.icns"),
        include_bytes!("../assets/veil-link.icns")
    )?;
    std::fs::write(
        icon_dir.join("veil.icns"),
        include_bytes!("../assets/veil.icns")
    )?;
    
    // 2. 创建 plist 文件定义文件类型
    // 3. 使用 lsregister 注册（静默执行）
    let _ = Command::new("/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister")
        .arg("-f")
        .arg(plist_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    
    Ok(())
}
```

#### Windows

```rust
#[cfg(target_os = "windows")]
fn setup_file_icons() -> Result<()> {
    use winreg::RegKey;
    use winreg::enums::*;
    
    // 1. 提取图标文件
    let icon_dir = dirs::data_local_dir()
        .unwrap()
        .join("veil\\icons");
    std::fs::create_dir_all(&icon_dir)?;
    
    std::fs::write(
        icon_dir.join("veil-link.ico"),
        include_bytes!("../assets/veil-link.ico")
    )?;
    
    // 2. 注册到 HKCU（不需要管理员权限）
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    
    let (key, _) = hkcu.create_subkey(r"Software\Classes\.veil-link")?;
    key.set_value("", &"VeilLink")?;
    
    // 3. 刷新图标缓存
    unsafe {
        use winapi::um::shellapi::{SHChangeNotify, SHCNE_ASSOCCHANGED};
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, 
                      std::ptr::null(), std::ptr::null());
    }
    
    Ok(())
}
```

#### Linux

```rust
#[cfg(target_os = "linux")]
fn setup_file_icons() -> Result<()> {
    // 1. 提取图标
    let icon_dir = dirs::data_local_dir()
        .unwrap()
        .join("icons/hicolor/256x256/apps");
    std::fs::create_dir_all(&icon_dir)?;
    
    std::fs::write(
        icon_dir.join("veil-link.png"),
        include_bytes!("../assets/veil-link.png")
    )?;
    
    // 2. 创建 MIME 类型定义
    let mime_dir = dirs::data_local_dir()
        .unwrap()
        .join("mime/packages");
    std::fs::create_dir_all(&mime_dir)?;
    
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<mime-info xmlns="http://www.freedesktop.org/standards/shared-mime-info">
  <mime-type type="application/vnd.veil.link">
    <comment>Veil Link File</comment>
    <glob pattern="*.veil-link"/>
    <icon name="veil-link"/>
  </mime-type>
  <mime-type type="application/vnd.veil.container">
    <comment>Veil Container</comment>
    <glob pattern="*.veil"/>
    <icon name="veil-container"/>
  </mime-type>
</mime-info>"#;
    
    std::fs::write(mime_dir.join("veil.xml"), xml)?;
    
    // 3. 更新数据库（静默）
    let _ = Command::new("update-mime-database")
        .arg(dirs::data_local_dir().unwrap().join("mime"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    
    Ok(())
}
```

### 13.3 入口点集成

```rust
// src/main.rs
fn main() {
    // 首次运行时自动设置图标（静默、后台）
    ensure_file_icons();
    
    // 继续执行用户命令
    let args = Args::parse();
    // ...
}

fn ensure_file_icons() {
    // 检查是否已设置
    if is_icons_configured() {
        return;
    }
    
    // 后台线程静默设置
    thread::spawn(|| {
        if setup_file_icons().is_ok() {
            mark_icons_configured();
        }
        // 失败不影响主流程
    });
}

fn is_icons_configured() -> bool {
    let config = load_config().unwrap_or_default();
    config.system.icons_configured
}

fn mark_icons_configured() {
    let mut config = load_config().unwrap_or_default();
    config.system.icons_configured = true;
    config.system.icons_configured_at = Some(chrono::Utc::now().to_rfc3339());
    config.system.icons_version = Some("1.0".to_string());
    let _ = save_config(&config);
}
```

### 13.4 配置文件扩展

```toml
# ~/.veil/config.toml
[system]
icons_configured = true
icons_configured_at = "2026-09-10T10:30:00Z"
icons_version = "1.0"
```

### 13.5 图标设计

**📌 .veil-link 图标**：
- 蓝色调
- 链接符号 + 锁
- 轻量感（表示小文件）

**📦 .veil 图标**：
- 橙色/金色调
- 盒子/容器 + 锁
- 厚重感（表示数据包）

### 13.6 图标资源

```
assets/
  ├── veil-link.icns     # macOS 图标
  ├── veil-link.ico      # Windows 图标
  ├── veil-link.png      # Linux 图标
  ├── veil.icns          # macOS 图标
  ├── veil.ico           # Windows 图标
  └── veil.png           # Linux 图标
```

图标文件通过 `include_bytes!()` 嵌入二进制，无需外部文件。

### 13.7 版本更新

```rust
fn should_update_icons() -> bool {
    let config = load_config().unwrap_or_default();
    const CURRENT_VERSION: &str = "1.0";
    
    match config.system.icons_version.as_deref() {
        Some(v) if v == CURRENT_VERSION => false,
        _ => true  // 未设置或版本旧
    }
}
```

### 13.8 卸载清理

```bash
# 可选：提供清理命令
veil uninstall --clean-icons
```

### 13.9 依赖库

```toml
[target.'cfg(target_os = "windows")'.dependencies]
winreg = "0.52"
winapi = { version = "0.3", features = ["shellapi"] }
```

macOS 和 Linux 使用系统工具，无需额外依赖。

---

## 14. 实现优先级

### Phase 1：核心功能（P0）

- [ ] 全局配置系统（`config.toml`）
- [ ] 默认工作目录
- [ ] `.veil-link` 格式定义
- [ ] 基础命令：`init`, `add`, `list`, `extract`, `rm`
- [ ] 工作区元数据（`.veil-meta`）
- [ ] **文件图标自动设置（静默）**

### Phase 2：打包与分享（P1）

- [ ] `.veil` 容器格式
- [ ] `pack` 命令
- [ ] `unpack` 命令
- [ ] 文件类型自动识别

### Phase 3：高级功能（P2）

- [ ] 自定义工作目录
- [ ] 容器专属工作目录
- [ ] `workspace` 命令组
- [ ] `move` 命令（迁移容器）
- [ ] `sync` 命令

### Phase 4：用户体验（P3）

- [ ] 提示系统框架
- [ ] 各类场景提示
- [ ] `info` 命令增强
- [ ] `help files` 文档

### Phase 5：工具与维护（P4）

- [ ] 旧版本迁移工具
- [ ] 工作区健康检查
- [ ] 垃圾清理工具
- [ ] 自动备份功能

---

## 15. 待讨论问题

### 14.1 命名

- [ ] 使用 `.veil-link` 还是 `.vlink`？
  - **决定**：使用 `.veil-link`（更清晰）

- [ ] 打包文件默认命名规则？
  - 选项 1：`myfiles.vault.veil`
  - 选项 2：`myfiles.pkg.veil`
  - 选项 3：`myfiles-packed.veil`

### 14.2 默认行为

- [ ] `veil add` 是否自动同步到 `.veil`？
  - **建议**：默认不同步，需要时手动 `pack`

- [ ] 删除 `.veil-link` 时是否提示？
  - **建议**：首次删除时提示，可配置

### 14.3 安全性

- [ ] 是否支持工作区加密（双重加密）？
- [ ] 是否支持工作区在内存文件系统（tmpfs）？

---

## 15. 文档与教程

### 15.1 用户文档

- [ ] 快速入门指南
- [ ] 完整命令参考
- [ ] 最佳实践
- [ ] 故障排除

### 15.2 开发者文档

- [ ] 架构设计文档（本文档）
- [ ] API 文档
- [ ] 文件格式规范
- [ ] 贡献指南

---

## 16. 测试计划

### 16.1 单元测试

- [ ] 配置文件解析
- [ ] 文件类型识别
- [ ] 工作区元数据操作
- [ ] 加密/解密正确性

### 16.2 集成测试

- [ ] 完整的工作流测试
- [ ] 并发读写测试
- [ ] 迁移测试

### 16.3 用户体验测试

- [ ] 新手引导流程
- [ ] 提示信息可读性
- [ ] 错误恢复能力

---

## 附录 A：术语表

- **工作区（Workspace）**：存储独立加密文件的目录
- **链接文件（.veil-link）**：指向工作区的配置文件
- **容器文件（.veil）**：工作区的打包格式
- **默认工作目录**：系统默认的工作区位置
- **自定义工作目录**：用户创建的命名工作区
- **容器专属工作目录**：某个容器独占的工作区

## 附录 B：类比说明

| Veil 概念 | 类比 |
|-----------|------|
| 工作区 | Git 仓库（.git 目录）|
| .veil-link | 快捷方式 / 书签 |
| .veil | ZIP 压缩包 / tar 归档 |
| pack | git archive |
| unpack | unzip / tar extract |

---

**文档版本**：1.0  
**创建日期**：2026-09-10  
**最后更新**：2026-09-10  
**作者**：Veil 开发团队
