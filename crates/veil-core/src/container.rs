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
