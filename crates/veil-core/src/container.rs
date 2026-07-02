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

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use age::secrecy::SecretString;

use crate::error::{Result, VeilError};
use crate::format;
use crate::index::{FsNode, Kind, TreeNode, build_tree, deserialize_index, serialize_index};
use crate::keys::{decrypt_bytes, decrypt_pri_key, encrypt_bytes, encrypt_pri_key};

/// 一个在内存中已解密、可读写的容器句柄。
pub struct Container {
    /// 容器文件路径
    path: PathBuf,
    /// 非对称密钥对（解密内容用；P5 会加 zeroize 清零）
    key_pair: age::x25519::Identity,
    /// 内存中的目录树（P1 用扁平列表）
    nodes: Vec<FsNode>,
    /// blob 区结束处 = Index 的起始偏移；下一个 blob 从这里追加
    blob_end: u64,
}

impl Container {
    /// 新建一个空容器并写入磁盘。
    ///
    /// # 参数
    /// - `path`:       容器文件路径（如 "photos.veil"）
    /// - `passphrase`: 用户密码，用来加密新生成的私钥
    pub fn create(path: impl AsRef<Path>, passphrase: SecretString) -> Result<Container> {
        // 1) 生成一对全新的非对称密钥
        let key_pair = age::x25519::Identity::generate();

        // 2) 用密码把私钥加密成密文私钥（scrypt 一生一次就在这）
        let cip_pri_key = encrypt_pri_key(&key_pair, passphrase)?;

        // 3) 先只写 Header，拿到它的长度（= blob 区起点，此刻也是 Index 起点）
        //    用花括号把 writer 限制在块内，块结束即关闭这个文件句柄
        let header_len = {
            let file = File::create(path.as_ref())?;
            let mut writer = BufWriter::new(file);
            let n = format::write_header(&mut writer, &cip_pri_key)?;
            writer.flush()?; // BufWriter 缓冲必须 flush 才真正落盘
            n
        };

        // 4) 组装句柄：空目录树，blob_end 指向 Header 之后
        //    只调 &self 的方法，所以 container 不需要 mut
        let container = Container {
            path: path.as_ref().to_path_buf(),
            key_pair,
            nodes: Vec::new(),
            blob_end: header_len, // 空容器没有 blob，Index 紧跟 Header
        };

        // 5) 写入空 Index + Footer
        container.write_index_and_footer()?;
        Ok(container)
    }

    /// 打开已存在的容器：用密码解密私钥，读出目录树。
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

