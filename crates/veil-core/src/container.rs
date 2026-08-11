//! # container —— .veil 容器的总装：create / add / list / read
//!
//! 本模块把 [`crate::keys`]（加解密）、[`crate::index`]（嵌套目录树）、
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
//! | Index = age(pub_key, 序列化后的嵌套目录树)                 |
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
//! - **公钥 `pub_key`**：只加密（写 blob / 写 Index）；
//! - **私钥 `key_pair`**：只解密（读 blob / 读 Index），本身被密码加密成 `cip_pri_key`；
//! - **密码**：只在加密/解密 `cip_pri_key` 处出现，经 scrypt 派生，每容器一生一次。

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use age::secrecy::SecretString;

use crate::error::{Result, VeilError};
use crate::format;
use crate::index::{self, FileMeta, Node, Tree, deserialize_index, serialize_index};
use crate::keys::{decrypt_bytes, decrypt_pri_key, encrypt_bytes, encrypt_pri_key};
use crate::slice_reader::SliceReader;

/// 一个在内存中已解密、可读写的容器句柄。
pub struct Container {
    /// 容器文件路径
    path: PathBuf,
    /// 非对称密钥对（解密内容用；P5 会加 zeroize 清零）
    key_pair: age::x25519::Identity,
    /// 内存中的**嵌套**目录树
    root: Tree,
    /// 容器创建时使用的 CLI 版本
    cli_version: String,
    // 崩溃安全策略：所有写入都**追加到文件末尾**（永不覆盖已提交数据），
    // Footer 最后写 + fsync 作为提交点；下一个 blob / Index 都从当前 EOF 追加，
    // 所以不需要单独记 blob_end。
}

impl Container {
    /// 新建一个空容器并写入磁盘。
    ///
    /// # 参数
    /// - `path`:       容器文件路径（如 "photos.veil"）
    /// - `passphrase`: 用户密码（`impl Into<SecretString>`，可直接传 `String`）
    /// - `cli_version`: CLI 版本字符串（如 "1.1.0"）
    /// # 返回
    /// - `Ok(Container)`：已写入磁盘的空容器句柄
    /// - `Err(VeilError)`：加密或写文件失败
    pub fn create(path: impl AsRef<Path>, passphrase: impl Into<SecretString>, cli_version: &str) -> Result<Container> {
        let passphrase = passphrase.into();
        let key_pair = age::x25519::Identity::generate();
        let cip_pri_key = encrypt_pri_key(&key_pair, passphrase)?;

        // 写 Header（文件开头）
        {
            let file = File::create(path.as_ref())?;
            let mut writer = BufWriter::new(file);
            format::write_header(&mut writer, cli_version, &cip_pri_key)?;
            writer.flush()?;
        }

        let container = Container {
            path: path.as_ref().to_path_buf(),
            key_pair,
            root: Tree::new(),
            cli_version: cli_version.to_string(),
        };
        container.commit()?; // 追加空 Index + Footer
        Ok(container)
    }

    /// 打开已存在的容器：用密码解密私钥，读出目录树。
    ///
    /// # 参数
    /// - `path`:       容器文件路径
    /// - `passphrase`: 用户密码（`impl Into<SecretString>`，可直接传 `String`；错误则解密失败）
    /// # 返回
    /// - `Ok(Container)`：解密后可读写的容器句柄（含目录树）
    /// - `Err(VeilError)`：密码错误、文件损坏或格式不符
    pub fn open(path: impl AsRef<Path>, passphrase: impl Into<SecretString>) -> Result<Container> {
        let passphrase = passphrase.into();
        let path = path.as_ref().to_path_buf();

        let header = {
            let mut file = File::open(&path)?;
            format::read_header(&mut file)?
        };
        let key_pair = decrypt_pri_key(&header.cip_pri_key, passphrase)?;

        // 加载 Index：正常读文件尾 Footer；崩溃过则恢复到上一个有效 Footer
        let root = recover_index(&path, &key_pair)?;

        Ok(Container {
            path,
            key_pair,
            root,
            cli_version: header.cli_version,
        })
    }

    /// 往容器里追加一个文件（同名则覆盖，旧 blob 成死空间）。
    ///
    /// 适用于**小文件**或已在内存的数据。大文件（几十 MB 以上）请用 [`add_file_streaming`](Self::add_file_streaming)。
    ///
    /// # 参数
    /// - `virtual_path`: 容器内的虚拟路径，如 "photos/a.jpg"
    /// - `plaintext`:    文件明文内容
    /// # 返回
    /// - `Ok(())`：文件已加密追加、Index/Footer 已更新
    /// - `Err(VeilError)`：加密或写文件失败
    pub fn add_file(&mut self, virtual_path: &str, plaintext: &[u8]) -> Result<()> {
        let content_hash: [u8; 32] = blake3::hash(plaintext).into();
        let blob_cipher = encrypt_bytes(&self.key_pair.to_public(), plaintext)?;

        // 把 blob **追加到文件末尾**并 fsync（确保 blob 落盘后，才让 Index 指向它）
        let blob_offset = {
            let mut file = OpenOptions::new().read(true).write(true).open(&self.path)?;
            let off = file.seek(SeekFrom::End(0))?;
            file.write_all(&blob_cipher)?;
            file.sync_all()?;
            off
        };

        let meta = FileMeta {
            size: plaintext.len() as u64,
            blob_offset,
            blob_len: blob_cipher.len() as u64,
            content_hash,
            mime: crate::mime::guess_mime(virtual_path),
            mtime: None,
        };

        index::insert_file(&mut self.root, virtual_path, meta); // 插入/覆盖到树
        self.commit()?; // 追加新 Index + Footer
        Ok(())
    }

