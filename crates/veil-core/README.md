# Veil Core

Veil 加密媒体保险箱的核心加密库 —— 提供单文件容器的加密、索引和读写功能。

## 概述

`veil-core` 是 Veil 项目的核心库，实现了基于 age 加密的单文件容器格式。它被 CLI 和 GUI 共同使用，提供了完整的容器管理 API。

## 特性

- 🔒 **安全加密**：基于 age（X25519 + ChaCha20-Poly1305）
- 🔑 **两级密钥**：密码加密私钥，私钥加密内容
- 📦 **单文件容器**：所有数据存储在 `.veil` 文件中
- 🌲 **嵌套目录树**：支持完整的目录结构
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

## 使用示例

### 创建容器

```rust
use veil_core::container::Container;

// 创建新容器
let container = Container::create("photos.veil", "mypassword")?;
```

### 打开容器

```rust
// 打开现有容器
let mut container = Container::open("photos.veil", "mypassword")?;
```

### 添加文件

```rust
// 添加单个文件
let content = std::fs::read("photo.jpg")?;
container.add_file("2024/photo.jpg", &content)?;

// 添加整个目录
container.add_dir("./photos", "backup/")?;
```

### 读取文件

```rust
// 读取文件内容
let content = container.read_file("2024/photo.jpg")?;

// 获取文件元数据
if let Some(meta) = container.get_file("2024/photo.jpg") {
    println!("大小: {} 字节", meta.size);
    println!("类型: {:?}", meta.mime);
}
```

### 导出文件

```rust
// 导出单个文件
container.extract_file("2024/photo.jpg", "./photo.jpg")?;

// 导出整个目录
container.extract_dir("backup/", "./restored/")?;

// 导出全部
container.extract_all("./all-files/")?;
```

### 删除和重命名

```rust
// 删除文件
container.remove_file("2024/photo.jpg")?;

// 重命名/移动文件
container.rename_file("old.jpg", "archive/old.jpg")?;
```

### 修改密码

```rust
// 修改容器密码（只重新加密私钥，不重新加密数据）
container.change_password("newpassword")?;
```

### 查看目录树

```rust
// 获取树状视图
let tree = container.tree_view();
println!("{}", tree);

// 列出所有文件
let files = veil_core::index::list_files(container.root());
for (path, meta) in files {
    println!("{}: {} 字节", path, meta.size);
}
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
| 对称加密 | ChaCha20-Poly1305 | 数据加密（通过 age） |
| 哈希 | BLAKE3 | 完整性校验 |
| MIME 识别 | infer | 文件类型识别 |

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

## API 文档

### Container

主要的容器管理结构体。

```rust
pub struct Container {
    path: PathBuf,           // 容器文件路径
    key_pair: Identity,      // 容器密钥对
    root: Tree,              // 目录树
}

impl Container {
    // 创建新容器
    pub fn create(path: impl AsRef<Path>, passphrase: impl Into<SecretString>) -> Result<Container>;
    
    // 打开现有容器
    pub fn open(path: impl AsRef<Path>, passphrase: impl Into<SecretString>) -> Result<Container>;
    
    // 添加文件
    pub fn add_file(&mut self, virtual_path: &str, plaintext: &[u8]) -> Result<()>;
    
    // 添加目录
    pub fn add_dir(&mut self, source_dir: impl AsRef<Path>, dest_prefix: &str) -> Result<()>;
    
    // 读取文件
    pub fn read_file(&self, virtual_path: &str) -> Result<Vec<u8>>;
    
    // 导出文件
    pub fn extract_file(&self, virtual_path: &str, dest: impl AsRef<Path>) -> Result<()>;
    
    // 导出目录
    pub fn extract_dir(&self, virtual_prefix: &str, dest: impl AsRef<Path>) -> Result<()>;
    
    // 导出全部
    pub fn extract_all(&self, dest: impl AsRef<Path>) -> Result<()>;
    
    // 删除文件
    pub fn remove_file(&mut self, virtual_path: &str) -> Result<()>;
    
    // 重命名文件
    pub fn rename_file(&mut self, from: &str, to: &str) -> Result<()>;
    
    // 修改密码
    pub fn change_password(&self, new_passphrase: impl Into<SecretString>) -> Result<()>;
    
    // 获取文件元数据
    pub fn get_file(&self, virtual_path: &str) -> Option<&FileMeta>;
    
    // 获取目录树根节点
    pub fn root(&self) -> &Tree;
    
    // 生成树状视图
    pub fn tree_view(&self) -> String;
}
```

### FileMeta

文件元数据。

```rust
pub struct FileMeta {
    pub size: u64,              // 文件大小（明文）
    pub blob_offset: u64,       // Blob 在文件中的偏移
    pub blob_len: u64,          // Blob 长度（密文）
    pub content_hash: [u8; 32], // BLAKE3 哈希
    pub mime: Option<String>,   // MIME 类型
    pub mtime: Option<u64>,     // 修改时间（保留字段）
}
```

### Tree

嵌套目录树。

```rust
pub struct Tree {
    // 内部结构，通过 Container API 访问
}
```

## 错误处理

```rust
use veil_core::error::{Result, VeilError};

match container.read_file("test.txt") {
    Ok(content) => println!("读取成功"),
    Err(VeilError::FileNotFound(path)) => println!("文件不存在: {}", path),
    Err(VeilError::DecryptionFailed) => println!("密码错误"),
    Err(e) => println!("其他错误: {}", e),
}
```

## 测试

```bash
# 运行所有测试
cargo test --package veil-core

# 运行特定测试
cargo test --package veil-core test_name
```

## 依赖

- `age` - 加密库（X25519 + ChaCha20-Poly1305）
- `blake3` - 哈希算法
- `scrypt` - 密钥派生
- `serde` - 序列化（Index）
- `infer` - MIME 类型识别

## 未来计划

- [ ] 流式读写支持（处理超大文件）
- [ ] 容器压缩
- [ ] 增量更新（避免重写整个 Index）
- [ ] 死空间回收（compaction）
- [ ] 多容器合并
- [ ] 容器分片

## 许可证

待定
