//! # container —— .veil 容器的总装：create / add / list / read
//!
//! 本模块把 [`crate::keys`]（加解密）、[`crate::index`]（目录树）、
//! [`crate::format`]（头尾字节）三者组装起来，读写一个真正的单文件容器。
//!
//! ## `.veil` 单文件布局（对应 spec §3）
//!
//! ```text
//! +-----------------------------------------------------------+  ← 文件偏移 0
//! | Header                                                    |
//! |   magic               = "VEILPKG\0"  (8 bytes)            |
//! |   version             = u16                               |
//! |   flags               = u16                               |
//! |   cip_pri_key_len     = u32                               |
//! |   cip_pri_key         = bytes                             |
//! |        └─ age(用户密码, 容器 x25519 私钥串)；scrypt 只此一次 |
//! +-----------------------------------------------------------+
//! | Blob 区（追加增长，昂贵的密文数据永不重写）                 |
//! |   blob_0 = age(pub_key, 文件0内容)  ← 各自完整 age STREAM   |
//! |   blob_1 = age(pub_key, 文件1内容)                         |
//! |   ...                                                     |
//! +-----------------------------------------------------------+
//! | Index = age(pub_key, 序列化后的目录树 Vec<FsNode>)         |
//! +-----------------------------------------------------------+
//! | Footer（定长 24 字节，位于文件末尾）                        |
//! |   index_offset = u64                                      |
//! |   index_len    = u64                                      |
//! |   footer_magic = "VEILEND\0"  (8 bytes)                   |
//! +-----------------------------------------------------------+  ← 文件末尾
//! ```
//!
//! ## 密钥模型：公钥加密、私钥解密（age x25519 信封，对应 spec §4）
//!
//! 容器有一对 x25519 密钥，职责严格分开：
//!
//! ```text
//!   用户密码 ──scrypt──► 派生密钥 ──加密──► [key_pair 的私钥] ──► cip_pri_key（存 Header）
//!                                            ▲
//!                                            │ 用密码解密取回
//!                                            ▼
//!   [pub_key 公钥 ] ──加密──► 每个 blob、Index      （创建/追加时用，无需密码）
//!   [key_pair 私钥] ──解密──► 每个 blob、Index      （读取时用，需先解密拿到）
//! ```
//!
//! - **公钥 `pub_key`（`age::x25519::Recipient`，由 `key_pair.to_public()` 得来）**：只用来**加密**。
//!   写 blob、写 Index 都用它 → 见 [`crate::keys::encrypt_bytes`]。
//!   公钥不是秘密，甚至可以在**不解密私钥**的情况下往容器里加文件。
//! - **非对称密钥 `key_pair`（`age::x25519::Identity`，含私钥）**：用其私钥**解密**。
//!   读任何 blob、读 Index 都用它 → 见 [`crate::keys::decrypt_bytes`]。它的私钥被用密码
//!   加密成 `cip_pri_key`，必须先用密码解密（[`crate::keys::decrypt_pri_key`]）才能拿到。
//! - **密码**：只出现在加密/解密 `cip_pri_key` 这一处，经 scrypt 派生，**每容器一生一次**。
//!   密码从不参与 blob/Index 的加解密，也从不存进文件。
//!
//! 一句话：**公钥负责“存进去”，私钥负责“取出来”，密码只负责“加密看管那把私钥”。**
//!
//! ## 读写要点
//! - **打开**：seek 到文件尾读定长 Footer → 得 Index 的偏移/长度 → 读并解密
//!   Index → 在内存里重建目录树。无需扫描整个文件。
//! - **追加文件**：在旧 Index 之前的位置续写新 blob，再把 Index + Footer 重写到
//!   末尾。**blob 数据永不重写**，只重写很小的 Index/Footer。
//! - **看单个文件**：从目录树查到 `blob_offset / blob_len`，只解密那一段 blob，
//!   完全不碰其他数据（随机访问，P2 的 SliceReader 负责）。
//! - **写入顺序（崩溃安全，P5 细化）**：先写 blob+fsync，再写 Index+Footer+fsync；
//!   崩溃时旧 Footer 仍指向旧 Index，末尾未完成的 blob 视为垃圾可回收。
//!

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use age::secrecy::SecretString;

use crate::error::Result;
use crate::format;
use crate::index::{FsNode, deserialize_index, serialize_index};
use crate::keys::{decrypt_bytes, decrypt_pri_key, encrypt_bytes, encrypt_pri_key};

