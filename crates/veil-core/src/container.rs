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
use crate::slice_reader::SliceReader;
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
    /// - `passphrase`: 用户密码（`impl Into<SecretString>`，可直接传 `String`）
    ///
    /// # 返回
    /// - `Ok(Container)`：已写入磁盘的空容器句柄
    /// - `Err(VeilError)`：加密或写文件失败
    pub fn create(path: impl AsRef<Path>, passphrase: impl Into<SecretString>) -> Result<Container> {
        // 接受 impl Into<SecretString>：渲染层可直接传 String，无需依赖 age
        let passphrase = passphrase.into();

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
    /// - `passphrase`: 用户密码（`impl Into<SecretString>`，可直接传 `String`；错误则解密失败）
    ///
    /// # 返回
    /// - `Ok(Container)`：解密后可读写的容器句柄（含目录树）
    /// - `Err(VeilError)`：密码错误、文件损坏或格式不符
    pub fn open(path: impl AsRef<Path>, passphrase: impl Into<SecretString>) -> Result<Container> {
        let passphrase = passphrase.into(); // 渲染层可直接传 String，无需依赖 age
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
    ///
    /// # 返回
    /// - `Ok(())`：文件已加密追加、Index/Footer 已更新
    /// - `Err(VeilError)`：加密或写文件失败
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

        // 这个文件的节点
        let node = FsNode {
            path: virtual_path.to_owned(),
            kind: Kind::File,
            size: plaintext.len() as u64,
            blob_offset: self.blob_end,
            blob_len: blob_cipher.len() as u64,
            content_hash,
            // 按扩展名猜 MIME，供查看器分发（未知为 None）
            mime: crate::mime::guess_mime(virtual_path),
            mtime: None,
        };
        self.blob_end += blob_cipher.len() as u64; // blob 区变长了

        // 同名则**覆盖**（旧节点被替换，旧 blob 成死空间待 P6 回收）；否则追加
        match self.nodes.iter_mut().find(|n| n.path == virtual_path) {
            Some(existing) => *existing = node,
            None => self.nodes.push(node),
        }

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
    /// # 返回
    /// - `Ok(())`：已从目录树移除并重写 Index/Footer
    /// - `Err(VeilError)`：找不到该文件，或写文件失败
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

    /// 修改容器密码。
    ///
    /// 只用新密码重新加密**私钥**（重写 Header 里那一小段 `cip_pri_key`），
    /// 几百 GB 的 blob 一个字节都不动——这是「两级密钥」设计的红利。
    ///
    /// # 参数
    /// - `new_passphrase`: 新密码（`impl Into<SecretString>`，可直接传 `String`）
    /// # 返回
    /// - `Ok(())`：Header 的密文私钥已用新密码重写
    /// - `Err(VeilError)`：加密或写文件失败
    pub fn change_password(&self, new_passphrase: impl Into<SecretString>) -> Result<()> {
        // 用新密码重新加密私钥（scrypt 就在这，仅这一次小操作）
        let new_cip_pri_key = encrypt_pri_key(&self.key_pair, new_passphrase.into())?;

        let mut file = OpenOptions::new().read(true).write(true).open(&self.path)?;
        // 确认新旧密文私钥等长（同长明文经 age 加密后长度稳定）；否则原地覆盖会破坏后续 blob
        let old_cip_pri_key = format::read_header(&mut file)?;
        if new_cip_pri_key.len() != old_cip_pri_key.len() {
            return Err(VeilError::Format("密文私钥长度变化，无法原地改密码".into()));
        }

        // 定位到 Header 里 cip_pri_key 的起点（偏移 16），原地覆盖
        file.seek(SeekFrom::Start(format::HEADER_FIXED_LEN))?;
        file.write_all(&new_cip_pri_key)?;
        Ok(())
    }

    /// 重命名 / 移动一个文件（改虚拟路径）。移动 = 目标路径带上新目录。
    ///
    /// 只改目录树里的路径（并按新扩展名重识别 MIME），重写 Index/Footer；blob 不动。
    ///
    /// # 参数
    /// - `from`: 现有文件路径
    /// - `to`:   目标路径（如 "archive/2024/a.jpg"）
    /// # 返回
    /// - `Ok(())`：已重命名并重写索引
    /// - `Err(VeilError)`：找不到源文件，或目标路径已存在
    pub fn rename_file(&mut self, from: &str, to: &str) -> Result<()> {
        // 目标已存在 → 拒绝（避免撞名产生歧义）
        if self.nodes.iter().any(|n| n.path == to) {
            return Err(VeilError::Format(format!("目标路径已存在: {to}")));
        }

        let node = self
            .nodes
            .iter_mut()
            .find(|n| n.path == from && n.kind == Kind::File)
            .ok_or_else(|| VeilError::Format(format!("找不到文件: {from}")))?;
        node.path = to.to_owned();
        node.mime = crate::mime::guess_mime(to); // 扩展名可能变，重新识别

        self.write_index_and_footer()?;
        Ok(())
    }

    /// 递归添加磁盘目录 `src_dir` 下的所有文件到容器。
    ///
    /// 每个文件按其相对路径加入，前缀 `dest_prefix`。
    /// 例：`add_dir("/data/photos", "photos")` → 文件进 "photos/2024/a.jpg" 等。
    ///
    /// # 参数
    /// - `src_dir`:     磁盘上的源目录
    /// - `dest_prefix`: 容器内的目标前缀（空串 = 放到根）
    /// # 返回
    /// - `Ok(())`：目录下所有文件已加入
    /// - `Err(VeilError)`：读目录/文件或加密失败
    ///
    /// 注意：目前每个文件走一次 `add_file`（各重写一次 Index），文件很多时偏慢；
    /// 且 `add_file` 整份读进内存，超大文件请等流式接口。
    pub fn add_dir(&mut self, src_dir: impl AsRef<Path>, dest_prefix: &str) -> Result<()> {
        let src_dir = src_dir.as_ref();

        // 先递归收集所有文件（绝对路径, 容器内虚拟路径）
        let mut files = Vec::new();
        collect_files(src_dir, src_dir, dest_prefix, &mut files)?;

        for (abs_path, virtual_path) in files {
            let bytes = std::fs::read(&abs_path)?;
            self.add_file(&virtual_path, &bytes)?;
        }
        Ok(())
    }

    /// 在目录树里按路径查一个**文件**节点（找不到 / 是目录 → 报错）。
    fn find_file(&self, virtual_path: &str) -> Result<&FsNode> {
        self.nodes
            .iter()
            .find(|n| n.path == virtual_path && n.kind == Kind::File)
            .ok_or_else(|| VeilError::Format(format!("找不到文件: {virtual_path}")))
    }

    /// 打开一个「只解密该 blob」的流式解密读取器（可 Seek）。
    ///
    /// 用 [`SliceReader`] 把容器大文件限定到该 blob 的 `[offset, offset+len)`，
    /// 再交给 age 解密。因为 `SliceReader` 可 Seek，返回的 `StreamReader` 也可 Seek
    /// —— 这就是随机访问（视频拖动）的基础。**完全不碰其他 blob。**
    ///
    /// # 参数
    /// - `node`: 要解密的文件节点
    ///
    /// # 返回
    /// - `Ok(reader)`：新建的 `StreamReader`流式解密读取器
    fn open_blob_reader(
        &self,
        node: &FsNode,
    ) -> Result<age::stream::StreamReader<SliceReader<File>>> {
        // 打开容器文件
        let file = File::open(&self.path)?;
        // 只读该文件的 blob 的范围，获取文件句柄
        let slice = SliceReader::new(file, node.blob_offset, node.blob_len)?;
        // 读 age 密文的"头部"
        let decryptor = age::Decryptor::new(slice)?;
        // 获取解密流
        let reader = decryptor.decrypt(std::iter::once(&self.key_pair as &dyn age::Identity))?;
        // 流式解密读取器
        Ok(reader)
    }

    /// 读出某个文件的完整明文，并做 blake3 往返校验。
    /// **流式解密、只碰这一个文件的 blob。**
    ///
    /// # 参数
    /// - `virtual_path`: 要读取的文件在容器内的虚拟路径
    /// # 返回
    /// - `Ok(plaintext)`：读到的明文字节
    /// - `Err(e)`：读取错误（如文件不存在、数据损坏等）
    pub fn read_file(&self, virtual_path: &str) -> Result<Vec<u8>> {
        let node = self.find_file(virtual_path)?;

        // 流式解密整段：不把密文整块预读进内存，而是边读边解
        let mut reader = self.open_blob_reader(node)?;
        let mut plaintext = Vec::new();
        // 把 reader 里的字节全部读进 plaintext
        // 不预读整块密文,reader 边从磁盘拉一小段密文、边解、边扔进 plaintext → 内存里只有明文在增长
        // 内存保存整个明文，不保存密文
        reader.read_to_end(&mut plaintext)?;

        // 往返校验：重算 blake3，应与存的一致；不一致说明数据损坏
        let hash: [u8; 32] = blake3::hash(&plaintext).into();
        if hash != node.content_hash {
            return Err(VeilError::Format("内容哈希不匹配（数据损坏？）".into()));
        }
        Ok(plaintext)
    }

    /// 随机读取某文件解密后的 `[offset, offset+len)` 一段（不读整文件）。
    ///
    /// 用于大文件 / 视频拖动：`seek` 到解密流的指定位置，只读请求的这一小段。
    /// 末尾不足 `len` 时返回实际读到的字节。
    ///
    /// 注意：只读一段时**无法做整文件 blake3 校验**（那需要全部明文）；不过 age 的
    /// 分块认证仍保证所读片段未被篡改。
    ///
    /// # 参数
    /// - `virtual_path`: 文件路径
    /// - `offset`:       从解密后明文的第几个字节开始
    /// - `len`:          最多读多少字节
    /// # 返回
    /// - `Ok(bufplaintext)`：读到的明文字节
    /// - `Err(e)`：读取错误（如文件不存在、偏移量超出范围等）
    ///
    pub fn read_range(&self, virtual_path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let node = self.find_file(virtual_path)?;
        // 获取流式解密读取器
        let mut reader = self.open_blob_reader(node)?;
        // 定位到指定偏移量，开始读取
        reader.seek(SeekFrom::Start(offset))?; // 在解密流里定位设置读取起始位置
        let mut buf = Vec::new();
        // take(len)：最多读 len 字节（到末尾就早停）
        reader.take(len as u64) // 设置读取最大字节数为 len
            .read_to_end(&mut buf)?; // 全部读取到 buf 里
        Ok(buf)
    }

    /// 把某文件解密到一个受控临时位置（优先 RAM 盘），返回 RAII 守卫。
    ///
    /// 用于**视频 / 音频 V1**：解密后把 [`TempPlaintext::path`] 交系统播放器打开；
    /// 守卫变量 Drop 时临时明文自动删除。流式解密，不整份进内存（大视频友好）。
    ///
    /// # 安全
    /// 临时明文会短暂落在**容器外**的受控目录（优先 RAM）、权限 0600、守卫销毁即删。
    /// SSD 上删除不保证物理擦除，UI 需如实告知（spec §6.2）。
    pub fn extract_to_temp(&self, virtual_path: &str) -> Result<crate::temp::TempPlaintext> {
        // 查找文件节点
        let node = self.find_file(virtual_path)?;
        // 打开流式解密读取器
        let reader = self.open_blob_reader(node)?;
        Ok(crate::temp::decrypt_to_temp(virtual_path, reader)?)
    }

    /// 把容器里所有文件解密导出到 `out_dir`，重建目录结构。
    ///
    /// 相当于 `add_file` 的逆操作（“解压”整个容器）。
    /// 每个文件都会经过 `read_file` 的 blake3 往返校验，校验不过即报错。
    ///
    /// # 参数
    /// - `out_dir`: 导出目标目录（不存在会自动创建）
    ///
    /// # 返回
    /// - `Ok(())`：所有文件已解密并重建目录结构写入 `out_dir`
    /// - `Err(VeilError)`：某文件校验失败或写盘失败
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
    /// # 返回
    /// - `Ok(())`：文件已解密并写入 `dest`
    /// - `Err(VeilError)`：找不到文件、校验失败或写盘失败
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

    /// 把某个虚拟目录（前缀）下的所有文件解密导出到 `out_dir`，保留相对结构。
    ///
    /// 例：`extract_dir("photos", "out")` → "photos/2024/a.jpg" 导出到 "out/2024/a.jpg"。
    ///
    /// # 参数
    /// - `virtual_prefix`: 容器内的目录前缀（如 "photos" 或 "photos/2024"）
    /// - `out_dir`:        导出目标目录（不存在会自动创建）
    /// # 返回
    /// - `Ok(())`：该目录下所有文件已导出
    /// - `Err(VeilError)`：某文件校验失败或写盘失败
    pub fn extract_dir(&self, virtual_prefix: &str, out_dir: impl AsRef<Path>) -> Result<()> {
        let out_dir = out_dir.as_ref();
        let prefix = format!("{}/", virtual_prefix.trim_end_matches('/'));

        for node in &self.nodes {
            if node.kind != Kind::File || !node.path.starts_with(&prefix) {
                continue;
            }
            // 相对该目录的路径，保留子结构
            let rel = node.path.strip_prefix(&prefix).unwrap_or(&node.path);
            let dest = out_dir.join(rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let plaintext = self.read_file(&node.path)?;
            std::fs::write(&dest, plaintext)?;
        }
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

/// 递归收集 `dir` 下的所有文件，算出各自在容器里的虚拟路径（供 [`Container::add_dir`] 用）。
///
/// `base` 是遍历起点，用来算相对路径；`dest_prefix` 拼在相对路径前面。
fn collect_files(
    base: &Path,
    dir: &Path,
    dest_prefix: &str,
    out: &mut Vec<(PathBuf, String)>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_files(base, &path, dest_prefix, out)?; // 递归下钻
        } else if path.is_file() {
            // 相对 base 的路径，统一用 "/" 连接（Windows 的 "\" 也换成 "/"）
            let rel = path.strip_prefix(base).unwrap_or(&path);
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let virtual_path = if dest_prefix.is_empty() {
                rel_str
            } else {
                format!("{}/{}", dest_prefix.trim_end_matches('/'), rel_str)
            };
            out.push((path, virtual_path));
        }
    }
    Ok(())
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
    fn extract_to_temp_and_auto_cleanup() {
        let path = temp_path("tmp.veil");
        let mut container = Container::create(&path, pass()).unwrap();
        container.add_file("clip.mp4", b"fake video bytes").unwrap();

        let temp_file_path;
        {
            let tmp = container.extract_to_temp("clip.mp4").unwrap();
            temp_file_path = tmp.path().to_path_buf();

            // 临时文件存在、内容正确、保留了扩展名
            assert!(temp_file_path.exists());
            assert_eq!(std::fs::read(&temp_file_path).unwrap(), b"fake video bytes");
            assert_eq!(temp_file_path.extension().unwrap(), "mp4");
        } // tmp 在这里 Drop → 应自动删除临时文件

        assert!(!temp_file_path.exists(), "Drop 后临时明文应被删除");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn add_file_fills_mime() {
        let path = temp_path("mime.veil");
        let mut container = Container::create(&path, pass()).unwrap();
        container.add_file("a.jpg", b"x").unwrap();
        container.add_file("notes", b"y").unwrap(); // 无扩展名

        let reopened = Container::open(&path, pass()).unwrap();
        let jpg = reopened.find_file("a.jpg").unwrap();
        let notes = reopened.find_file("notes").unwrap();
        assert_eq!(jpg.mime.as_deref(), Some("image/jpeg"));
        assert_eq!(notes.mime, None);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn change_password_works() {
        let path = temp_path("chpw.veil");
        let mut container = Container::create(&path, "old-pass".to_string()).unwrap();
        container.add_file("f.txt", b"data").unwrap();
        container.change_password("new-pass".to_string()).unwrap();

        // 旧密码打不开，新密码可以，内容还在
        assert!(Container::open(&path, "old-pass".to_string()).is_err());
        let reopened = Container::open(&path, "new-pass".to_string()).unwrap();
        assert_eq!(reopened.read_file("f.txt").unwrap(), b"data");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rename_file_works() {
        let path = temp_path("rename.veil");
        let mut container = Container::create(&path, pass()).unwrap();
        container.add_file("a.txt", b"hi").unwrap();
        container.rename_file("a.txt", "sub/b.md").unwrap();

        // 撞名要报错
        container.add_file("keep.txt", b"x").unwrap();
        assert!(container.rename_file("keep.txt", "sub/b.md").is_err());

        let reopened = Container::open(&path, pass()).unwrap();
        assert!(reopened.read_file("a.txt").is_err()); // 旧路径没了
        assert_eq!(reopened.read_file("sub/b.md").unwrap(), b"hi");
        // MIME 跟着新扩展名更新
        let node = reopened.nodes().iter().find(|n| n.path == "sub/b.md").unwrap();
        assert_eq!(node.mime.as_deref(), Some("text/markdown"));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn add_file_overwrites_same_path() {
        let path = temp_path("overwrite.veil");
        let mut container = Container::create(&path, pass()).unwrap();
        container.add_file("a.txt", b"first").unwrap();
        container.add_file("a.txt", b"second-longer").unwrap(); // 同名覆盖

        // 无重复节点，内容是最新的
        assert_eq!(container.nodes().iter().filter(|n| n.path == "a.txt").count(), 1);
        assert_eq!(container.read_file("a.txt").unwrap(), b"second-longer");

        let reopened = Container::open(&path, pass()).unwrap();
        assert_eq!(reopened.nodes().len(), 1);
        assert_eq!(reopened.read_file("a.txt").unwrap(), b"second-longer");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn extract_dir_subtree() {
        let path = temp_path("exdir.veil");
        let mut container = Container::create(&path, pass()).unwrap();
        container.add_file("photos/2024/a.txt", b"A").unwrap();
        container.add_file("photos/b.txt", b"B").unwrap();
        container.add_file("docs/c.txt", b"C").unwrap(); // 不属于 photos

        let out = temp_path("exdir_out");
        container.extract_dir("photos", &out).unwrap();
        assert_eq!(std::fs::read(out.join("2024/a.txt")).unwrap(), b"A");
        assert_eq!(std::fs::read(out.join("b.txt")).unwrap(), b"B");
        assert!(!out.join("c.txt").exists()); // docs 下的没被导出

        std::fs::remove_dir_all(&out).ok();
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn add_dir_recursive() {
        // 造一个磁盘目录树
        let src = temp_path("srcdir");
        std::fs::create_dir_all(src.join("nested")).unwrap();
        std::fs::write(src.join("top.txt"), b"top").unwrap();
        std::fs::write(src.join("nested/deep.bin"), b"deep").unwrap();

        let path = temp_path("adddir.veil");
        let mut container = Container::create(&path, pass()).unwrap();
        container.add_dir(&src, "imported").unwrap();

        let reopened = Container::open(&path, pass()).unwrap();
        assert_eq!(reopened.read_file("imported/top.txt").unwrap(), b"top");
        assert_eq!(reopened.read_file("imported/nested/deep.bin").unwrap(), b"deep");

        std::fs::remove_dir_all(&src).ok();
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_range_random_access() {
        let path = temp_path("range.veil");
        let mut container = Container::create(&path, pass()).unwrap();
        container.add_file("data.bin", b"0123456789ABCDEF").unwrap();

        let reopened = Container::open(&path, pass()).unwrap();
        // 从第 4 字节起读 5 个 → "45678"
        assert_eq!(reopened.read_range("data.bin", 4, 5).unwrap(), b"45678");
        // 末尾不足：从第 14 字节起读 10 个 → 只剩 "EF"
        assert_eq!(reopened.read_range("data.bin", 14, 10).unwrap(), b"EF");
        // 整段仍可完整读回
        assert_eq!(reopened.read_file("data.bin").unwrap(), b"0123456789ABCDEF");

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
