# Veil CLI

Veil的命令行工具 —— 一个简单、安全、高效的文件加密容器管理工具。

## 特性

- 🔒 **安全加密**：基于 age 加密（X25519 + ChaCha20-Poly1305）
- 🔑 **两级密钥**：密码加密私钥，私钥加密内容，修改密码无需重新加密数据
- 📦 **单文件容器**：所有数据存储在一个 `.veil` 文件中
- 🌲 **目录树结构**：支持嵌套目录，保持文件组织
- ⚡ **批处理模式**：Shell 模式性能提升 2-6 倍，适合批量操作
- 🎯 **灵活参数**：支持位置参数和选项参数，可灵活混用
- 🔐 **安全密码输入**：交互式输入不回显，支持环境变量和命令行参数
- ✅ **完整测试**：22 个集成测试，覆盖所有核心功能

## 安装

```bash
# 从源码编译
cargo build --release --package veil-cli

# 可执行文件位置
./target/release/veil

# 可选：安装到系统路径
cargo install --path crates/veil-cli
```

## 快速开始

```bash
# 1. 创建加密容器
veil init photos.veil
请输入密码: ****
请再次输入密码: ****
✓ 容器创建成功

# 2. 添加文件
veil add photos.veil vacation.jpg 2024/vacation.jpg
✓ 文件已添加

# 3. 查看内容
veil free photos.veil
容器内容:
└── 2024/
    └── vacation.jpg

# 4. 导出文件
veil ex photos.veil 2024/vacation.jpg ./recovered.jpg
✓ 文件已导出

# 5. 批处理模式（性能优化）
veil shell photos.veil
veil> add photo1.jpg
veil> add photo2.jpg
veil> free
veil> exit
```

## 命令概览

```
veil init       创建新容器
veil add        添加文件/目录到容器
veil rm         删除容器内的文件
veil mv         移动/重命名文件
veil free       树状显示容器内容
veil ex         导出文件/目录
veil info       显示容器信息
veil passwd     修改容器密码
veil shell      交互式批处理模式（性能优化）
```

## 使用方式

所有命令支持**位置参数**（简洁）和**选项参数**（清晰）两种方式，可以灵活混用。

### 批处理模式（推荐用于批量操作）

**Shell 模式**可以一次解密私钥，然后执行多个命令，性能提升 2-6 倍：

```bash
# 启动 shell 模式
$ veil shell photos.veil
请输入密码: ****
容器已打开，进入交互模式 ✓
输入 'help' 查看可用命令，'exit' 退出

# 批量添加文件（私钥只解密一次）
veil> add vacation1.jpg photos/vacation1.jpg
✓ 文件已添加: photos/vacation1.jpg

veil> add vacation2.jpg photos/vacation2.jpg
✓ 文件已添加: photos/vacation2.jpg

veil> add vacation3.jpg photos/vacation3.jpg
✓ 文件已添加: photos/vacation3.jpg

# 查看内容
veil> free
容器内容:
└── photos/
    ├── vacation1.jpg
    ├── vacation2.jpg
    └── vacation3.jpg

# 查看信息
veil> info
容器信息:
  文件数量: 3
  总大小: 1.2 MB

# 退出
veil> exit
正在退出...
```

**Shell 模式支持的命令**：
- `add <source> [dest]` - 添加文件/目录
- `rm <path>` - 删除文件
- `mv <from> <to>` - 移动/重命名文件
- `free` / `ls` - 树状显示内容
- `info` - 显示容器信息
- `ex <input> <output>` - 导出文件
- `help` - 显示帮助
- `exit` / `quit` - 退出 shell

### 1. 创建容器

```bash
# 位置参数: veil init <容器> <密码>
veil init photos.veil mypassword

# 选项参数
veil init photos.veil -p mypassword

# 交互式（最安全，会提示输入密码）
veil init photos.veil
请输入密码: ****
请再次输入密码: ****
```

### 2. 添加文件

自动识别是文件还是目录。

```bash
# 位置参数: veil add <容器> <输入(外部)> <输出(容器内)> <密码>
veil add photos.veil vacation.jpg 2024/vacation.jpg mypassword

# 不指定输出路径，使用文件名
veil add photos.veil vacation.jpg mypassword

# 选项参数: -i 输入, -o 输出, -p 密码
veil add photos.veil -i vacation.jpg -o 2024/vacation.jpg -p mypassword

# 混合使用
veil add photos.veil vacation.jpg -o 2024/vacation.jpg -p mypassword

# 添加整个目录
veil add photos.veil ~/Pictures/ backup/ mypassword

# 环境变量方式（脚本使用）
export VEIL_PASSWORD=mypassword
veil add photos.veil vacation.jpg
```

### 3. 查看内容

树状显示容器内的所有文件和目录。

```bash
# 位置参数: veil free <容器> <密码>
veil free photos.veil mypassword

# 选项参数
veil free photos.veil -p mypassword

# 输出示例：
容器内容:
├── 2024/
│   ├── vacation.jpg
│   └── family.jpg
└── archive/
    └── old-photos/
        └── photo.jpg

统计信息:
  文件数量: 4
  总大小: 5242880 字节 (5.00 MB)
```

### 4. 导出文件

```bash
# 位置参数: veil ex <容器> <输入(容器内)> <输出(外部)> <密码>
veil ex photos.veil 2024/vacation.jpg ./recovered.jpg mypassword

# 选项参数: -i 输入, -o 输出, -p 密码
veil ex photos.veil -i 2024/vacation.jpg -o ./recovered.jpg -p mypassword

# 导出整个目录
veil ex photos.veil 2024/ ./recovered-photos/ mypassword

# 导出全部内容
veil ex photos.veil -a ./all-files/ mypassword
```

### 5. 删除文件

