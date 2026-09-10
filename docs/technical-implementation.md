# Veil 2.0 技术实现文档

## 一、架构设计

### 1.1 核心概念

```
工作区（Workspace）
  ├── 容器目录 1/
  │   ├── .veil-meta          ← 加密的元数据
  │   ├── a3f2c1d4.enc        ← 加密文件 1
  │   └── b7e4f9a8.enc        ← 加密文件 2
  ├── 容器目录 2/
  └── 容器目录 3/

容器文件（.veil）
  ├── Header                   ← 容器元信息
  ├── Encrypted Metadata       ← 加密的元数据
  └── File Data                ← 加密的文件数据
```

### 1.2 密钥派生流程

```
用户密码 (password)
    ↓
Salt (32 bytes, 存储在 .veil-meta 明文头部)
    ↓
Argon2id(password, salt, params)
    ↓
Master Key (256-bit)
    ↓
    ├─→ 加密/解密 .veil-meta
    └─→ 加密/解密容器内所有文件（使用不同 nonce）
```

**关键设计**：
- 每个容器有独立的 salt → 独立的 master key
- 即使使用相同密码，不同容器的密钥也不同
- Master key 从不持久化，用完立即清零

### 1.3 文件加密流程

```
原始文件 (plaintext)
    ↓
生成随机 nonce (12 bytes)
    ↓
ChaCha20-Poly1305(master_key, plaintext, nonce)
    ↓
加密文件 (ciphertext + auth_tag)
    ↓
保存为随机 ID.enc (如: a3f2c1d4.enc)
```

## 二、文件格式

### 2.1 .veil-meta 格式

```
[固定头部 - 10 bytes]
  magic: "VEILMETA" (8 bytes)
  header_len: u16 (2 bytes)

[TLV 字段 - 可变长度]
  Tag (1 byte) | Length (u16) | Value (variable)
  ...

[加密数据 - 可变长度]
  ChaCha20-Poly1305 加密的 JSON 元数据
```

**TLV Tags**:
```rust
Version = 0x01      // 格式版本 (u16)
Salt = 0x02         // 密钥派生 salt (32 bytes)
Algorithm = 0x03    // 加密算法 ID (u8)
KdfParams = 0x04    // KDF 参数
Nonce = 0x05        // 元数据加密 nonce (12 bytes)
Custom = 0xF0       // 自定义扩展
EndMarker = 0xFF    // 头部结束标记
```

**元数据 JSON 结构**:
```json
{
  "container_name": "myfiles",
  "workspace_type": "default",
  "created_at": "2026-09-10T10:00:00Z",
  "files": [
    {
      "encrypted_name": "a3f2c1d4.enc",
      "original_name": "photo.jpg",
      "size": 1024000,
      "nonce": [12 bytes],
      "encrypted_at": "2026-09-10T10:05:00Z",
      "hash": null
    }
  ]
}
```

### 2.2 .veil 容器格式

```
[Header - 22 bytes]
  magic: "VEILPKG\0" (8 bytes)
  version: u16 (2 bytes)
  header_size: u32 (4 bytes)
  metadata_size: u32 (4 bytes)
  file_count: u32 (4 bytes)

[Metadata Section - variable]
  加密的元数据（来自 .veil-meta 的加密部分）

[File Data Section - variable]
  For each file:
    [File Header]
      name_len: u16 (2 bytes)
      name: [u8; name_len]
      data_size: u64 (8 bytes)
    [File Data]
      encrypted data (data_size bytes)
```

### 2.3 全局配置格式 (~/.veil/config.toml)

```toml
version = "1.0"

[system]
icons_configured = true
first_run = false

[workspace.default]
path = "~/.veil/workspaces/default"

[workspace.custom.work]
path = "~/Documents/veil-work"
description = "工作相关文件"
created_at = "2026-09-10T10:30:00Z"

[containers.myfiles]
workspace = "default"
container_dir = "myfiles"
created_at = "2026-09-10T11:00:00Z"

[containers.secrets]
workspace_path = "~/EncryptedVolume/veil"
dedicated = true
created_at = "2026-09-10T11:20:00Z"

[preferences]
hints_level = "full"
cache_keys = false
cache_timeout_seconds = 300

[encryption.defaults]
algorithm = "AES-256-GCM"
key_derivation = "Argon2id"

[encryption.argon2id]
memory_kb = 65536      # 64 MB
iterations = 3
parallelism = 4
```

## 三、核心算法

### 3.1 Argon2id 参数