        // Index 起点就是 blob 区的结束处
        Ok(Container { path, key_pair, nodes, blob_end: index_offset })
    }

    /// 往容器里追加一个文件。
    ///
    /// # 参数
    /// - `virtual_path`: 容器内的虚拟路径，如 "photos/a.jpg"
    /// - `plaintext`:    文件明文内容
    pub fn add_file(&mut self, virtual_path: &str, plaintext: &[u8]) -> Result<()> {
        // 明文的 blake3 哈希，存进节点，日后读出时比对（往返校验）
        let content_hash: [u8; 32] = blake3::hash(plaintext).into();

        // 用公钥把内容加密成一个独立 blob
        let blob_cipher = encrypt_bytes(&self.key_pair.to_public(), plaintext)?;

        // 在 blob_end 处追加这个 blob
        // （blob_end 此刻正是旧 Index 的位置，旧 Index 稍后被重写覆盖）
        {
            let mut file = OpenOptions::new().read(true).write(true).open(&self.path)?;
            file.seek(SeekFrom::Start(self.blob_end))?;
            file.write_all(&blob_cipher)?;
        }

        // 记录这个文件的节点
        self.nodes.push(FsNode {
            path: virtual_path.to_owned(),
            kind: Kind::File,
            size: plaintext.len() as u64,
            blob_offset: self.blob_end,
            blob_len: blob_cipher.len() as u64,
            content_hash,
            mime: None,
            mtime: None,
        });
        self.blob_end += blob_cipher.len() as u64; // blob 区变长了

        // 把更新后的 Index + Footer 重写到 blob 区之后
        self.write_index_and_footer()?;
        Ok(())
    }

    /// 按虚拟路径删除一个文件。
    ///
    /// 只从目录树移除该节点并重写 Index/Footer；被删文件的 blob 字节仍留在文件里成为
    /// **死空间**（仍是密文，不泄露内容），空间回收留到 P6 的 compaction。
    ///
    /// # 参数
    /// - `virtual_path`: 要删除的文件在容器内的虚拟路径
    ///
    /// # 错误
    /// 找不到该文件时返回 `VeilError::Format`。
    pub fn remove_file(&mut self, virtual_path: &str) -> Result<()> {
        let before = self.nodes.len();
        // retain：只保留“不是目标文件”的节点，等于删掉目标
        self.nodes
            .retain(|n| !(n.path == virtual_path && n.kind == Kind::File));

        // 数量没变说明没找到
        if self.nodes.len() == before {
            return Err(VeilError::Format(format!("找不到文件: {virtual_path}")));
        }

        // 重写 Index + Footer（blob 数据不动；被删 blob 成为死空间）
        self.write_index_and_footer()?;
        Ok(())
    }

    /// 读出某个文件的明文，并做 blake3 往返校验。
    /// **只解密这一个文件的 blob，完全不碰其他数据。**
    ///
    /// # 参数
    /// - `virtual_path`: 要读取的文件在容器内的虚拟路径
    pub fn read_file(&self, virtual_path: &str) -> Result<Vec<u8>> {
        // 在内存目录树里查这个路径（只找文件，不找目录）
        let node = self
            .nodes
            .iter()
            .find(|n| n.path == virtual_path && n.kind == Kind::File)
            .ok_or_else(|| VeilError::Format(format!("找不到文件: {virtual_path}")))?;

        // 只读该 blob 的那一段字节（seek 到偏移，精确读 blob_len 个字节）
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(node.blob_offset))?;
        let mut blob_cipher = vec![0u8; node.blob_len as usize];
        file.read_exact(&mut blob_cipher)?;

        // 解密
        let plaintext = decrypt_bytes(&self.key_pair, &blob_cipher)?;

        // 往返校验：重算 blake3，应与存的一致；不一致说明数据损坏
        let hash: [u8; 32] = blake3::hash(&plaintext).into();
        if hash != node.content_hash {
            return Err(VeilError::Format("内容哈希不匹配（数据损坏？）".into()));
        }
        Ok(plaintext)
    }

    /// 把容器里所有文件解密导出到 `out_dir`，重建目录结构。
    ///
    /// 相当于 `add_file` 的逆操作（“解压”整个容器）。
    /// 每个文件都会经过 `read_file` 的 blake3 往返校验，校验不过即报错。
    ///
    /// # 参数
    /// - `out_dir`: 导出目标目录（不存在会自动创建）
    ///
    /// # 注意
    /// 这会把**明文写到磁盘**（显式导出，非查看流程）。调用方自行确保目标位置安全。
    pub fn extract_all(&self, out_dir: impl AsRef<Path>) -> Result<()> {
        let out_dir = out_dir.as_ref();

        for node in &self.nodes {
            // 目标路径 = 导出目录 + 容器内的虚拟路径
            let dest = out_dir.join(&node.path);

            match node.kind {
                // 目录：直接建出来（create_dir_all 会一路建齐父级）
                Kind::Dir => {
                    std::fs::create_dir_all(&dest)?;
                }
                // 文件：先确保父目录存在，再解密内容写入
                Kind::File => {
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    // read_file 内部只解密该 blob，并做 blake3 校验
                    let plaintext = self.read_file(&node.path)?;
                    std::fs::write(&dest, plaintext)?;
                }
            }
        }
        Ok(())
    }

    /// 按虚拟路径解密**单个**文件，写到磁盘上的 `dest`。
    ///
    /// = `read_file`（解密到内存 + blake3 校验）+ 写文件；`extract_all` 的单文件版。
    ///
    /// # 参数
    /// - `virtual_path`: 容器内要解密的文件路径
    /// - `dest`:         输出文件路径（父目录不存在会自动创建）
    ///
    /// # 注意
    /// 会把**明文写到磁盘**（显式导出，非查看流程）。
    pub fn extract_file(&self, virtual_path: &str, dest: impl AsRef<Path>) -> Result<()> {
        let dest = dest.as_ref();

        // 解密内容（内部含 blake3 往返校验）
        let plaintext = self.read_file(virtual_path)?;

        // 确保父目录存在（dest 是裸文件名时 parent 为空，跳过）
        // let-chains（edition 2024）：if let 和条件用 && 串在一起
        if let Some(parent) = dest.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(dest, plaintext)?;
        Ok(())
    }

    /// 当前目录树（只读借用）
    pub fn nodes(&self) -> &[FsNode] {
        &self.nodes
    }

    /// 把目录树渲染成多行字符串（类似 `tree` 命令）。
    ///
    /// 存储始终是扁平的 `Vec<FsNode>`，这里用 [`build_tree`] 在**展示时**临时折叠成
    /// 嵌套树再渲染，不改动存储。目录名后带 `/`，文件不带；空容器返回空字符串。
    pub fn tree_view(&self) -> String {
        let root = build_tree(&self.nodes);

        // 递归渲染。prefix 是当前层的缩进前缀（含竖线）
        fn render(node: &TreeNode, prefix: &str, out: &mut String) {
            let count = node.children.len();
            for (i, (name, child)) in node.children.iter().enumerate() {
                let is_last = i == count - 1;
                let branch = if is_last { "└── " } else { "├── " };
                // file 为 None 即目录，名字后加 "/"
                let slash = if child.file.is_none() { "/" } else { "" };
                out.push_str(&format!("{prefix}{branch}{name}{slash}\n"));
                // 下一层前缀：本项是最后一个就用空格，否则用竖线延续
                let child_prefix = format!("{prefix}{}", if is_last { "    " } else { "│   " });
                render(child, &child_prefix, out);
            }
        }

        let mut out = String::new();
        render(&root, "", &mut out);
        out
    }

    /// 把 Index + Footer 写到 blob 区之后（`blob_end` 处），并截断多余尾巴。
    ///
    /// P1 简化：每次加文件都重写 Index/Footer（很小）。blob 数据不重写。
    fn write_index_and_footer(&self) -> Result<()> {
        // 目录树 → 序列化 → 用公钥加密
        let index_plain = serialize_index(&self.nodes)?;
        let index_cipher = encrypt_bytes(&self.key_pair.to_public(), &index_plain)?;

        // 定位到 blob 区之后，写 Index，再写 Footer 记录 Index 的位置
        let mut file = OpenOptions::new().read(true).write(true).open(&self.path)?;
        file.seek(SeekFrom::Start(self.blob_end))?;
        file.write_all(&index_cipher)?;
        format::write_footer(&mut file, self.blob_end, index_cipher.len() as u64)?;

        // 新的 Index+Footer 可能比旧的短，砍掉文件末尾可能残留的旧字节
        let end = self.blob_end + index_cipher.len() as u64 + format::FOOTER_LEN;
        file.set_len(end)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 生成一个进程内唯一的临时路径（并行测试互不踩），测试结束自行清理。
    fn temp_path(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("veil_test_{}_{n}_{tag}", std::process::id()))
    }

    fn pass() -> SecretString {
        SecretString::from("correct horse".to_owned())
    }

    #[test]
    fn create_open_empty() {
        let path = temp_path("empty.veil");
        Container::create(&path, pass()).unwrap();

        let container = Container::open(&path, pass()).unwrap();
        assert_eq!(container.nodes().len(), 0);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn add_read_roundtrip() {
        let path = temp_path("rt.veil");
        let mut container = Container::create(&path, pass()).unwrap();
        container.add_file("a.txt", b"hello").unwrap();
        container.add_file("dir/b.bin", &[0u8, 1, 2, 255]).unwrap();

        // 当前句柄能读回
        assert_eq!(container.read_file("a.txt").unwrap(), b"hello");

        // 重新打开后仍能读回，且条目数正确
        let reopened = Container::open(&path, pass()).unwrap();
        assert_eq!(reopened.nodes().len(), 2);
        assert_eq!(reopened.read_file("dir/b.bin").unwrap(), vec![0u8, 1, 2, 255]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn wrong_passphrase_fails() {
        let path = temp_path("wp.veil");
        Container::create(&path, pass()).unwrap();

        let wrong = SecretString::from("nope".to_owned());
        assert!(Container::open(&path, wrong).is_err());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn remove_then_missing() {
        let path = temp_path("rm.veil");
        let mut container = Container::create(&path, pass()).unwrap();
        container.add_file("keep.txt", b"1").unwrap();
        container.add_file("gone.txt", b"2").unwrap();
        container.remove_file("gone.txt").unwrap();

        // 删不存在的文件 → 报错
        assert!(container.remove_file("nope.txt").is_err());

        let reopened = Container::open(&path, pass()).unwrap();
        assert_eq!(reopened.nodes().len(), 1);
        assert_eq!(reopened.read_file("keep.txt").unwrap(), b"1");
        assert!(reopened.read_file("gone.txt").is_err()); // 已删，读不到

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_missing_file_errors() {
        let path = temp_path("miss.veil");
        let container = Container::create(&path, pass()).unwrap();
        assert!(container.read_file("nope.txt").is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn extract_all_writes_files() {
        let path = temp_path("ex.veil");
        let mut container = Container::create(&path, pass()).unwrap();
        container.add_file("x.txt", b"X").unwrap();
        container.add_file("sub/y.txt", b"Y").unwrap();

        let out = temp_path("ex_out");
        container.extract_all(&out).unwrap();
        assert_eq!(std::fs::read(out.join("x.txt")).unwrap(), b"X");
        assert_eq!(std::fs::read(out.join("sub/y.txt")).unwrap(), b"Y");

        std::fs::remove_dir_all(&out).ok();
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn tamper_is_detected() {
        let path = temp_path("tamper.veil");
        let mut container = Container::create(&path, pass()).unwrap();
        container.add_file("secret.txt", b"top secret content").unwrap();

        // 找到该文件 blob 的中间位置
        let node = &container.nodes()[0];
        let flip_at = node.blob_offset + node.blob_len / 2;

        // 翻转 blob 里的一个字节（模拟 bit rot / 篡改）
        {
            let mut file = OpenOptions::new().read(true).write(true).open(&path).unwrap();
            file.seek(SeekFrom::Start(flip_at)).unwrap();
            let mut byte = [0u8; 1];
            file.read_exact(&mut byte).unwrap();
            byte[0] ^= 0xFF;
            file.seek(SeekFrom::Start(flip_at)).unwrap();
            file.write_all(&byte).unwrap();
        }

        // header/index 未动，能打开；但读该文件必失败（age 认证 或 blake3 校验）
        let reopened = Container::open(&path, pass()).unwrap();
        assert!(reopened.read_file("secret.txt").is_err());

        std::fs::remove_file(&path).ok();
    }
}
