# Veil - 面纱

一个简单、安全、高效的文件加密容器工具。

## 简介

Veil 是一个基于现代加密算法的文件加密容器工具，将任意文件和目录加密存储在单个 `.veil` 容器文件中。支持完整的目录结构，提供命令行工具便于集成到工作流程中。

## 特性

- 🔒 **现代加密算法**：基于 age 加密（X25519 + ChaCha20-Poly1305）
- 🔑 **两级密钥系统**：密码加密私钥，私钥加密内容，修改密码无需重新加密数据
- 📦 **单文件容器**：所有数据存储在一个 `.veil` 文件中，便于传输和备份
- 🌲 **目录树结构**：支持嵌套目录，保持文件组织
- ⚡ **批处理模式**：Shell 模式性能提升 2-6 倍
- 📊 **进度显示**：实时显示加密/解密进度
- 💪 **崩溃安全**：追加写入 + Footer 提交点，确保数据完整性
- ✅ **完整测试**：22 个集成测试，覆盖所有核心功能

## 快速开始

### 安装

```bash
# 从源码编译
cargo build --release --package veil-cli

# 或使用打包脚本
./build.sh

# 安装到系统
sudo cp release/bin/veil /usr/local/bin/
```

### 基本使用

```bash
# 创建加密容器
veil init vault.veil

# 添加文件
veil add vault.veil document.pdf

# 添加目录
veil add vault.veil ~/Photos backup/

# 查看内容
veil free vault.veil

# 导出文件
veil ex vault.veil document.pdf ./output.pdf

# 批处理模式（高性能）
veil shell vault.veil
veil> add file1.txt
veil> add file2.txt
veil> free
veil> exit
```

## 项目结构

```
Veil/
├── crates/
│   ├── veil-core/      # 核心加密库
│   └── veil-cli/       # 命令行工具
├── build.sh            # 打包脚本
├── INSTALL.md          # 安装指南
└── README.md           # 本文件
```

## 核心组件

### veil-core

核心加密库，提供容器管理 API。

**主要功能**：
- 容器创建和打开
- 文件加密和解密
- 目录树管理
- 密钥派生和管理
- 完整性校验

**技术栈**：
- `age` - 加密库
- `scrypt` - 密钥派生
- `blake3` - 哈希校验
- `serde` - 序列化

### veil-cli

命令行工具，提供友好的用户界面。

**9 个核心命令**：
- `init` - 创建容器
- `add` - 添加文件/目录
- `rm` - 删除文件
- `mv` - 移动/重命名
- `free` - 树状显示内容
- `ex` - 导出文件/目录
- `info` - 显示统计信息
- `passwd` - 修改密码
- `shell` - 批处理模式

## 加密技术

### 两级密钥系统

```
用户密码
    ↓ scrypt(N=32768, r=8, p=1)
密钥派生
    ↓ age encrypt
加密私钥 (存储在 Header)
    ↓ age decrypt
容器私钥 (内存中)
    ↓
加密/解密文件内容
```

**优势**：
- 修改密码只需重新加密私钥（~100ms）
- 无需重新加密所有数据（节省时间）
- scrypt 抗暴力破解
- 私钥在内存中复用，性能高效

### 加密算法

| 用途 | 算法 | 参数 |
|------|------|------|
| 密钥派生 | scrypt | N=32768, r=8, p=1 |
| 密钥交换 | X25519 | Curve25519 |
| 对称加密 | ChaCha20-Poly1305 | AEAD |
| 哈希校验 | BLAKE3 | 256-bit |

### 文件格式

```
+------------------+
| Header           |  magic + version + 加密私钥
+------------------+
| Blob 1           |  加密的文件内容
+------------------+
| Blob 2           |  加密的文件内容
+------------------+
| ...              |
+------------------+
| Index            |  加密的目录树
+------------------+
| Footer           |  magic + 索引位置（提交点）
+------------------+
```

## 性能

在 MacBook Pro (M1) 上的测试结果：

| 操作 | 单命令模式 | Shell 模式 | 提升 |
|------|-----------|-----------|------|
| 打开容器 | ~150ms | ~150ms | - |
| 添加小文件 | ~160ms | ~10ms | **16x** |
| 添加 10MB 文件 | ~250ms | ~100ms | **2.5x** |
| 读取文件 | ~160ms | ~10ms | **16x** |
| 删除文件 | ~160ms | ~10ms | **16x** |