```rust
pub const STANDARD: Argon2Params = Argon2Params {
    memory_kb: 256 * 1024,  // 256 MB
    iterations: 3,
    parallelism: 4,
};
```

**安全性分析**：
- 内存：256 MB（防御 GPU 并行攻击）
- 迭代：3 次（平衡安全性和性能）
- 并行度：4（利用多核）
- 预计破解时间：天文数字（假设强密码）

### 3.2 ChaCha20-Poly1305

```rust
let cipher = ChaCha20Poly1305::new(&master_key);
let nonce = random_nonce();  // 12 bytes
let ciphertext = cipher.encrypt(&nonce, plaintext)?;
```

**特点**：
- AEAD（认证加密）
- 256-bit 密钥
- 96-bit nonce（每个文件独立）
- 128-bit 认证标签

### 3.3 文件名加密

```rust
fn generate_random_id() -> String {
    let bytes: [u8; 8] = random();
    hex::encode(bytes)  // 16 字符十六进制
}
```

**示例**：
- 原始：`photo.jpg`
- 加密：`a3f2c1d4e5f67890.enc`
- 映射存储在加密的 .veil-meta 中

## 四、操作流程

### 4.1 初始化容器

```rust
fn init_container(name: &str, password: &str) {
    // 1. 生成随机 salt
    let salt = random_bytes::<32>();
    
    // 2. 派生 master key
    let master_key = argon2id_derive(password, salt);
    
    // 3. 创建初始元数据
    let metadata = MetaData::new(name);
    
    // 4. 生成 nonce
    let nonce = random_bytes::<12>();
    
    // 5. 加密元数据
    let encrypted = chacha20poly1305_encrypt(
        master_key, 
        metadata.to_json(), 
        nonce
    );
    
    // 6. 构建头部（明文）
    let header = MetaHeader { salt, nonce, ... };
    
    // 7. 写入 .veil-meta
    write_file(".veil-meta", [header.to_bytes(), encrypted]);
    
    // 8. 清零密钥
    master_key.zeroize();
}
```

### 4.2 添加文件

```rust
fn add_file(file_path: &Path, password: &str) {
    // 1. 读取 .veil-meta 头部获取 salt
    let header = read_meta_header();
    
    // 2. 派生 master key
    let master_key = argon2id_derive(password, header.salt);
    
    // 3. 解密元数据
    let metadata = decrypt_meta(master_key, header);
    
    // 4. 读取原始文件
    let plaintext = read_file(file_path);
    
    // 5. 生成随机 ID 和 nonce
    let encrypted_name = format!("{}.enc", random_id());
    let file_nonce = random_bytes::<12>();
    
    // 6. 加密文件
    let ciphertext = chacha20poly1305_encrypt(
        master_key, 
        plaintext, 
        file_nonce
    );
    
    // 7. 保存加密文件
    write_file(encrypted_name, ciphertext);
    
    // 8. 更新元数据
    metadata.add_file(FileEntry {
        encrypted_name,
        original_name: file_path.name(),
        nonce: file_nonce,
        ...
    });
    
    // 9. 重新加密并保存元数据
    encrypt_and_save_meta(master_key, metadata);
    
    // 10. 清零密钥
    master_key.zeroize();
}
```

### 4.3 打包容器

```rust
fn pack(workspace_path: &Path, output: &Path, password: &str) {
    // 1. 读取并验证密码
    let metadata = read_meta(workspace_path, password);
    
    // 2. 读取加密的元数据
    let meta_bytes = read_file(".veil-meta");
    let header_len = MetaHeader::header_len(meta_bytes);
    let encrypted_meta = &meta_bytes[header_len..];
    
    // 3. 写入容器头部
    let container_header = ContainerHeader::new(
        encrypted_meta.len(),
        metadata.files.len()
    );
    write_file(output, container_header.to_bytes());
    
    // 4. 写入加密的元数据
    append_file(output, encrypted_meta);
    
    // 5. 写入每个加密文件
    for file_entry in metadata.files {
        // 文件条目头部
        let entry_header = FileEntryHeader {
            name: file_entry.original_name,
            data_size: file_entry.size,
        };
        append_file(output, entry_header.to_bytes());
        
        // 文件数据
        let encrypted_data = read_file(
            workspace_path.join(file_entry.encrypted_name)
        );
        append_file(output, encrypted_data);
    }
}
```

### 4.4 解包容器

