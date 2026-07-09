# Veil - 面纱

一个简单、安全、高效的文件加密容器工具。

## 简介

Veil 是一个基于现代加密算法的文件加密容器工具，将任意文件和目录加密存储在单个 `.veil` 容器文件中。支持完整的目录结构，提供命令行工具便于集成到工作流程中。

## 特性

- 🔒 **现代加密算法**：基于 age 加密（X25519 + ChaCha20-Poly1305）
- 🔑 **两级密钥系统**：密码加密私钥，私钥加密内容，修改密码无需重新加密数据
- 📦 **单文件容器**：所有数据存储在一个 `.veil` 文件中，便于传输和备份
- 🌲 **目录树结构**：支持嵌套目录，保持文件组织
- 💾 **流式处理**：大文件边读边加密，内存占用恒定（~64KB）
- ⚡ **批处理模式**：Shell 模式性能提升 2-6 倍
- 📊 **进度显示**：实时显示加密/解密进度
- 💪 **崩溃安全**：追加写入 + Footer 提交点，确保数据完整性
- 🌐 **完全跨平台**：支持 Windows、macOS、Linux，容器文件可跨平台传输
- ✅ **完整测试**：47 个测试全部通过

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

# 查看内容
veil free vault.veil

# 导出文件
veil ex vault.veil document.pdf ./output.pdf

# 批处理模式（高性能）
veil shell vault.veil
```

完整使用指南请查看 [CLI 文档](crates/veil-cli/README.md)。

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

- **[veil-core](crates/veil-core/README.md)** - 核心加密库，提供容器管理 API
- **[veil-cli](crates/veil-cli/README.md)** - 命令行工具，9 个命令 + 批处理模式

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

**优势**：修改密码只需重新加密私钥（~100ms），无需重新加密所有数据。

### 加密算法

| 用途 | 算法 | 说明 |
|------|------|------|
| 密钥派生 | scrypt | 抗暴力破解 |
| 密钥交换 | X25519 | Curve25519 |
| 对称加密 | ChaCha20-Poly1305 | AEAD 流密码 |
| 哈希校验 | BLAKE3 | 256-bit |

### 流密码特性

Veil 使用 **ChaCha20-Poly1305 流密码**进行文件加密，具有以下优势：

**内存高效：**
- 加密时使用固定 64KB 缓冲区边读边加密
- 4GB 视频只需 64KB 内存（传统方式需要 4GB）
- 内存占用恒定，不随文件大小增长

**灵活读写：**
- 流密码生成连续的密文字节流，无块边界
- 加密和解密的缓冲区大小可以完全不同
- 支持任意位置 Seek 随机访问

**技术原理：**
```
明文逐字节与密钥流 XOR：
  明文: A B C D E F
  密钥流: K1 K2 K3 K4 K5 K6
  密文: A⊕K1 B⊕K2 C⊕K3 ... (连续字节流)

解密时只需按顺序读取密文并与相同密钥流 XOR，
无论一次读取 1 字节还是 1MB 都能正确还原明文。
```

这与分块密码（如 AES-CBC）不同，后者要求数据必须按固定块（16 字节）对齐，加密和解密的块大小必须一致。

**性能提升：**

| 文件大小 | 传统方式内存占用 | 流式处理内存占用 | 提升倍数 |
|---------|----------------|----------------|----------|
| 10 MB   | ~10 MB         | ~64 KB         | 156x     |
| 100 MB  | ~100 MB        | ~64 KB         | 1562x    |
| 1 GB    | ~1 GB          | ~64 KB         | 16000x   |
| 4 GB    | 崩溃/交换        | ~64 KB         | 可用     |

**向后兼容：**
- 保留 `add_file(&[u8])` 接口供小文件使用
- 两种加密方式生成的密文格式完全相同
- 流式加密和整体加密的文件可以混存在同一容器中

技术细节请查看 [Core 文档](crates/veil-core/README.md)。

## 使用场景

### 个人隐私保护

```bash
veil init private.veil
veil add private.veil ~/Documents/confidential.pdf
```

### 敏感文件传输

```bash
# 加密打包
veil init transfer.veil
veil add transfer.veil secret.doc

# 接收方解密
veil ex transfer.veil secret.doc ./output.doc
```

### 批量加密

```bash
veil shell archive.veil
veil> add photo1.jpg
veil> add photo2.jpg
veil> add photo3.jpg
veil> exit
```

## 安全性

- ✅ **加密算法**：军事级 ChaCha20-Poly1305 AEAD
- ✅ **密钥派生**：scrypt 抗暴力破解
- ✅ **完整性校验**：BLAKE3 哈希验证
- ✅ **崩溃安全**：追加写入 + Footer 提交点
- ✅ **零泄漏**：私钥仅在内存中，进程结束自动销毁

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

### 测试覆盖

- veil-core: 25 个单元测试
- veil-cli: 22 个集成测试
- 总计: 47 个测试

## 常见问题

**Q: 忘记密码怎么办？**  
A: 无法恢复。Veil 使用强加密，没有后门。请妥善保管密码。

**Q: 容器文件可以在不同系统间传输吗？**  
A: 可以。`.veil` 文件格式跨平台。

**Q: 大文件会占用很多内存吗？**  
A: 不会。Veil 使用流式处理，加密 4GB 视频只需 64KB 内存。

**Q: 修改密码需要多久？**  
A: 约 100ms。只需重新加密私钥，无需重新加密数据。

## 路线图

- [x] 流式读写支持（已完成 - 处理超大文件）
- [ ] 容器压缩
- [ ] 增量更新
- [ ] 死空间回收
- [ ] 多容器合并
- [ ] 文件去重

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