**批处理模式优势**：
- 私钥只解密一次
- 后续操作复用私钥
- 适合批量文件操作
- 性能提升 2-6 倍

## 使用场景

### 个人隐私保护

```bash
# 加密私人文档
veil init private.veil
veil add private.veil ~/Documents/tax-return.pdf
veil add private.veil ~/Documents/passport.pdf
```

### 敏感文件传输

```bash
# 将文件打包到容器中
veil init transfer.veil
veil add transfer.veil confidential.doc

# 通过网络传输 transfer.veil
# 接收方解密
veil ex transfer.veil confidential.doc ./output.doc
```

### 定期备份

```bash
#!/bin/bash
# 自动备份脚本
export VEIL_PASSWORD="your-password"
DATE=$(date +%Y%m%d)

veil init backup-$DATE.veil
veil shell backup-$DATE.veil <<EOF
add ~/Documents documents/
add ~/Photos photos/
exit
EOF
```

### 批量加密

```bash
# 使用 shell 模式批量加密
veil shell archive.veil
veil> add photo1.jpg
veil> add photo2.jpg
veil> add photo3.jpg
veil> add photo4.jpg
veil> add photo5.jpg
veil> exit
```

## 安全性

### 密码管理

**三种输入方式**：
1. **交互式**（推荐）：密码不回显，最安全
2. **环境变量**：`VEIL_PASSWORD=xxx veil ...`，适合脚本
3. **命令行参数**：明文可见，不推荐生产环境

### 数据安全

- ✅ **加密算法**：现代加密算法 ChaCha20-Poly1305 AEAD
- ✅ **密钥派生**：scrypt 抗暴力破解
- ✅ **完整性校验**：BLAKE3 哈希验证数据完整性
- ✅ **崩溃安全**：追加写入 + Footer 提交点
- ✅ **零泄漏**：私钥仅在内存中，进程结束自动销毁

### 限制

- ⚠️ **不支持多线程**：不支持多线程并发操作同一个容器
- ⚠️ **内存占用**：文件完全读入内存（超大文件需注意）
- ⚠️ **密码强度**：安全性完全依赖密码强度

## 开发

### 环境要求

- Rust 1.70+
- Cargo

### 编译

```bash
# 开发版本
cargo build --package veil-cli

# 发布版本
cargo build --release --package veil-cli

# 运行测试
cargo test --all
```

### 测试

```bash
# 运行所有测试
cargo test --all --quiet

# 测试覆盖
# - veil-core: 25 个单元测试
# - veil-cli: 22 个集成测试
```

## 路线图

- [ ] 流式读写支持（处理超大文件）
- [ ] 容器压缩（减小文件大小）
- [ ] 增量更新（避免重写整个索引）
- [ ] 死空间回收（compaction）
- [ ] 多容器合并
- [ ] 容器分片
- [ ] 文件去重
- [ ] 版本控制

## 常见问题

**Q: 忘记密码怎么办？**  
A: 无法恢复。Veil 使用强加密，没有后门。请妥善保管密码。

**Q: 容器文件可以在不同系统间传输吗？**  
A: 可以。`.veil` 文件格式跨平台，可以在 Windows、macOS、Linux 之间传输。

**Q: 为什么添加大文件很慢？**  
A: 当前版本文件完全读入内存。流式读写支持在开发路线图中。

**Q: 可以加密整个磁盘吗？**  
A: 不适合。Veil 设计用于文件级加密。磁盘加密请使用 BitLocker、FileVault 等。

**Q: 修改密码需要多久？**  
A: 约 100ms。只需重新加密私钥，无需重新加密数据。

## 许可证

Apache License 2.0

详见 [LICENSE](LICENSE) 文件。

## 贡献

欢迎提交 Issue 和 Pull Request。

## 致谢

基于以下优秀的开源项目：
- [age](https://github.com/str4d/rage) - 现代加密工具
- [scrypt](https://en.wikipedia.org/wiki/Scrypt) - 密钥派生算法
- [BLAKE3](https://github.com/BLAKE3-team/BLAKE3) - 快速哈希算法