/// 一个在内存中已解密、可读写的容器句柄。
pub struct Container {
    /// 容器文件路径
    path: PathBuf,
    /// 非对称密钥对（解密内容用；P5 会加 zeroize 清零）
    key_pair: age::x25519::Identity,
    /// 密文私钥：重写 Header 时复用，避免重跑昂贵的 scrypt
    cip_pri_key: Vec<u8>,
    /// 内存中的目录树（P1 用扁平列表）
    nodes: Vec<FsNode>,
}

impl Container {
    /// 新建一个**空**容器并写入磁盘。
    ///
    /// # 参数
    /// - `path`:       容器文件路径（如 "photos.veil"）
    /// - `passphrase`: 用户密码，用来加密新生成的私钥
    pub fn create(path: impl AsRef<Path>, passphrase: SecretString) -> Result<Container> {
        // 1) 生成一对全新的非对称密钥
        let key_pair = age::x25519::Identity::generate();

        // 2) 用密码把私钥加密成密文私钥（scrypt 一生一次就在这）
        let cip_pri_key = encrypt_pri_key(&key_pair, passphrase)?;

        // 3) 组装句柄：空目录树
        let container = Container {
            path: path.as_ref().to_path_buf(),
            key_pair,
            cip_pri_key,
            nodes: Vec::new(),
        };

        // 4) 落盘
        container.write_to_disk()?;
        Ok(container)
    }

    /// 把当前状态整体写到磁盘：Header + (blob 区) + Index + Footer。
    ///
    /// P1 简化：每次整体重写。P5 再优化成「blob 只追加、仅重写 Index/Footer」。
    fn write_to_disk(&self) -> Result<()> {
        // File::create：新建（若已存在则清空）。用 BufWriter 减少系统调用
        let file = File::create(&self.path)?;
        let mut writer = BufWriter::new(file);

        // (a) 写 Header，拿到它的字节数 = 后面内容的起始偏移
        let header_len = format::write_header(&mut writer, &self.cip_pri_key)?;

        // (b) blob 区：空容器还没有 blob，所以 Index 紧跟 Header 之后
        //     （有文件后，这里会先写各个 blob，index_offset 相应变大）
        let index_offset = header_len;

        // (c) Index：目录树 → 序列化 → 用公钥加密 → 写入
        let index_plain = serialize_index(&self.nodes)?;
        let index_cipher = encrypt_bytes(&self.key_pair.to_public(), &index_plain)?;
        writer.write_all(&index_cipher)?;
        let index_len = index_cipher.len() as u64;

        // (d) Footer：记下 Index 的位置
        format::write_footer(&mut writer, index_offset, index_len)?;

        // BufWriter 缓冲的内容必须 flush 才真正落盘
        writer.flush()?;
        Ok(())
    }

    /// 打开一个已存在的容器：用密码解密私钥，读出目录树。
    ///
    /// # 参数
    /// - `path`:       容器文件路径
    /// - `passphrase`: 用户密码（错误则解密失败）
    pub fn open(path: impl AsRef<Path>, passphrase: SecretString) -> Result<Container> {
        let path = path.as_ref().to_path_buf();
        let mut file = File::open(&path)?;

        // 1) 读 Header → 拿到密文私钥（顺便校验 magic/version）
        let cip_pri_key = format::read_header(&mut file)?;

        // 2) 用密码解密私钥 → 拿回 key_pair（scrypt 一生一次就在这）
        let key_pair = decrypt_pri_key(&cip_pri_key, passphrase)?;

        // 3) 跳到文件尾读 Footer → 得知 Index 在哪、多长
        let (index_offset, index_len) = format::read_footer(&mut file)?;

        // 4) 定位到 Index 起点，精确读出 index_len 字节密文
        file.seek(SeekFrom::Start(index_offset))?;
        let mut index_cipher = vec![0u8; index_len as usize];
        file.read_exact(&mut index_cipher)?;

        // 5) 解密 + 反序列化 → 内存目录树
        let index_plain = decrypt_bytes(&key_pair, &index_cipher)?;
        let nodes = deserialize_index(&index_plain)?;

        Ok(Container { path, key_pair, cip_pri_key, nodes })
    }

    /// 当前目录树（只读借用）
    pub fn nodes(&self) -> &[FsNode] {
        &self.nodes
    }
    
}