```bash
# 位置参数: veil rm <容器> <路径> <密码>
veil rm photos.veil 2024/vacation.jpg mypassword

# 选项参数
veil rm photos.veil -i 2024/vacation.jpg -p mypassword
```

### 6. 移动/重命名文件

```bash
# 位置参数: veil mv <容器> <源路径> <目标路径> <密码>
veil mv photos.veil old.jpg archive/old.jpg mypassword

# 选项参数: -f from, -t to, -p 密码
veil mv photos.veil -f old.jpg -t archive/old.jpg -p mypassword
```

### 7. 查看容器信息

```bash
# 位置参数: veil info <容器> <密码>
veil info photos.veil mypassword

# 选项参数
veil info photos.veil -p mypassword

# 输出示例：
容器信息:
  路径: photos.veil
  容器大小: 5242880 字节 (5.00 MB)

内容统计:
  文件数量: 10
  内容总大小: 4718592 字节 (4.50 MB)

文件类型分布:
  image/jpeg                     8
  image/png                      2
```

### 8. 修改容器密码

修改密码只需重新加密私钥，不需要重新加密数据（两级密钥的优势）。

```bash
# 位置参数: veil passwd <容器> <旧密码> <新密码>
veil passwd photos.veil oldpass newpass

# 选项参数: -p 旧密码, -n 新密码
veil passwd photos.veil -p oldpass -n newpass

# 混合使用
veil passwd photos.veil oldpass -n newpass

# 交互式（最安全）
veil passwd photos.veil
请输入当前密码: ****
请设置新密码: ****
请再次输入密码: ****
```

## 密码管理

### 三种密码输入方式

1. **交互式输入（推荐）** - 最安全
   ```bash
   veil init photos.veil
   请输入密码: [输入不回显]
   ```

2. **环境变量** - 适合脚本
   ```bash
   export VEIL_PASSWORD=mypassword
   veil add photos.veil file.jpg
   ```

3. **命令行参数** - 快速测试（不推荐生产环境）
   ```bash
   veil add photos.veil file.jpg mypassword
   # 注意：密码会在终端历史和进程列表中可见
   ```

### 密码生存周期

- **交互式输入**：仅在内存中短暂存在，使用后销毁
- **环境变量**：
  - 临时设置（单命令）：`VEIL_PASSWORD=pass veil add ...` - 命令结束后销毁
  - export：持续到终端关闭或 `unset VEIL_PASSWORD`
- **命令行参数**：明文可见，不推荐

## 密钥管理

### 两级密钥设计

```
用户密码
  ↓ (scrypt)
加密私钥 (存储在 Header)
  ↓
容器私钥 (内存中)
  ↓
加密/解密文件内容
```

### 性能特性

**单命令模式**：
- 每个命令都是独立进程
- 每次都需要解密私钥（~150ms）
- 适合偶尔使用

**Shell 批处理模式**：
- 一次解密私钥（~150ms）
- 后续命令复用私钥（~10ms）
- 性能提升 2-6 倍
- 适合批量操作

**修改密码**：
- 只重新加密私钥
- 不重新加密数据
- 速度快（~100ms）

## 线程安全性

- ❌ **不支持**多线程并发操作同一个容器
- ✅ **支持**多线程使用不同的容器
- ✅ **CLI 单线程**设计足够满足需求
- 💡 **GUI 应用**可在应用层使用 `Arc<RwLock<Container>>` 包装

## 示例场景

### 场景 1：备份私人照片

```bash
# 创建容器
veil init my-photos.veil

# 批量添加照片（使用 shell 模式）
veil shell my-photos.veil
veil> add ~/Pictures/2024-01-01.jpg 2024/jan/01.jpg
veil> add ~/Pictures/2024-01-02.jpg 2024/jan/02.jpg
veil> add ~/Pictures/2024-01-03.jpg 2024/jan/03.jpg
veil> free
veil> exit

# 查看容器信息
veil info my-photos.veil
```

### 场景 2：加密敏感文档

```bash
# 创建容器
veil init documents.veil

# 添加文档
veil add documents.veil contract.pdf legal/contract.pdf
veil add documents.veil tax-return.pdf finance/2024/tax.pdf

# 查看内容
veil free documents.veil

# 需要时导出
veil ex documents.veil legal/contract.pdf ./contract.pdf
```

### 场景 3：定期备份脚本

```bash
#!/bin/bash

# 使用环境变量
export VEIL_PASSWORD="your-secure-password"

# 创建容器（如果不存在）
if [ ! -f backup.veil ]; then
    veil init backup.veil
fi

# 使用 shell 模式批量备份
veil shell backup.veil <<EOF
add ~/Documents/ documents/
add ~/Projects/ projects/
info
exit
EOF

echo "备份完成"
```

### 场景 4：文件整理

```bash
# 进入 shell 模式
veil shell archive.veil

# 整理文件
veil> mv temp.txt archive/2024/temp.txt
veil> mv old-file.txt archive/2023/old-file.txt
veil> rm duplicates/file1.txt
veil> free

# 退出
veil> exit
```

## 技术细节

### 加密算法
- **密钥派生**：scrypt（N=32768, r=8, p=1）
- **非对称加密**：X25519（密钥交换）
- **对称加密**：ChaCha20-Poly1305（数据加密）
- **哈希**：BLAKE3（完整性校验）

### 文件格式
```
+------------------+
| Header           |
|  - Magic         |
|  - Version       |
|  - Encrypted Key |
+------------------+
| Blob 1           |
+------------------+
| Blob 2           |
+------------------+
| ...              |
+------------------+
| Index            |
+------------------+
| Footer           |
+------------------+
```

### 崩溃安全
- 所有写入追加到文件末尾
- Footer 作为提交点
- 未提交的数据在下次打开时自动截断