    /// 流式添加文件（适用于大文件，不会一次性读入内存）。
    ///
    /// 使用固定大小的缓冲区（64KB）边读边加密边写入，内存占用恒定。得益于 age 使用
    /// **流密码**（ChaCha20-Poly1305），加密和解密的块大小可以完全不同——加密时用
    /// 64KB 缓冲，解密时可以用 8KB 或 1MB，结果都正确。流密码将明文逐字节与密钥流
    /// XOR，生成连续的密文字节流，没有"块边界"概念，因此无需对齐或填充。
    ///
    /// # 参数
    /// - `virtual_path`: 容器内的虚拟路径
    /// - `reader`:       实现了 `Read` 的数据源（如 `File`、`BufReader`）
    /// # 返回
    /// - `Ok(())`：文件已流式加密追加、Index/Footer 已更新
    /// - `Err(VeilError)`：读取、加密或写文件失败
    ///
    /// # 示例
    /// ```no_run
    /// # use veil_core::container::Container;
    /// # use std::fs::File;
    /// # fn example() -> veil_core::error::Result<()> {
    /// let mut container = Container::open("data.veil", "password")?;
    /// let file = File::open("large_video.mp4")?;
    /// container.add_file_streaming("videos/vacation.mp4", file)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn add_file_streaming(
        &mut self,
        virtual_path: &str,
        mut reader: impl Read,
    ) -> Result<()> {
        const CHUNK_SIZE: usize = 64 * 1024; // 64KB 缓冲区

        let mut file = OpenOptions::new().read(true).write(true).open(&self.path)?;
        let blob_offset = file.seek(SeekFrom::End(0))?;

        // age 流式加密器（写入容器文件）
        let recipient = self.key_pair.to_public();
        let encryptor = age::Encryptor::with_recipients(vec![Box::new(recipient) as Box<dyn age::Recipient>].iter().map(|r| r.as_ref()))
            .expect("failed to create encryptor");
        let mut writer = encryptor.wrap_output(&mut file)?;

        // 边读边 hash 边加密边写
        let mut hasher = blake3::Hasher::new();
        let mut total_size = 0u64;
        let mut buffer = vec![0u8; CHUNK_SIZE];

        loop {
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                break;
            }

            hasher.update(&buffer[..n]);
            writer.write_all(&buffer[..n])?;
            total_size += n as u64;
        }

        writer.finish()?;
        let blob_len = file.stream_position()? - blob_offset;
        file.sync_all()?; // 确保 blob 落盘后，才让 Index 指向它

        let content_hash: [u8; 32] = hasher.finalize().into();

        let meta = FileMeta {
            size: total_size,
            blob_offset,
            blob_len,
            content_hash,
            mime: crate::mime::guess_mime(virtual_path),
            mtime: None,
        };

        index::insert_file(&mut self.root, virtual_path, meta);
        self.commit()?;
        Ok(())
    }

    /// 按虚拟路径删除一个文件（顺带清理变空的父目录）。
    ///
    /// 被删文件的 blob 字节仍留在文件里成为**死空间**（仍是密文），P6 再 compaction 回收。
    ///
    /// # 参数
    /// - `virtual_path`: 要删除的文件路径
    /// # 返回
    /// - `Ok(())`：已从目录树移除并重写 Index/Footer
    /// - `Err(VeilError)`：找不到该文件，或写文件失败
    pub fn remove_file(&mut self, virtual_path: &str) -> Result<()> {
        if index::remove_file(&mut self.root, virtual_path).is_none() {
            return Err(VeilError::Format(format!("找不到文件: {virtual_path}")));
        }
        self.commit()?;
        Ok(())
    }

    /// 修改容器密码。只用新密码重新加密**私钥**（重写整个 Header），
    /// 几百 GB 的 blob 一个字节都不动——「两级密钥」设计的红利。
    ///
    /// 注意：由于 Header 现在包含可变长度的 CLI 版本字符串，我们需要重写整个文件。
    ///
    /// # 参数
    /// - `new_passphrase`: 新密码（`impl Into<SecretString>`，可直接传 `String`）
    /// # 返回
    /// - `Ok(())`：Header 的密文私钥已用新密码重写
    /// - `Err(VeilError)`：加密或写文件失败
    pub fn change_password(&self, new_passphrase: impl Into<SecretString>) -> Result<()> {
        let new_cip_pri_key = encrypt_pri_key(&self.key_pair, new_passphrase.into())?;

        // 读取旧 Header 获取 CLI 版本
        let mut file = OpenOptions::new().read(true).write(true).open(&self.path)?;
        let old_header = format::read_header(&mut file)?;

        // 生成新 Header
        let mut new_header_bytes = Vec::new();
        format::write_header(&mut new_header_bytes, &old_header.cli_version, &new_cip_pri_key)?;

        // 检查新旧 Header 长度是否相同
        let old_header_len = {
            let mut temp_file = File::open(&self.path)?;
            let header = format::read_header(&mut temp_file)?;
            let mut old_bytes = Vec::new();
            format::write_header(&mut old_bytes, &header.cli_version, &header.cip_pri_key)?;
            old_bytes.len()
        };

        if new_header_bytes.len() != old_header_len {
            return Err(VeilError::Format("密文私钥长度变化，无法原地改密码".into()));
        }

        // 原地覆盖 Header
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&new_header_bytes)?;
        file.flush()?;
        Ok(())
    }

    /// 重命名 / 移动一个文件（改虚拟路径）。移动 = 目标路径带上新目录。
    ///
    /// 只改目录树（并按新扩展名重识别 MIME），重写 Index/Footer；blob 不动。
    ///
    /// # 参数
    /// - `from`: 现有文件路径
    /// - `to`:   目标路径（如 "archive/2024/a.jpg"）
    /// # 返回
    /// - `Ok(())`：已重命名并重写索引
    /// - `Err(VeilError)`：找不到源文件，或目标路径已存在
    pub fn rename_file(&mut self, from: &str, to: &str) -> Result<()> {
        if index::get_file(&self.root, to).is_some() {
            return Err(VeilError::Format(format!("目标路径已存在: {to}")));
        }
        let mut meta = index::remove_file(&mut self.root, from)
            .ok_or_else(|| VeilError::Format(format!("找不到文件: {from}")))?;
        meta.mime = crate::mime::guess_mime(to); // 扩展名可能变，重新识别
        index::insert_file(&mut self.root, to, meta);
        self.commit()?;
        Ok(())
    }

    /// 递归添加磁盘目录 `src_dir` 下的所有文件到容器，前缀 `dest_prefix`。
    ///
    /// 使用流式处理，每个文件边读边加密，内存占用恒定。批量写入所有 blob 后只提交一次
    /// Index，避免重复重写索引。
    ///
    /// # 参数
    /// - `src_dir`:     磁盘上的源目录
    /// - `dest_prefix`: 容器内的目标前缀（空串 = 放到根）
    /// # 返回
    /// - `Ok(())`：目录下所有文件已加入
    /// - `Err(VeilError)`：读目录/文件或加密失败
    pub fn add_dir(&mut self, src_dir: impl AsRef<Path>, dest_prefix: &str) -> Result<()> {
        self.add_dir_with_progress(src_dir, dest_prefix, |_, _, _, _| {})
    }

    /// 添加目录，带进度回调。
    ///
    /// 回调参数：`(已完成文件数, 总文件数, 当前文件路径, 当前文件大小)`
    pub fn add_dir_with_progress<F>(
        &mut self,
        src_dir: impl AsRef<Path>,
        dest_prefix: &str,
        mut progress_callback: F,
    ) -> Result<()>
    where
        F: FnMut(usize, usize, &Path, u64),
    {
        let src_dir = src_dir.as_ref();
        let mut files = Vec::new();
        collect_files(src_dir, src_dir, dest_prefix, &mut files)?;

        let total_files = files.len();

        // 空目录也调用一次回调，让上层知道总数为 0
        if total_files == 0 {
            return Ok(());
        }

        // 批量：所有 blob 追加到末尾（逐个流式读取），**只提交一次 Index**。
        // 相比逐个 add_file，把 O(N²) 的 Index 重写降为 O(N)、2N 次 fsync 降为 2 次。
        {
            let mut file = OpenOptions::new().read(true).write(true).open(&self.path)?;
            let mut offset = file.seek(SeekFrom::End(0))?;
            for (index, (abs_path, virtual_path)) in files.into_iter().enumerate() {
                let file_size = abs_path.metadata()?.len();
                progress_callback(index, total_files, &abs_path, file_size); // 开始处理当前文件
                self.stage_blob_streaming(&mut file, &mut offset, &virtual_path, File::open(&abs_path)?)?;
            }
            file.sync_all()?; // 所有 blob 一次性落盘（崩溃安全的前提）
        }
        self.commit()?; // Index + Footer 只提交一次
        Ok(())
    }

    /// 把一个 blob 追加到**已打开文件**的当前位置并插入目录树，**不 fsync、不提交**。
    ///
    /// 供 [`add_dir`](Self::add_dir) 等批量场景用；调用方负责最后统一 `sync_all` + `commit`。
    /// `offset` 传入该 blob 的起始偏移，返回时更新为下一个 blob 的偏移。
    fn stage_blob(
        &mut self,
        file: &mut File,
        offset: &mut u64,
        virtual_path: &str,
        plaintext: &[u8],
    ) -> Result<()> {
        let content_hash: [u8; 32] = blake3::hash(plaintext).into();
        let blob_cipher = encrypt_bytes(&self.key_pair.to_public(), plaintext)?;
        file.write_all(&blob_cipher)?;

        let meta = FileMeta {
            size: plaintext.len() as u64,
            blob_offset: *offset,
            blob_len: blob_cipher.len() as u64,
            content_hash,
            mime: crate::mime::guess_mime(virtual_path),
            mtime: None,
        };
        *offset += blob_cipher.len() as u64;
        index::insert_file(&mut self.root, virtual_path, meta);
        Ok(())
    }

    /// 流式版本的 `stage_blob`（供 `add_dir` 批量场景用）。
    fn stage_blob_streaming(
        &mut self,
        file: &mut File,
        offset: &mut u64,
        virtual_path: &str,
        mut reader: impl Read,
    ) -> Result<()> {
        const CHUNK_SIZE: usize = 64 * 1024;

        let blob_offset = *offset;

        // age 流式加密器
        let recipient = self.key_pair.to_public();
        let encryptor = age::Encryptor::with_recipients(vec![Box::new(recipient) as Box<dyn age::Recipient>].iter().map(|r| r.as_ref()))
            .expect("failed to create encryptor");
        let mut writer = encryptor.wrap_output(&mut *file)?;

        // 边读边 hash 边加密
        let mut hasher = blake3::Hasher::new();
        let mut total_size = 0u64;
        let mut buffer = vec![0u8; CHUNK_SIZE];

        loop {
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
            writer.write_all(&buffer[..n])?;
            total_size += n as u64;
        }

        writer.finish()?;
        let blob_len = file.stream_position()? - blob_offset;
        *offset = file.stream_position()?;

        let content_hash: [u8; 32] = hasher.finalize().into();

        let meta = FileMeta {
            size: total_size,
            blob_offset,
            blob_len,
            content_hash,
            mime: crate::mime::guess_mime(virtual_path),
            mtime: None,
        };

        index::insert_file(&mut self.root, virtual_path, meta);
        Ok(())
    }

    /// 读出某个文件的完整明文，并做 blake3 往返校验。**流式解密、只碰这一个 blob。**
    ///
    /// 适用于**小文件**或需要全部内容的场景。大文件请用 [`open_file_reader`](Self::open_file_reader) 流式读取。
    ///
    /// # 参数
    /// - `virtual_path`: 要读取的文件路径
    /// # 返回
    /// - `Ok(Vec<u8>)`：文件明文
    /// - `Err(VeilError)`：找不到文件、解密失败或校验不一致
    pub fn read_file(&self, virtual_path: &str) -> Result<Vec<u8>> {
        let meta = self.file_meta(virtual_path)?;

        let mut reader = self.open_blob_reader(meta)?;
        let mut plaintext = Vec::new();
        reader.read_to_end(&mut plaintext)?;

        let hash: [u8; 32] = blake3::hash(&plaintext).into();
        if hash != meta.content_hash {
            return Err(VeilError::Format("内容哈希不匹配（数据损坏？）".into()));
        }
        Ok(plaintext)
    }

    /// 打开文件的流式读取器（适用于大文件，不会一次性读入内存）。
    ///
    /// 返回实现了 `Read + Seek` 的解密流，可以按需读取任意大小的数据块。得益于 age
    /// 使用**流密码**（ChaCha20-Poly1305），解密时的读取块大小可以与加密时完全不同。
    /// 流密码生成连续的密文字节流，解密时只需按顺序读取并与密钥流 XOR，无论一次读
    /// 1 字节还是 1MB 都能正确还原明文。
    ///
    /// # 参数
    /// - `virtual_path`: 要读取的文件路径
    /// # 返回
    /// - `Ok(impl Read + Seek)`：流式解密读取器
    /// - `Err(VeilError)`：找不到文件或打开失败
    ///
    /// # 示例
    /// ```no_run
    /// # use veil_core::container::Container;
    /// # use std::io::{Read, copy};
    /// # use std::fs::File;
    /// # fn example() -> veil_core::error::Result<()> {
    /// let container = Container::open("data.veil", "password")?;
    /// let mut reader = container.open_file_reader("videos/vacation.mp4")?;
    /// let mut output = File::create("vacation.mp4")?;
    /// std::io::copy(&mut reader, &mut output)?;  // 流式复制，内存占用恒定
    /// # Ok(())
    /// # }
    /// ```
    pub fn open_file_reader(
        &self,
        virtual_path: &str,
    ) -> Result<impl Read + Seek> {
        let meta = self.file_meta(virtual_path)?;
        self.open_blob_reader(meta)
    }

    /// 随机读取某文件解密后的 `[offset, offset+len)` 一段（不读整文件）。
    ///
    /// # 参数
    /// - `virtual_path`: 文件路径
    /// - `offset`:       从明文第几字节开始
    /// - `len`:          最多读多少字节
    /// # 返回
    /// - `Ok(Vec<u8>)`：读到的字节（末尾不足 `len` 时返回实际读到的）
    /// - `Err(VeilError)`：找不到文件或解密失败
    pub fn read_range(&self, virtual_path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let meta = self.file_meta(virtual_path)?;
        let mut reader = self.open_blob_reader(meta)?;
        reader.seek(SeekFrom::Start(offset))?;
        let mut buf = Vec::new();
        reader.take(len as u64).read_to_end(&mut buf)?;
        Ok(buf)
    }

    /// 把某文件解密到一个受控临时位置（优先 RAM 盘），返回 RAII 守卫。
    /// 用于视频/音频 V1：守卫 Drop 时临时明文自动删除。
    ///
    /// # 参数
    /// - `virtual_path`: 文件路径
    /// # 返回
    /// - `Ok(TempPlaintext)`：临时明文守卫
    /// - `Err(VeilError)`：找不到文件或解密/写入失败
    pub fn extract_to_temp(&self, virtual_path: &str) -> Result<crate::temp::TempPlaintext> {
        let meta = self.file_meta(virtual_path)?;
        let reader = self.open_blob_reader(meta)?;
        Ok(crate::temp::decrypt_to_temp(virtual_path, reader)?)
    }

    /// 按虚拟路径解密**单个**文件，写到磁盘 `dest`。使用流式处理，内存占用恒定。
    ///
    /// # 参数
    /// - `virtual_path`: 容器内要解密的文件路径
    /// - `dest`:         输出文件路径（父目录不存在会自动创建）
    /// # 返回
    /// - `Ok(())`：文件已解密并写入 `dest`
    /// - `Err(VeilError)`：找不到文件、校验失败或写盘失败
    pub fn extract_file(&self, virtual_path: &str, dest: impl AsRef<Path>) -> Result<()> {
        self.extract_file_with_progress(virtual_path, dest, |_, _| {})
    }

    /// 按虚拟路径流式解密**单个**文件到 `dest`，边导出边回调进度。
    ///
    /// 回调参数为 `(已导出字节数, 文件总字节数)`，每读完一块调用一次，
    /// 供进度条等 UI 实时刷新。其余行为与 [`extract_file`](Self::extract_file) 一致。
    pub fn extract_file_with_progress<F>(
        &self,
        virtual_path: &str,
        dest: impl AsRef<Path>,
        mut progress_callback: F,
    ) -> Result<()>
    where
        F: FnMut(u64, u64),
    {
        use std::io::BufWriter;

        let dest = dest.as_ref();
        let meta = self.file_meta(virtual_path)?;
        let total_size = meta.size;

        if let Some(parent) = dest.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        // 流式解密并写入文件
        let mut reader = self.open_blob_reader(meta)?;
        let mut writer = BufWriter::new(File::create(dest)?);

        // 边读边校验 hash
        let mut hasher = blake3::Hasher::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut exported = 0u64;

        loop {
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
            writer.write_all(&buffer[..n])?;
            exported += n as u64;
            progress_callback(exported, total_size);
        }

        writer.flush()?;

        // 校验完整性
        let hash: [u8; 32] = hasher.finalize().into();
        if hash != meta.content_hash {
            std::fs::remove_file(dest)?; // 校验失败，删除损坏文件
            return Err(VeilError::Format("内容哈希不匹配（数据损坏？）".into()));
        }

        Ok(())
    }

    /// 把某个虚拟目录（前缀）下的所有文件解密导出到 `out_dir`，保留相对结构。
    ///
    /// # 参数
    /// - `virtual_prefix`: 目录前缀（如 "photos"）
    /// - `out_dir`:        导出目标目录
    /// # 返回
    /// - `Ok(())`：该目录下所有文件已导出
    /// - `Err(VeilError)`：某文件校验失败或写盘失败
    pub fn extract_dir(&self, virtual_prefix: &str, out_dir: impl AsRef<Path>) -> Result<()> {
        let out_dir = out_dir.as_ref();
        let prefix = format!("{}/", virtual_prefix.trim_end_matches('/'));

        for (path, _meta) in index::list_files(&self.root) {
            if !path.starts_with(&prefix) {
                continue;
            }
            let rel = path.strip_prefix(&prefix).unwrap_or(&path);
            let dest = out_dir.join(rel);
            self.extract_file(&path, dest)?;
        }
        Ok(())
    }

    /// 把容器里所有文件解密导出到 `out_dir`，重建目录结构。
    ///
    /// # 参数
    /// - `out_dir`: 导出目标目录
    /// # 返回
    /// - `Ok(())`：所有文件已导出
    /// - `Err(VeilError)`：某文件校验失败或写盘失败
    pub fn extract_all(&self, out_dir: impl AsRef<Path>) -> Result<()> {
        let out_dir = out_dir.as_ref();
        for (path, _meta) in index::list_files(&self.root) {
            let dest = out_dir.join(&path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let plaintext = self.read_file(&path)?;
            std::fs::write(&dest, plaintext)?;
        }
        Ok(())
    }

    /// 目录树的根（只读借用）——渲染层直接遍历它来展示/浏览。
    pub fn root(&self) -> &Tree {
        &self.root
    }

    /// 按路径查一个文件的元数据（找不到 / 是目录 → None）。
    pub fn get_file(&self, virtual_path: &str) -> Option<&FileMeta> {
        index::get_file(&self.root, virtual_path)
    }

    /// 获取容器创建时使用的 CLI 版本。
    pub fn cli_version(&self) -> &str {
        &self.cli_version
    }

    /// 查找匹配通配符模式的文件路径列表。
    ///
    /// 支持 `*`（不匹配 `/`）和 `**`（匹配任意层级）。
    ///
    /// # 示例
    /// ```ignore
    /// let paths = container.find_files("**/*.jpg")?;
    /// ```
    pub fn find_files(&self, pattern: &str) -> Result<Vec<String>> {
        let matched = index::match_files(&self.root, pattern)?;
        Ok(matched.into_iter().map(|(path, _)| path).collect())
    }

    /// 删除匹配通配符模式的所有文件，返回被删除的文件路径列表。
    ///
    /// # 示例
    /// ```ignore
    /// let deleted = container.remove_matched("temp/*")?;
    /// ```
    pub fn remove_matched(&mut self, pattern: &str) -> Result<Vec<String>> {
        let matched = index::match_files(&self.root, pattern)?;
        let paths: Vec<String> = matched.iter().map(|(p, _)| p.clone()).collect();

        for path in &paths {
            index::remove_file(&mut self.root, path);
        }

        if !paths.is_empty() {
            self.commit()?;
        }

        Ok(paths)
    }

    /// 导出匹配通配符模式的所有文件到指定目录。
    ///
    /// # 示例
    /// ```ignore
    /// container.extract_matched("photos/**/*.jpg", "./output")?;
    /// ```
    pub fn extract_matched(&self, pattern: &str, out_dir: impl AsRef<Path>) -> Result<()> {
        let matched = index::match_files(&self.root, pattern)?;
        let out_dir = out_dir.as_ref();

        for (virtual_path, _) in matched {
            let dest = out_dir.join(&virtual_path);
            self.extract_file(&virtual_path, dest)?;
        }

        Ok(())
    }

    /// 把目录树渲染成多行字符串（类似 `tree` 命令）。
    pub fn tree_view(&self) -> String {
        let mut out = String::new();
        render_tree(&self.root, "", &mut out);
        out
    }

    /// 按路径查文件元数据，找不到 → 报错。
    fn file_meta(&self, virtual_path: &str) -> Result<&FileMeta> {
        index::get_file(&self.root, virtual_path)
            .ok_or_else(|| VeilError::Format(format!("找不到文件: {virtual_path}")))
    }

    /// 打开一个「只解密该 blob」的流式解密读取器（可 Seek）。**完全不碰其他 blob。**
    fn open_blob_reader(
        &self,
        meta: &FileMeta,
    ) -> Result<age::stream::StreamReader<SliceReader<File>>> {
        let file = File::open(&self.path)?;
        let slice = SliceReader::new(file, meta.blob_offset, meta.blob_len)?;
        let decryptor = age::Decryptor::new(slice)?;
        let reader = decryptor.decrypt(std::iter::once(&self.key_pair as &dyn age::Identity))?;
        Ok(reader)
    }

    /// 提交当前目录树：把新的 Index + Footer **追加到文件末尾**并 fsync。
    ///
    /// 追加式（不覆盖已提交数据）+ Footer 最后写 + fsync = 崩溃安全：崩溃只会在末尾
    /// 留下未提交的垃圾（下次 open 时被恢复截断），**已提交的数据永不被破坏**。
    /// 代价：旧的 Index/Footer 成为死空间，P6 的 compaction 回收。
    fn commit(&self) -> Result<()> {
        let index_plain = serialize_index(&self.root)?;
        let index_cipher = encrypt_bytes(&self.key_pair.to_public(), &index_plain)?;

        let mut file = OpenOptions::new().read(true).write(true).open(&self.path)?;
        let index_offset = file.seek(SeekFrom::End(0))?; // 追加在末尾
        file.write_all(&index_cipher)?;
        format::write_footer(&mut file, index_offset, index_cipher.len() as u64)?;
        file.sync_all()?; // fsync：这是「提交点」
        Ok(())
    }

    /// 校验容器内所有文件的完整性（age 认证 + blake3 往返）。
    ///
    /// # 返回
    /// **损坏文件的路径列表**（空 = 全部完好）。
    pub fn verify_all(&self) -> Vec<String> {
        index::list_files(&self.root)
            .into_iter()
            .filter(|(path, _)| self.read_file(path).is_err())
            .map(|(path, _)| path)
            .collect()
    }
}

