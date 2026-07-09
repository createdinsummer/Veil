# Veil Core

Veil的核心加密库 —— 提供单文件容器的加密、索引和读写功能。

## 概述

`veil-core` 是 Veil 项目的核心库，实现了基于 age 加密的单文件容器格式。它被 CLI 和 GUI 共同使用，提供了完整的容器管理 API。

## 特性

- 🔒 **安全加密**：基于 age（X25519 + ChaCha20-Poly1305）
- 🔑 **两级密钥**：密码加密私钥，私钥加密内容
- 📦 **单文件容器**：所有数据存储在 `.veil` 文件中
- 🌲 **嵌套目录树**：支持完整的目录结构
- 💾 **流式处理**：大文件边读边加密，内存占用恒定（~64KB）
- 🔐 **密钥派生**：使用 scrypt 安全派生密钥
- ✅ **完整性校验**：BLAKE3 哈希验证
- 🎯 **MIME 识别**：自动识别文件类型
- 💪 **崩溃安全**：追加写入 + Footer 提交点

## 架构

```
veil-core
├── container.rs   // 容器总装：create/open/add/extract
├── keys.rs        // 密钥管理：加密/解密/派生
├── index.rs       // 嵌套目录树索引
├── format.rs      // 文件格式：Header/Footer
├── mime.rs        // MIME 类型识别
├── slice_reader.rs // 切片读取器
└── error.rs       // 错误类型定义
```

## 文件格式

```
+---------------------------+  ← 文件偏移 0
| Header                    |
|   magic = "VEILPKG\0"     |
|   version = u16           |
|   flags = u16             |
|   cip_pri_key_len = u32   |
|   cip_pri_key = bytes     |  ← age(用户密码, 容器私钥)
+---------------------------+
| Blob 1 (加密数据)          |
+---------------------------+
| Blob 2 (加密数据)          |
+---------------------------+
| ...                       |
+---------------------------+
| Index (加密的目录树)       |
+---------------------------+
| Footer                    |
|   magic = "VEILFOOT"      |
|   index_offset = u64      |
|   index_len = u64         |
+---------------------------+  ← 文件末尾
```


## 密钥管理

### 两级密钥设计

```
用户密码 (password)
    ↓ scrypt(N=32768, r=8, p=1)
密钥派生
    ↓ age encrypt
加密私钥 (cip_pri_key) ← 存储在 Header
    ↓ age decrypt
容器私钥 (key_pair) ← 保存在内存
    ↓
加密/解密文件内容 (blobs)
```

### 优势

- ✅ **修改密码快**：只需重新加密私钥
- ✅ **安全性高**：密码通过 scrypt 派生，抗暴力破解
- ✅ **性能好**：私钥在内存中复用，避免重复解密

## 加密算法

| 用途 | 算法 | 说明 |
|------|------|------|
| 密钥派生 | scrypt | N=32768, r=8, p=1 |
| 非对称加密 | X25519 | 密钥交换 |
| 对称加密 | ChaCha20-Poly1305 | 流密码数据加密（通过 age） |
| 哈希 | BLAKE3 | 完整性校验 |
| MIME 识别 | infer | 文件类型识别 |

### 流式处理

Veil 使用 **ChaCha20-Poly1305 流密码**进行大文件加密：

**核心 API：**
```rust
// 流式添加文件（大文件推荐）
container.add_file_streaming(path, File::open("large.mp4")?)?;

// 流式读取文件
let mut reader = container.open_file_reader(path)?;
std::io::copy(&mut reader, &mut output)?;

// 内存添加（小文件）
container.add_file(path, &data)?;
```

**内存优势：**
- 使用固定 64KB 缓冲区边读边加密
- 4GB 视频只需 64KB 内存（传统方式需要 4GB）
- 内存占用恒定，不随文件大小增长

**流密码特性：**
- 密文是连续字节流，无块边界
- 加密和解密的缓冲区大小可以完全不同
- 支持任意位置 Seek 随机访问
- 无需对齐或填充

**技术原理：**
```
明文逐字节与密钥流 XOR 生成密文：
  A B C D E F  →  A⊕K1 B⊕K2 C⊕K3 D⊕K4 E⊕K5 F⊕K6

解密时按顺序读取并与相同密钥流 XOR，
无论一次读 1 字节还是 1MB 都能正确还原。
```

这与分块密码（如 AES-CBC）不同，后者要求数据必须按固定块对齐。

## 崩溃安全

### 设计原则

1. **追加写入**：所有数据追加到文件末尾，不覆盖已提交数据
2. **Footer 作为提交点**：Footer 最后写入并 fsync，之前的数据才算提交
3. **自动恢复**：下次打开时，未提交的数据自动截断

### 写入流程

```
1. 追加 Blob → fsync
2. 追加新 Index → fsync
3. 写入 Footer → fsync  ← 提交点
```

### 崩溃恢复

```
打开容器 → 读取 Footer → 获取 Index 位置 → 截断未提交数据
```

## 线程安全性

- ❌ **不支持**多线程并发操作同一个 `Container`
- ✅ **支持**多线程操作不同的容器
- 💡 **建议**：在应用层使用 `Arc<RwLock<Container>>` 包装

## 性能特性

| 操作 | 时间复杂度 | 说明 |
|------|-----------|------|
| 创建容器 | O(1) | 写入 Header + 空 Index |
| 打开容器 | O(n) | n = 文件数，需解密 Index |
| 添加文件 | O(n) | 需重写整个 Index |
| 读取文件 | O(1) | 直接定位 blob |
| 删除文件 | O(n) | 需重写 Index（blob 成死空间） |
| 修改密码 | O(1) | 只重写 Header |


## 依赖

- `age` - 加密库（X25519 + ChaCha20-Poly1305）
- `blake3` - 哈希算法
- `scrypt` - 密钥派生
- `serde` - 序列化（Index）
- `infer` - MIME 类型识别