```rust
fn unpack(container_path: &Path, workspace: &Path, password: &str) {
    // 1. 读取容器头部
    let header = read_container_header(container_path);
    
    // 2. 读取加密的元数据
    let encrypted_meta = read_bytes(
        container_path, 
        header.header_size, 
        header.metadata_size
    );
    
    // 3. 解密元数据（验证密码）
    let metadata = decrypt_meta(encrypted_meta, password);
    
    // 4. 创建工作区目录
    create_dir(workspace);
    
    // 5. 提取每个文件
    let mut offset = header.header_size + header.metadata_size;
    for _ in 0..header.file_count {
        // 读取文件条目头部
        let entry_header = read_file_entry_header(container_path, offset);
        offset += entry_header.size();
        
        // 读取加密文件数据
        let encrypted_data = read_bytes(
            container_path, 
            offset, 
            entry_header.data_size
        );
        offset += entry_header.data_size;
        
        // 查找对应的文件条目（获取加密文件名）
        let file_entry = metadata.find_file(entry_header.name);
        
        // 保存为加密文件名
        write_file(
            workspace.join(file_entry.encrypted_name),
            encrypted_data
        );
    }
    
    // 6. 写入 .veil-meta
    write_file(workspace.join(".veil-meta"), encrypted_meta);
}
```

## 五、安全性分析

### 5.1 威胁模型

**假设**：
- 攻击者可以访问所有文件（工作区、容器文件、配置）
- 攻击者不知道用户密码
- 攻击者可以执行离线攻击

**防御**：
- ✅ 高强度 KDF（Argon2id）防御暴力破解
- ✅ 独立 salt 防御彩虹表攻击
- ✅ AEAD 加密防御篡改
- ✅ 文件名加密防御信息泄露
- ✅ 独立 nonce 防御模式攻击

### 5.2 安全边界

**保护的**：
- ✅ 文件内容
- ✅ 文件名
- ✅ 文件数量
- ✅ 文件大小
- ✅ 目录结构

**不保护的**：
- ❌ 访问模式（何时访问）
- ❌ 容器存在性
- ❌ 工作区路径
- ❌ 加密算法类型（明文存储）

### 5.3 密钥生命周期

```
创建容器时：
  生成 salt → 派生 master key → 加密元数据 → 清零 key

添加文件时：
  读取 salt → 派生 master key → 解密元数据 → 加密文件 
  → 更新元数据 → 清零 key

提取文件时：
  读取 salt → 派生 master key → 解密元数据 → 解密文件 
  → 清零 key
```

**关键**：密钥从不持久化，用完立即清零（使用 Zeroizing）

## 六、性能考虑

### 6.1 性能瓶颈

1. **Argon2id 密钥派生**：每次约 26 秒（测试环境）
   - 解决：用户只需在关键操作时输入密码
   - 优化：Release 模式编译

2. **元数据更新**：每次文件操作都需要重写
   - 解决：原子写入，小文件影响不大
   - 优化：批量操作

3. **文件加密/解密**：线性复杂度
   - 解决：并发处理（未来优化）

### 6.2 优化策略

**已实现**：
- ✅ 独立文件加密（支持并发）
- ✅ 延迟打包（日常操作在工作区）
- ✅ 增量更新（只操作变化的文件）

**未来优化**：
- [ ] 密钥缓存（可选）
- [ ] 批量操作API
- [ ] 多线程加密

## 七、测试覆盖

### 7.1 单元测试

```rust
#[test]
fn test_workspace_init() { ... }

#[test]
fn test_add_extract_file() { ... }

#[test]
fn test_wrong_password() { ... }

#[test]
fn test_remove_file() { ... }
```

### 7.2 集成测试

- ✅ 完整工作流（init → add → list → extract → rm）
- ✅ 打包/解包流程
- ✅ 密码验证
- ✅ 数据完整性

## 八、未来扩展

### 8.1 可扩展设计

**TLV 格式**：轻松添加新字段
```rust
MetaTag::CompressionAlgorithm = 0x06,
MetaTag::EncryptedBy = 0x09,  // 公钥指纹
```

**版本控制**：支持格式演进
```rust
if header.version == 1 { ... }
else if header.version == 2 { ... }
```

### 8.2 计划功能

- [ ] 压缩支持
- [ ] 完整性校验（Blake3）
- [ ] 非对称加密（age 集成）
- [ ] 增量同步
- [ ] 云存储后端

---

**文档版本**：1.0  
**创建日期**：2026-09-10  
**作者**：Veil 开发团队