/// 打开时加载 Index：先试文件尾的 Footer（正常）；若无效（崩溃留下的尾部垃圾），
/// 向前分块扫描上一个有效 Footer，截断尾部垃圾后返回（崩溃恢复）。
fn recover_index(path: &Path, key_pair: &age::x25519::Identity) -> Result<Tree> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let file_len = file.seek(SeekFrom::End(0))?;

    // 快路径：文件尾正好是有效 Footer
    if let Some(tree) = try_footer_end(&mut file, file_len, key_pair)? {
        return Ok(tree);
    }

    // 崩溃恢复：从末尾向前分块搜 Footer 魔数，找第一个能解密的 Footer
    const CHUNK: u64 = 1 << 20; // 1 MiB
    let magic_len = format::FOOTER_MAGIC.len();
    let mut window_end = file_len;
    loop {
        let window_start = window_end.saturating_sub(CHUNK);
        file.seek(SeekFrom::Start(window_start))?;
        let mut buf = vec![0u8; (window_end - window_start) as usize];
        file.read_exact(&mut buf)?;

        // 从窗口尾向前找魔数
        if buf.len() >= magic_len {
            for i in (0..=buf.len() - magic_len).rev() {
                if &buf[i..i + magic_len] == format::FOOTER_MAGIC {
                    let footer_end = window_start + i as u64 + magic_len as u64;
                    if let Some(tree) = try_footer_end(&mut file, footer_end, key_pair)? {
                        file.set_len(footer_end)?; // 截断尾部垃圾
                        file.sync_all()?;
                        return Ok(tree);
                    }
                }
            }
        }
        if window_start == 0 {
            break;
        }
        // 重叠 magic_len-1 字节，防魔数跨窗口边界漏掉
        window_end = window_start + magic_len as u64 - 1;
    }
    Err(VeilError::Format("找不到有效的 Footer（文件损坏）".into()))
}

/// 尝试把 `footer_end` 当作某个 Footer 的结束位置来加载 Index：校验魔数 + 自洽 + 能解密。
fn try_footer_end(
    file: &mut File,
    footer_end: u64,
    key_pair: &age::x25519::Identity,
) -> Result<Option<Tree>> {
    if footer_end < format::FOOTER_LEN {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(footer_end - format::FOOTER_LEN))?;
    let mut buf = [0u8; format::FOOTER_LEN as usize];
    if file.read_exact(&mut buf).is_err() {
        return Ok(None);
    }
    if &buf[16..24] != format::FOOTER_MAGIC {
        return Ok(None);
    }
    let index_offset = u64::from_le_bytes(buf[0..8].try_into().unwrap());
    let index_len = u64::from_le_bytes(buf[8..16].try_into().unwrap());
    // 自洽：index_offset + index_len + FOOTER_LEN 应正好等于 footer_end
    if index_offset
        .checked_add(index_len)
        .and_then(|s| s.checked_add(format::FOOTER_LEN))
        != Some(footer_end)
    {
        return Ok(None);
    }

    // 读并解密 Index（能解密才算真有效，进一步排除魔数误匹配）
    file.seek(SeekFrom::Start(index_offset))?;
    let mut cipher = vec![0u8; index_len as usize];
    if file.read_exact(&mut cipher).is_err() {
        return Ok(None);
    }
    match decrypt_bytes(key_pair, &cipher).and_then(|p| deserialize_index(&p)) {
        Ok(tree) => Ok(Some(tree)),
        Err(_) => Ok(None),
    }
}

/// 递归收集 `dir` 下的所有文件，算出各自在容器里的虚拟路径（供 [`Container::add_dir`] 用）。
fn collect_files(
    base: &Path,
    dir: &Path,
    dest_prefix: &str,
    out: &mut Vec<(PathBuf, String)>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();

        // 获取元数据，不跟随符号链接
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue, // 跳过无法访问的文件
        };

        if metadata.is_symlink() {
            // 跳过符号链接，避免重复计数和循环引用
            continue;
        } else if metadata.is_dir() {
            collect_files(base, &path, dest_prefix, out)?;
        } else if metadata.is_file() {
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

/// 把嵌套目录树渲染成 tree 风格的多行文本。
fn render_tree(dir: &Tree, prefix: &str, out: &mut String) {
    let count = dir.len();
    for (i, (name, node)) in dir.iter().enumerate() {
        let is_last = i == count - 1;
        let branch = if is_last { "└── " } else { "├── " };
        let slash = if matches!(node, Node::Dir(_)) { "/" } else { "" };
        out.push_str(&format!("{prefix}{branch}{name}{slash}\n"));
        if let Node::Dir(children) = node {
            let child_prefix = format!("{prefix}{}", if is_last { "    " } else { "│   " });
            render_tree(children, &child_prefix, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_path(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("veil_test_{}_{n}_{tag}", std::process::id()))
    }

    fn pass() -> SecretString {
        SecretString::from("correct horse".to_owned())
    }

    /// 测试用 CLI 版本
    fn test_cli_version() -> &'static str {
        "1.1.0-test"
    }

    /// 容器里的文件总数（辅助断言）。
    fn file_count(c: &Container) -> usize {
        index::list_files(c.root()).len()
    }

    #[test]
    fn create_open_empty() {
        let path = temp_path("empty.veil");
        Container::create(&path, pass(), test_cli_version()).unwrap();
        let container = Container::open(&path, pass()).unwrap();
        assert!(container.root().is_empty());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn add_read_roundtrip() {
        let path = temp_path("rt.veil");
        let mut container = Container::create(&path, pass(), test_cli_version()).unwrap();
        container.add_file("a.txt", b"hello").unwrap();
        container.add_file("dir/b.bin", &[0u8, 1, 2, 255]).unwrap();

        assert_eq!(container.read_file("a.txt").unwrap(), b"hello");

        let reopened = Container::open(&path, pass()).unwrap();
        assert_eq!(file_count(&reopened), 2);
        assert_eq!(reopened.read_file("dir/b.bin").unwrap(), vec![0u8, 1, 2, 255]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn wrong_passphrase_fails() {
        let path = temp_path("wp.veil");
        Container::create(&path, pass(), test_cli_version()).unwrap();
        assert!(Container::open(&path, SecretString::from("nope".to_owned())).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn remove_then_missing() {
        let path = temp_path("rm.veil");
        let mut container = Container::create(&path, pass(), test_cli_version()).unwrap();
        container.add_file("keep.txt", b"1").unwrap();
        container.add_file("gone.txt", b"2").unwrap();
        container.remove_file("gone.txt").unwrap();
        assert!(container.remove_file("nope.txt").is_err());

        let reopened = Container::open(&path, pass()).unwrap();
        assert_eq!(file_count(&reopened), 1);
        assert_eq!(reopened.read_file("keep.txt").unwrap(), b"1");
        assert!(reopened.read_file("gone.txt").is_err());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_missing_file_errors() {
        let path = temp_path("miss.veil");
        let container = Container::create(&path, pass(), test_cli_version()).unwrap();
        assert!(container.read_file("nope.txt").is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn add_file_fills_mime() {
        let path = temp_path("mime.veil");
        let mut container = Container::create(&path, pass(), test_cli_version()).unwrap();
        container.add_file("a.jpg", b"x").unwrap();
        container.add_file("notes", b"y").unwrap();

        let reopened = Container::open(&path, pass()).unwrap();
        assert_eq!(reopened.get_file("a.jpg").unwrap().mime.as_deref(), Some("image/jpeg"));
        assert_eq!(reopened.get_file("notes").unwrap().mime, None);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn add_file_overwrites_same_path() {
        let path = temp_path("overwrite.veil");
        let mut container = Container::create(&path, pass(), test_cli_version()).unwrap();
        container.add_file("a.txt", b"first").unwrap();
        container.add_file("a.txt", b"second-longer").unwrap();

        assert_eq!(file_count(&container), 1);
        assert_eq!(container.read_file("a.txt").unwrap(), b"second-longer");

        let reopened = Container::open(&path, pass()).unwrap();
        assert_eq!(file_count(&reopened), 1);
        assert_eq!(reopened.read_file("a.txt").unwrap(), b"second-longer");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn change_password_works() {
        let path = temp_path("chpw.veil");
        let mut container = Container::create(&path, "old-pass".to_string(), test_cli_version()).unwrap();
        container.add_file("f.txt", b"data").unwrap();
        container.change_password("new-pass".to_string()).unwrap();

        assert!(Container::open(&path, "old-pass".to_string()).is_err());
        let reopened = Container::open(&path, "new-pass".to_string()).unwrap();
        assert_eq!(reopened.read_file("f.txt").unwrap(), b"data");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rename_file_works() {
        let path = temp_path("rename.veil");
        let mut container = Container::create(&path, pass(), test_cli_version()).unwrap();
        container.add_file("a.txt", b"hi").unwrap();
        container.rename_file("a.txt", "sub/b.md").unwrap();

        container.add_file("keep.txt", b"x").unwrap();
        assert!(container.rename_file("keep.txt", "sub/b.md").is_err());

        let reopened = Container::open(&path, pass()).unwrap();
        assert!(reopened.read_file("a.txt").is_err());
        assert_eq!(reopened.read_file("sub/b.md").unwrap(), b"hi");
        assert_eq!(reopened.get_file("sub/b.md").unwrap().mime.as_deref(), Some("text/markdown"));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn extract_dir_subtree() {
        let path = temp_path("exdir.veil");
        let mut container = Container::create(&path, pass(), test_cli_version()).unwrap();
        container.add_file("photos/2024/a.txt", b"A").unwrap();
        container.add_file("photos/b.txt", b"B").unwrap();
        container.add_file("docs/c.txt", b"C").unwrap();

        let out = temp_path("exdir_out");
        container.extract_dir("photos", &out).unwrap();
        assert_eq!(std::fs::read(out.join("2024/a.txt")).unwrap(), b"A");
        assert_eq!(std::fs::read(out.join("b.txt")).unwrap(), b"B");
        assert!(!out.join("c.txt").exists());

        std::fs::remove_dir_all(&out).ok();
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn add_dir_recursive() {
        let src = temp_path("srcdir");
        std::fs::create_dir_all(src.join("nested")).unwrap();
        std::fs::write(src.join("top.txt"), b"top").unwrap();
        std::fs::write(src.join("nested/deep.bin"), b"deep").unwrap();

        let path = temp_path("adddir.veil");
        let mut container = Container::create(&path, pass(), test_cli_version()).unwrap();
        container.add_dir(&src, "imported").unwrap();

        let reopened = Container::open(&path, pass()).unwrap();
        assert_eq!(reopened.read_file("imported/top.txt").unwrap(), b"top");
        assert_eq!(reopened.read_file("imported/nested/deep.bin").unwrap(), b"deep");

        std::fs::remove_dir_all(&src).ok();
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn extract_to_temp_and_auto_cleanup() {
        let path = temp_path("tmp.veil");
        let mut container = Container::create(&path, pass(), test_cli_version()).unwrap();
        container.add_file("clip.mp4", b"fake video bytes").unwrap();

        let temp_file_path;
        {
            let tmp = container.extract_to_temp("clip.mp4").unwrap();
            temp_file_path = tmp.path().to_path_buf();
            assert!(temp_file_path.exists());
            assert_eq!(std::fs::read(&temp_file_path).unwrap(), b"fake video bytes");
            assert_eq!(temp_file_path.extension().unwrap(), "mp4");
        }
        assert!(!temp_file_path.exists(), "Drop 后临时明文应被删除");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_range_random_access() {
        let path = temp_path("range.veil");
        let mut container = Container::create(&path, pass(), test_cli_version()).unwrap();
        container.add_file("data.bin", b"0123456789ABCDEF").unwrap();

        let reopened = Container::open(&path, pass()).unwrap();
        assert_eq!(reopened.read_range("data.bin", 4, 5).unwrap(), b"45678");
        assert_eq!(reopened.read_range("data.bin", 14, 10).unwrap(), b"EF");
        assert_eq!(reopened.read_file("data.bin").unwrap(), b"0123456789ABCDEF");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn crash_recovery_truncates_garbage() {
        let path = temp_path("crash.veil");
        let mut container = Container::create(&path, pass(), test_cli_version()).unwrap();
        container.add_file("a.txt", b"hello").unwrap();
        container.add_file("b.txt", b"world").unwrap();
        drop(container);

        // 模拟崩溃：在文件末尾追加一段"未提交的垃圾"（写 blob/Index 写到一半崩了）
        {
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b"garbage from a crashed half-written blob append........")
                .unwrap();
        }

        // open 应自动恢复到上一个有效 Footer，内容完好
        let reopened = Container::open(&path, pass()).unwrap();
        assert_eq!(reopened.read_file("a.txt").unwrap(), b"hello");
        assert_eq!(reopened.read_file("b.txt").unwrap(), b"world");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn verify_all_detects_corruption() {
        let path = temp_path("verify.veil");
        let mut container = Container::create(&path, pass(), test_cli_version()).unwrap();
        container.add_file("good.txt", b"ok").unwrap();
        container.add_file("bad.txt", b"will be tampered").unwrap();
        assert!(container.verify_all().is_empty()); // 全好

        // 篡改 bad.txt 的 blob 中间一字节
        let meta = container.get_file("bad.txt").unwrap();
        let flip_at = meta.blob_offset + meta.blob_len / 2;
        {
            let mut f = OpenOptions::new().read(true).write(true).open(&path).unwrap();
            f.seek(SeekFrom::Start(flip_at)).unwrap();
            let mut b = [0u8; 1];
            f.read_exact(&mut b).unwrap();
            b[0] ^= 0xFF;
            f.seek(SeekFrom::Start(flip_at)).unwrap();
            f.write_all(&b).unwrap();
        }

        let reopened = Container::open(&path, pass()).unwrap();
        assert_eq!(reopened.verify_all(), vec!["bad.txt".to_owned()]);
        assert_eq!(reopened.read_file("good.txt").unwrap(), b"ok"); // 好文件不受影响

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn tamper_is_detected() {
        let path = temp_path("tamper.veil");
        let mut container = Container::create(&path, pass(), test_cli_version()).unwrap();
        container.add_file("secret.txt", b"top secret content").unwrap();

        let meta = container.get_file("secret.txt").unwrap();
        let flip_at = meta.blob_offset + meta.blob_len / 2;

        {
            let mut file = OpenOptions::new().read(true).write(true).open(&path).unwrap();
            file.seek(SeekFrom::Start(flip_at)).unwrap();
            let mut byte = [0u8; 1];
            file.read_exact(&mut byte).unwrap();
            byte[0] ^= 0xFF;
            file.seek(SeekFrom::Start(flip_at)).unwrap();
            file.write_all(&byte).unwrap();
        }

        let reopened = Container::open(&path, pass()).unwrap();
        assert!(reopened.read_file("secret.txt").is_err());

        std::fs::remove_file(&path).ok();
    }
}
