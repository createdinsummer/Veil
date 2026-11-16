//! Veil 文件操作的统一入口。
//!
//! 上层文件流程不得直接读取或写入 `.enc`，所有文件内容的读取、写入、解密导出和
//! 密钥轮换重写都应经过本模块。`.enc` 文件由固定大小的独立 AEAD 分块依次拼接，
//! 每个分块单独认证，因此内存占用只与分块大小有关，不随文件总大小增长。
//!
//! 后续增加文件级校验、转换、复制或读写组合时，也应优先在本模块实现并复用这里
//! 的分块格式与原子发布逻辑。
//!
//! 所属业务模块：veil-core 的文件加密层。数据库只存目录树、元数据、密钥和 nonce，
//! 文件内容一律以 .enc 密文独立落盘；本模块是 .enc 读写的唯一入口。
//!
//! 文件内核心清单：
//! - 常量：`DATA_KEY_LEN`(32 字节主密钥)、`NONCE_LEN`(12 字节)、`CHUNK_SIZE`
//!   (1 MiB 分块)、`TAG_LEN`(16 字节认证标签)。
//! - 加密入口：`encrypt_file_streaming`/`encrypt_staged_file(_with_hash)`。
//! - 解密入口：`decrypt_file_streaming(_without_overwrite)`/`decrypt_file_to_writer`/
//!   `DecryptedFileReader`（流式读认证明文）。
//! - 密钥轮换：`rewrite_file_streaming`/`rewrite_staged_file`（旧密钥解 → 新密钥加）。
//! - 探测：`detect_key_index`（首个分块判断文件属于哪把数据主密钥）。
//! - 摘要：`plaintext_digest`（认证后流式算明文 BLAKE3，判断覆盖是否真的改了内容）。
//!
//! 磁盘格式（业务约定）：文件 = 依次拼接的分块密文；每块密文 = 明文(≤1 MiB) +
//! 16 字节认证标签；分块 nonce = BLAKE3(base_nonce ‖ 块序号大端 32 位) 的前 12 字节；
//! 分块 AAD = b"VEILCHNK" + 块序号大端 32 位（绑定位置，防重排/替换攻击）。
//!
//! 依赖能力：
//! - `chacha20poly1305`：AEAD 分块加解密。
//! - `blake3`：nonce 派生与明文摘要。
//! - `fsutil`：原子发布、排他重命名、目录持久化。
//! - `progress`：进度统计与取消检查。
//!
//! 整体业务目标：内存占用恒为 O(分块大小)，大文件不整读进内存；写入先落临时文件
//! 再发布（新 .enc 拒绝覆盖旧文件）；数据库引用的提交和崩溃恢复还须由 service 事务流程协调。

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use chacha20poly1305::{
    ChaCha20Poly1305,
    aead::{Aead, KeyInit, Payload},
};

use crate::error::{Result, VeilError};

/// Veil 数据主密钥长度，单位为字节。
pub const DATA_KEY_LEN: usize = 32;

/// Veil 文件分块 nonce 长度，单位为字节。
pub const NONCE_LEN: usize = 12;

/// 单个分块的明文大小，单位为字节。
pub const CHUNK_SIZE: usize = 1024 * 1024;

/// ChaCha20-Poly1305 认证标签长度，单位为字节。
pub const TAG_LEN: usize = 16;

/// 分块 AAD 前缀，用于区分普通文件分块和未来其他 AEAD 用途。
///
/// AAD（附加认证数据）= 加密时不进密文、但参与认证标签计算的数据；这里把
/// 固定前缀 + 块序号绑进认证，防止攻击者重排/替换文件内的分块而不被发现。
const CHUNK_AAD_PREFIX: &[u8; 8] = b"VEILCHNK";

/// 使用系统随机源生成文件级 base nonce。
///
/// 该值不直接用相同内容重复加密；真正的分块 nonce 还会混入块序号。
///
/// 功能：生成 12 字节随机基础 nonce，存入数据库的 `file_info.nonce`；同一文件的
/// 所有分块 nonce 都由它派生。相同内容用不同 base nonce 加密会得到不同密文。
///
/// 返回值：`[u8; 12]` 随机 nonce。
///
/// 注意事项：系统随机源失败会 panic——没有安全随机数就不该继续加密业务。
/// 入参：无。
/// 异常：系统随机源失败时 expect 触发 panic，不使用固定或弱随机回退值。
/// 注意事项：随机 nonce 不等于保密数据，必须与密文对应保存；本函数不自动写入数据库。
pub fn generate_base_nonce() -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce).expect("生成文件 nonce 失败");
    nonce
}

/// 将明文文件流式加密为 Veil 的 `.enc` 文件。
///
/// 输入是明文源文件，输出是分块 ChaCha20-Poly1305 密文文件。目标通过同目录
/// 临时文件原子发布，目标已存在时拒绝覆盖。返回源文件的实际明文大小。
///
/// 功能：普通写入/覆盖流程的第一阶段——把明文源文件加密成全新随机命名的密文，
/// 供数据库事务提交引用；目标存在时拒绝覆盖，防止误毁旧密文。
///
/// 入参：
/// - `source_path`：明文源文件路径。
/// - `destination_path`：目标 .enc 路径（必须尚不存在）。
/// - `data_key`：当前活动文件数据主密钥（32 字节）。
/// - `base_nonce`：本次加密的 12 字节基础 nonce（新文件每次随机生成）。
///
/// 返回值：`Ok(u64)` 源文件明文大小（数据库 `file_info.size` 的权威值）。
///
/// 注意事项：进度与取消由 `progress` 全局状态驱动；取消时目标临时文件不发布。
/// 异常：源读取、临时文件、同步或发布失败返回 I/O 错误；加密失败返回 EncryptionError；
/// 大小超出分块限制或源实际长度不符返回 InvalidFormat，取消返回 Cancelled。
/// 注意事项：本方法不生成随机文件名，destination_path 由调用方提供；返回大小来自打开后读取的文件元数据。
pub fn encrypt_file_streaming(
    source_path: &Path,
    destination_path: &Path,
    data_key: &[u8; DATA_KEY_LEN],
    base_nonce: &[u8; NONCE_LEN],
) -> Result<u64> {
    encrypt_file_streaming_impl(source_path, destination_path, data_key, base_nonce, false)
}

/// 写入仅由容器事务引用的密文；目标在元数据提交前不得供读取者使用。
///
/// 功能：与 `encrypt_file_streaming` 相同的加密流程，但目标发布到暂存区
/// （.staging/<lock_id>/），发布允许"排他重命名或排他创建再复制"两种方式；
/// 只有数据库提交引用后才对读取者可见。
/// 入参：source_path/destination_path: &Path 为明文源和未被引用的暂存密文路径；
/// data_key: &[u8; 32] 为活动文件密钥；base_nonce: &[u8; 12] 为本次新生成的基础 nonce。
/// 返回值：Ok(u64) 为源文件声明并完整加密的明文字节数。
/// 异常：读取、临时写入、同步、发布、加密、长度校验或取消错误通过 VeilError 返回。
/// 注意事项：函数不插入数据库引用；暂存文件在磁盘上存在，业务不可见性由调用方事务约定保证。
pub fn encrypt_staged_file(
    source_path: &Path,
    destination_path: &Path,
    data_key: &[u8; DATA_KEY_LEN],
    base_nonce: &[u8; NONCE_LEN],
) -> Result<u64> {
    encrypt_file_streaming_impl(source_path, destination_path, data_key, base_nonce, true)
}

/// 在同一遍读取中计算实际加密明文的 BLAKE3 哈希。
///
/// 功能：加密的同时流式计算明文摘要，避免加密后再读一遍。摘要用于判断"覆盖
/// 是否真的改变了内容"——内容没变就不追加版本（见设计文档的版本链规则）。
///
/// 返回值：`Ok((明文大小, BLAKE3 摘要))`。
///
/// 注意事项：摘要是对"实际加密的明文"计算，读取失败即整体失败。
/// 入参：source_path/destination_path: &Path 为明文源与未被引用目标；
/// data_key: &[u8; 32] 为文件密钥，base_nonce: &[u8; 12] 为本次基础 nonce。
/// 异常：读取、写入、认证加密、长度检查、取消或发布失败返回 VeilError，不返回可用摘要。
/// 注意事项：返回的 blake3::Hash 描述加密过程中读到的字节，不冻结源文件；外部同长度修改仍可能被读到。
pub fn encrypt_staged_file_with_hash(
    source_path: &Path,
    destination_path: &Path,
    data_key: &[u8; DATA_KEY_LEN],
    base_nonce: &[u8; NONCE_LEN],
) -> Result<(u64, blake3::Hash)> {
    /// 加密输入的摘要包装器：拥有真实源流和累计 BLAKE3 状态，同一遍读取同时提供内容与摘要。
    /// 每轮只把实际读到的前缀送入摘要器，避免将缓冲未使用部分误计入文件指纹。
    struct HashReader<R> {
        /// 已打开的明文输入，数据来自调用方选择的源文件。
        inner: R,
        /// 按输入顺序累计实际读取字节的摘要状态，外层在加密结束后取最终指纹。
        hasher: blake3::Hasher,
    }
    impl<R: Read> Read for HashReader<R> {
        /// 功能：在一次加密读取中同步累计实际输入的明文摘要，避免再读一次源文件。
        /// 入参：self: &mut `HashReader<R>` 为真实输入与 BLAKE3 摘要状态；buf: &mut [u8] 为本次读取目标缓冲。
        /// 返回值：Ok(usize) 为真实读取字节数，只有 buf 的这一前缀参与哈希；0 表示本次无更多数据。
        /// 异常：底层 inner.read 的 io::Error 原样传播，失败时不把未读取缓冲误计入摘要。
        /// 注意事项：不 seek、不冻结源文件、不独立计数总量；每次成功读到的实际字节按先后顺序进入同一摘要器。
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let count = self.inner.read(buf)?;
            // 只哈希本次实际读到的字节，与加密流完全同步
            self.hasher.update(&buf[..count]);
            Ok(count)
        }
    }
    let file = File::open(source_path)?;
    let size = file.metadata()?.len();
    let mut source = HashReader {
        inner: file,
        hasher: blake3::Hasher::new(),
    };
    let mut output = create_atomic_output(destination_path)?;
    encrypt_reader(
        &mut source,
        output.as_file_mut(),
        data_key,
        base_nonce,
        size,
    )?;
    publish_atomic_output(output, destination_path, false, true)?;
    // 加密结束后的最终摘要；与大小一起返回给事务层
    Ok((size, source.hasher.finalize()))
}

/// 两个公开加密入口的公共实现。
///
/// 功能：打开源文件 → 建同目录临时输出 → 逐块加密 → 原子发布（暂存或普通）。
///
/// `staged` 为 true 表示目标是暂存区文件（允许排他创建+复制的发布路径），
/// false 表示普通 .enc 发布（目标必须尚不存在）。
/// 入参：source_path/destination_path: &Path 为输入与输出；data_key: &[u8; 32] 和 base_nonce: &[u8; 12] 为本次密钥上下文；
/// staged: bool 为 true 时允许暂存发布回退复制，false 使用禁止覆盖的普通发布。
/// 返回值：Ok(u64) 为源文件元数据声明并完整处理的字节数。
/// 异常：文件 I/O、分块格式、加密、发布或取消错误通过 VeilError 传播。
/// 注意事项：源长度在打开后读取一次，之后缩短或加长会在读取或末尾检查失败；不保证隔离同长度并发修改。
fn encrypt_file_streaming_impl(
    source_path: &Path,
    destination_path: &Path,
    data_key: &[u8; DATA_KEY_LEN],
    base_nonce: &[u8; NONCE_LEN],
    staged: bool,
) -> Result<u64> {
    let mut source = File::open(source_path)?;
    let plaintext_size = source.metadata()?.len();
    let mut output = create_atomic_output(destination_path)?;
    encrypt_reader(
        &mut source,
        output.as_file_mut(),
        data_key,
        base_nonce,
        plaintext_size,
    )?;
    publish_atomic_output(output, destination_path, false, staged)?;
    Ok(plaintext_size)
}

/// 将 Veil 的 `.enc` 文件流式解密到目标路径。
///
/// 目标允许被原子替换。源文件必须与元数据声明的 `plaintext_size` 和分块格式
/// 一致，否则不会发布输出文件。
///
/// 功能：解密导出到普通目标（可能覆盖旧导出文件）；先用临时文件写完并同步，
/// 认证全部通过才原子替换目标，认证失败时目标保持原样。
///
/// 入参：
/// - `source_path`：.enc 密文路径。
/// - `destination_path`：明文目标路径（允许替换已存在的文件）。
/// - `data_key`：加密时使用的数据主密钥。
/// - `base_nonce`：密文对应的基础 nonce。
/// - `plaintext_size`：数据库记录的明文大小——密文长度必须与它精确对应。
///
/// 返回值：`Ok(())` 解密并发布成功。
///
/// 注意事项：密文被篡改/截断/加长都会认证失败，且不会留下半截明文目标。
/// 异常：文件/写入/同步/发布失败返回 I/O 错误；错误密钥、nonce 或标签返回 DecryptionError；
/// 分块数量或尾部长度不符返回 InvalidFormat，取消返回 Cancelled。
/// 注意事项：“截断/加长”可能在 I/O 或长度检查阶段失败，不一定都表现为认证错误；新明文先写临时文件。
pub fn decrypt_file_streaming(
    source_path: &Path,
    destination_path: &Path,
    data_key: &[u8; DATA_KEY_LEN],
    base_nonce: &[u8; NONCE_LEN],
    plaintext_size: u64,
) -> Result<()> {
    let mut source = File::open(source_path)?;
    let mut output = create_atomic_output(destination_path)?;
    decrypt_reader(
        &mut source,
        output.as_file_mut(),
        data_key,
        base_nonce,
        plaintext_size,
    )?;
    publish_atomic_output(output, destination_path, true, false)
}

/// 解密到不存在的目标文件；目标已存在时拒绝覆盖。
///
/// 功能：与 `decrypt_file_streaming` 相同，但发布时使用排他语义——
/// 目标已存在即失败，不覆盖用户已有文件。
/// 入参：source_path/destination_path: &Path 为密文源与明文目标；data_key: &[u8; 32]、base_nonce: &[u8; 12] 必须对应源；
/// plaintext_size: u64 为容器数据库记录的完整明文大小，允许零字节文件。
/// 返回值：全部分块认证、长度检查及禁止覆盖发布成功时为 Ok(())。
/// 异常：目标已存在、I/O、认证、长度格式或取消失败均以 VeilError 返回。
/// 注意事项：目标存在检查在发布时生效，可能已经完成解密才发现冲突；不更新容器数据库。
pub fn decrypt_file_streaming_without_overwrite(
    source_path: &Path,
    destination_path: &Path,
    data_key: &[u8; DATA_KEY_LEN],
    base_nonce: &[u8; NONCE_LEN],
    plaintext_size: u64,
) -> Result<()> {
    let mut source = File::open(source_path)?;
    let mut output = create_atomic_output(destination_path)?;
    decrypt_reader(
        &mut source,
        output.as_file_mut(),
        data_key,
        base_nonce,
        plaintext_size,
    )?;
    publish_atomic_output(output, destination_path, false, false)
}

/// 将密文流式解密到调用方提供的写入器；调用方负责处理失败时的部分输出。
///
/// 功能：不落盘明文文件，直接把认证后的明文喂给任意写入器（如网络流、哈希器）。
/// 失败时可能已写出部分明文，由调用方决定如何丢弃。
///
/// 入参：`writer` 明文接收方；其余同 `decrypt_file_streaming`。
/// 返回值：所有分块认证、写入及最终精确结束检查成功时返回 Ok(())。
/// 异常：打开/读取/写入失败为 I/O 错误，认证失败为 DecryptionError，格式或尾部不符为 InvalidFormat，也可返回 Cancelled。
/// 注意事项：writer: &mut impl Write 可以是文件、网络或内存输出，并非天然不落盘；不自动回滚已经写出的认证明文。
pub fn decrypt_file_to_writer(
    source_path: &Path,
    writer: &mut impl Write,
    data_key: &[u8; DATA_KEY_LEN],
    base_nonce: &[u8; NONCE_LEN],
    plaintext_size: u64,
) -> Result<()> {
    let mut source = File::open(source_path)?;
    decrypt_reader(&mut source, writer, data_key, base_nonce, plaintext_size)
}

/// 流式读取经过认证的文件明文，不创建明文临时文件。
///
/// 业务定位：边读边解的 `Read` 实现——上层把它当普通文件流用；每读满一块就
/// 解密并认证下一块，明文只以一块大小的缓冲存在内存里。密钥和明文缓冲都用
/// zeroize 包装，析构时清零。
pub struct DecryptedFileReader {
    /// 密文源文件。
    source: File,
    /// 数据主密钥（Zeroizing：析构时内存清零）。
    key: zeroize::Zeroizing<[u8; DATA_KEY_LEN]>,
    /// 文件基础 nonce，派生出每个分块的 nonce。
    nonce: [u8; NONCE_LEN],
    /// 元数据声明的明文总大小：决定分块数量与末块长度。
    size: u64,
    /// 下一个待解密的分块序号。
    chunk: u32,
    /// 总分块数（按明文大小推算）。
    count: u32,
    /// 当前分块已解密的明文缓冲（一块大小，Zeroizing 清零）。
    buffer: zeroize::Zeroizing<Vec<u8>>,
    /// 缓冲内下一个待返回字节的位置。
    offset: usize,
    /// 是否已验证密文精确结束（无多余未认证尾部）。
    verified_end: bool,
}

impl DecryptedFileReader {
    /// 打开 .enc 文件准备流式读取。
    ///
    /// 入参：`path` 密文路径；`key` 数据主密钥；`nonce` 基础 nonce；`size` 明文大小。
    ///
    /// 返回值：`Ok(Self)` 已打开的读取器；`Err` 文件打不开或分块数超出 u32 范围。
    /// 异常：打开文件失败返回 I/O 错误；size: u64 导致分块数量超过 u32 时返回 InvalidFormat。
    /// 注意事项：path/key/nonce/size 来自同一内容版本；这里只准备读取状态，不提前认证首块，也不证明整个文件完整。
    pub fn open(
        path: &Path,
        key: &[u8; DATA_KEY_LEN],
        nonce: &[u8; NONCE_LEN],
        size: u64,
    ) -> Result<Self> {
        Ok(Self {
            source: File::open(path)?,
            key: zeroize::Zeroizing::new(*key),
            nonce: *nonce,
            size,
            chunk: 0,
            count: chunk_count(size)?,
            buffer: zeroize::Zeroizing::new(Vec::new()),
            offset: 0,
            verified_end: false,
        })
    }
}

impl Read for DecryptedFileReader {
    /// 功能：按需认证下一密文分块，并从当前认证明文缓冲向调用方提供流式内容。
    /// 入参：self: &mut DecryptedFileReader 为保存密钥、位置和末尾验证状态的读取器；out: &mut [u8] 为明文目标缓冲。
    /// 返回值：Ok(n) 为本次复制到 out 的字节数，最多取当前块剩余数据；空请求直接 0，真正结束并确认无额外密文后也为 0。
    /// 异常：新块前取消、读取、格式或认证失败转为 io::Error；后续块失败不会撤销先前已返回的明文。
    /// 注意事项：必须一直读取到结束才能验证尾部，仅读一段不能证明整个文件完整；从已有块缓冲返回字节时不会重新检查取消。
    /// 密钥与内部块缓冲析构时清零，调用方 out 中的明文副本有独立生命周期。
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        // 空请求直接返回 0，不推进状态
        if out.is_empty() {
            return Ok(0);
        }
        // 当前缓冲已读完：要么继续解密下一块，要么确认文件结束
        if self.offset == self.buffer.len() {
            // 每读一块前检查取消标志，支持长文件导出中途取消
            crate::progress::check_cancelled().map_err(std::io::Error::other)?;
            if self.chunk == self.count {
                // 声明全部分块已读：首次到达末尾时验证密文没有多余尾部
                if !self.verified_end {
                    ensure_reader_eof(&mut self.source).map_err(std::io::Error::other)?;
                    self.verified_end = true;
                }
                return Ok(0);
            }
            // 读取下一块的密文 + 标签，派生该块 nonce 与 AAD 后解密认证
            let ciphertext = read_chunk_ciphertext(&mut self.source, self.size, self.chunk)
                .map_err(std::io::Error::other)?;
            let nonce = chunk_nonce(&self.nonce, self.chunk);
            let aad = chunk_aad(self.chunk);
            self.buffer = zeroize::Zeroizing::new(
                decrypt_chunk(&self.key, &nonce, &ciphertext, &aad)
                    .map_err(std::io::Error::other)?,
            );
            self.chunk += 1;
            self.offset = 0;
            if self.buffer.is_empty() {
                // 明文为空（空文件）也走一次末尾验证，保证密文精确结束
                ensure_reader_eof(&mut self.source).map_err(std::io::Error::other)?;
                self.verified_end = true;
                return Ok(0);
            }
        }
        // 从当前块缓冲尽量满足本次读取
        let count = out.len().min(self.buffer.len() - self.offset);
        out[..count].copy_from_slice(&self.buffer[self.offset..self.offset + count]);
        self.offset += count;
        Ok(count)
    }
}

/// 在读取旧密文的同时用新密钥写入新密文文件。
///
/// 这是完整改密使用的单遍重写入口：逐块完成“旧密钥解密、新密钥加密、写入新
/// 文件”。文件不会以完整明文形式加载到内存；内存按分块处理，但读写、加解密缓冲可能同时存在。新旧
/// nonce 必须属于各自的数据主密钥，新目标文件已存在时拒绝覆盖。
///
/// 功能：完整改密（双密钥轮值）的迁移动作——把一把 retiring 旧密钥加密的密文
/// 重写为 active 新密钥的密文，文件 ID、版本链、目录关系都不变。
///
/// 入参：
/// - `old_key`/`old_nonce`：旧密文的密钥与 nonce。
/// - `new_key`/`new_nonce`：新密文的密钥与 nonce（新 nonce 必须随机生成）。
/// - `old_path`/`new_path`：旧密文路径与新密文路径（必须不同，新目标不得已存在）。
/// - `plaintext_size`：元数据声明的明文大小。
/// 返回值：全部旧分块认证、重新加密、长度检查及新密文发布成功时返回 Ok(())。
/// 异常：旧源读取、新目标写入/发布、密钥认证、长度格式或加密失败返回 VeilError。
/// 注意事项：old_key/new_key 均为 32 字节，old_nonce/new_nonce 均为 12 字节，plaintext_size 为 u64 字节数；
/// 重写循环本身不主动检查进度取消，业务应按既有轮换阶段约束调用；本方法不修改数据库文件 ID 或版本链。
pub fn rewrite_file_streaming(
    old_key: &[u8; DATA_KEY_LEN],
    new_key: &[u8; DATA_KEY_LEN],
    old_path: &Path,
    new_path: &Path,
    old_nonce: &[u8; NONCE_LEN],
    new_nonce: &[u8; NONCE_LEN],
    plaintext_size: u64,
) -> Result<()> {
    rewrite_file_streaming_impl(
        old_key,
        new_key,
        old_path,
        new_path,
        old_nonce,
        new_nonce,
        plaintext_size,
        false,
    )
}

/// 重写仅由容器事务引用的密文；目标在元数据提交前不得供读取者使用。
///
/// 功能：与 `rewrite_file_streaming` 相同，但目标发布到暂存区（staged=true），
/// 供事务提交引用后再对读取者可见。
/// 入参：old_key/new_key: &[u8; 32] 为旧/新文件密钥；old_path/new_path: &Path 为旧密文和未被引用新目标；
/// old_nonce/new_nonce: &[u8; 12] 为各自基础 nonce；plaintext_size: u64 为原始明文字节数。
/// 返回值：完整重写并发布新暂存密文时为 Ok(())，旧文件保留。
/// 异常：原文件 I/O、认证、新加密、格式或暂存发布失败返回 VeilError。
/// 注意事项：新路径在磁盘上可见，调用方须在数据库引用提交前隔离业务读取，失败需处理可能残留的目标。
pub fn rewrite_staged_file(
    old_key: &[u8; DATA_KEY_LEN],
    new_key: &[u8; DATA_KEY_LEN],
    old_path: &Path,
    new_path: &Path,
    old_nonce: &[u8; NONCE_LEN],
    new_nonce: &[u8; NONCE_LEN],
    plaintext_size: u64,
) -> Result<()> {
    rewrite_file_streaming_impl(
        old_key,
        new_key,
        old_path,
        new_path,
        old_nonce,
        new_nonce,
        plaintext_size,
        true,
    )
}

/// 两个重写入口的公共实现：打开旧密文 → 建临时输出 → 逐块解+加 → 原子发布。
#[allow(clippy::too_many_arguments)]
/// 入参：old_key/new_key 与 old_nonce/new_nonce 为两代密钥上下文；old_path/new_path 为源/目标；
/// plaintext_size: u64 是声明明文大小；staged: bool 决定新密文采用暂存回退还是普通禁止覆盖发布。
/// 返回值：旧密文完整验证、新文件重写与发布成功时为 Ok(())。
/// 异常：读取、格式、认证、加密、写入或发布错误通过 VeilError 传播。
/// 注意事项：只准备新密文，不删除旧文件或改写数据库引用；并发控制由上层租约覆盖。
fn rewrite_file_streaming_impl(
    old_key: &[u8; DATA_KEY_LEN],
    new_key: &[u8; DATA_KEY_LEN],
    old_path: &Path,
    new_path: &Path,
    old_nonce: &[u8; NONCE_LEN],
    new_nonce: &[u8; NONCE_LEN],
    plaintext_size: u64,
    staged: bool,
) -> Result<()> {
    let mut old_file = File::open(old_path)?;
    let mut new_file = create_atomic_output(new_path)?;
    rewrite_reader(
        &mut old_file,
        new_file.as_file_mut(),
        old_key,
        new_key,
        old_nonce,
        new_nonce,
        plaintext_size,
    )?;
    publish_atomic_output(new_file, new_path, false, staged)
}

/// 通过首个分块判断 `.enc` 文件可以使用哪一把数据主密钥解密。
///
/// 返回 `keys` 中命中密钥的索引。该函数只读取首个分块，不写出明文；完整改密可
/// 据此判断文件是否已经迁移到当前数据主密钥。
///
/// 功能：完整改密期间最多两把密钥（active + retiring）；用候选密钥逐个尝试解密
/// 首块，认证通过即命中。用于恢复中断的轮换、判断文件归属哪把密钥。
///
/// 入参：
/// - `source_path`：密文路径。
/// - `keys`：候选数据主密钥数组（每元素一把 32 字节密钥）。
/// - `base_nonce`：文件的基础 nonce。
/// - `plaintext_size`：明文大小（用于计算首块密文长度）。
///
/// 返回值：`Ok(usize)` 命中密钥在数组中的索引；`Err` 无候选密钥或全部认证失败。
///
/// 注意事项：只读首个分块，不产生明文文件；认证失败不泄露内容。
/// 异常：空候选集合为 InvalidFormat；源读取/截断为 I/O 错误；全部候选认证失败为 DecryptionError。
/// 注意事项：函数接受任意非空候选切片，并非内部限制最多两把；只有业务轮换状态通常是两把，
/// 首块通过不代表后续分块或尾部完整，完整验证仍须读取整个密文。
pub fn detect_key_index(
    source_path: &Path,
    keys: &[[u8; DATA_KEY_LEN]],
    base_nonce: &[u8; NONCE_LEN],
    plaintext_size: u64,
) -> Result<usize> {
    if keys.is_empty() {
        return Err(VeilError::InvalidFormat("没有可用的数据主密钥".to_string()));
    }

    let mut source = File::open(source_path)?;
    // 只读首块（序号 0）的密文 + 标签
    let first_ciphertext = read_chunk_ciphertext(&mut source, plaintext_size, 0)?;

    // 逐个候选密钥尝试：首块 nonce 与 AAD 对每个密钥相同，认证通过即命中
    for (index, key) in keys.iter().enumerate() {
        let nonce = chunk_nonce(base_nonce, 0);
        let aad = chunk_aad(0);
        if decrypt_chunk(key, &nonce, &first_ciphertext, &aad).is_ok() {
            return Ok(index);
        }
    }

    Err(VeilError::DecryptionError(
        "文件无法使用任一数据主密钥解密".to_string(),
    ))
}

/// 根据元数据声明的明文大小计算分块数量。
///
/// 空文件按 1 块计算（格式保证 .enc 至少有一块结构），非空按 1 MiB 向上取整。
/// 入参：plaintext_size: u64 为明文总字节数，零值也形成一块认证数据。
/// 返回值：Ok(u32) 为分块总数；空文件为 1，非空按 1 MiB 计算。
/// 异常：分块数量超过 u32 范围返回 InvalidFormat。
/// 注意事项：只计算格式所需块数，不检查真实文件大小或预分配磁盘。
fn chunk_count(plaintext_size: u64) -> Result<u32> {
    let count = if plaintext_size == 0 {
        1
    } else {
        plaintext_size.saturating_add(CHUNK_SIZE as u64 - 1) / CHUNK_SIZE as u64
    };
    u32::try_from(count).map_err(|_| VeilError::InvalidFormat("文件分块数量过多".to_string()))
}

/// 返回指定分块的明文长度。
///
/// 除最后一块外均为 1 MiB；末块为剩余字节。块序号越界或偏移溢出报格式错误。
/// 入参：plaintext_size: u64 为总明文字节数；chunk_index: u32 为从零开始的块序号。
/// 返回值：Ok(usize) 为该块明文长度，范围为 0 到 CHUNK_SIZE；空文件第零块返回 0。
/// 异常：总块数无效、序号越界或块偏移计算溢出返回 InvalidFormat。
/// 注意事项：只按元数据计算，不读取文件；对应密文还需额外包含 16 字节标签。
fn chunk_plaintext_len(plaintext_size: u64, chunk_index: u32) -> Result<usize> {
    let count = chunk_count(plaintext_size)?;
    if chunk_index >= count {
        return Err(VeilError::InvalidFormat("文件分块序号越界".to_string()));
    }

    let start = u64::from(chunk_index)
        .checked_mul(CHUNK_SIZE as u64)
        .ok_or_else(|| VeilError::InvalidFormat("文件分块偏移溢出".to_string()))?;
    if plaintext_size == 0 {
        return Ok(0);
    }

    Ok((plaintext_size - start).min(CHUNK_SIZE as u64) as usize)
}

/// 由文件 base nonce 和块序号派生分块 nonce。
///
/// 哈希输入包含完整 base nonce 和 32 位块序号，避免直接复用文件 nonce，也避免
/// 不同分块使用相同 nonce。
///
/// 派生方式：BLAKE3(base_nonce(12 字节) ‖ 块序号大端 32 位)，取摘要前 12 字节。
/// 这样把文件基础 nonce 与块位置共同纳入派生，降低不同分块复用 nonce 的风险；
/// 12 字节摘要仍有碰撞概率，不能据此承诺数学上的绝不重复，调用方仍须为每次加密生成新基础 nonce。
/// 入参：base_nonce: &[u8; 12] 为文件本次基础 nonce；chunk_index: u32 为从零开始的块序号。
/// 返回值：[u8; 12]，为二者拼接后 BLAKE3 摘要的前 12 字节。
/// 异常：固定长度数组操作，不返回错误。
/// 注意事项：同样输入产生同样结果，不能在同一密钥下复用同一基础 nonce 加密不同版本内容。
fn chunk_nonce(base_nonce: &[u8; NONCE_LEN], chunk_index: u32) -> [u8; NONCE_LEN] {
    let mut input = [0u8; NONCE_LEN + 4];
    input[..NONCE_LEN].copy_from_slice(base_nonce);
    input[NONCE_LEN..].copy_from_slice(&chunk_index.to_be_bytes());

    let digest = blake3::hash(&input);
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&digest.as_bytes()[..NONCE_LEN]);
    nonce
}

/// 构造分块 AAD。
///
/// AAD 绑定块序号，防止攻击者在同一文件内重排或替换分块而不被认证检查发现。
///
/// 结构：8 字节固定前缀 "VEILCHNK" + 4 字节块序号（大端），共 12 字节。
/// 入参：chunk_index: u32 为从零开始的分块序号。
/// 返回值：[u8; 12] 为固定前缀 VEILCHNK 加上大端序号。
/// 异常：固定数组拼装，不返回错误。
/// 注意事项：AAD 参与认证但不保密；它绑定分块位置，不自动携带文件 ID 或版本 ID。
fn chunk_aad(chunk_index: u32) -> [u8; 12] {
    let mut aad = [0u8; 12];
    aad[..8].copy_from_slice(CHUNK_AAD_PREFIX);
    aad[8..].copy_from_slice(&chunk_index.to_be_bytes());
    aad
}

/// 使用指定密钥、分块 nonce 和 AAD 加密一个分块。
///
/// 返回该块的密文 + 认证标签（长度 = 明文 + 16）。
/// 入参：key: &[u8; 32] 为文件密钥；nonce: &[u8; 12] 为当前块 nonce；
/// plaintext: &[u8] 为本块明文；aad: &[u8] 为需共同认证但不加密的数据。
/// 返回值：Ok(`Vec<u8>`) 为密文加 16 字节认证标签。
/// 异常：加密器构造或认证加密失败返回 EncryptionError。
/// 注意事项：不能复用同密钥同 nonce 加密不同内容；返回值及输入缓冲的清理由各自所有者负责。
fn encrypt_chunk(
    key: &[u8; DATA_KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|error| VeilError::EncryptionError(error.to_string()))?;
    cipher
        .encrypt(
            nonce.into(),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|error| VeilError::EncryptionError(error.to_string()))
}

/// 使用指定密钥、分块 nonce 和 AAD 解密并认证一个分块。
///
/// 认证失败（密钥/nonce 错误、密文被篡改、AAD 不符）即报解密错误，不返回明文。
/// 入参：key: &[u8; 32] 与 nonce: &[u8; 12] 必须匹配原加密；
/// ciphertext: &[u8] 包含末尾标签，aad: &[u8] 必须与加密时完全相同。
/// 返回值：Ok(`Vec<u8>`) 为认证通过后的本块明文。
/// 异常：构造解密器或认证失败返回 DecryptionError，不返回本块不可信明文。
/// 注意事项：只验证这一块；完整文件的长度、分块顺序和结束位置由调用链另行检查。
fn decrypt_chunk(
    key: &[u8; DATA_KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>> {
    let cipher = ChaCha20Poly1305::new_from_slice(key)
        .map_err(|error| VeilError::DecryptionError(error.to_string()))?;
    cipher
        .decrypt(
            nonce.into(),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|error| VeilError::DecryptionError(error.to_string()))
}

/// 按元数据长度从 reader 读取一个完整分块的密文和认证标签。
///
/// 分块密文长度 = 该块明文长度 + 16 字节标签；长度由明文大小与块序号推算，
/// 因此密文被截断会立即在读阶段报错。
/// 入参：reader: &mut impl Read 为当前位置正好指向该块的密文流；
/// plaintext_size: u64 为完整明文大小；chunk_index: u32 为本轮块序号。
/// 返回值：Ok(`Vec<u8>`) 为这一块完整密文和标签。
/// 异常：序号/大小非法返回 InvalidFormat，读不满或输入失败返回 I/O 错误。
/// 注意事项：不执行 seek 或解密，必须由调用方按顺序定位流；成功读取不代表标签通过认证。
fn read_chunk_ciphertext(
    reader: &mut impl Read,
    plaintext_size: u64,
    chunk_index: u32,
) -> Result<Vec<u8>> {
    let plaintext_len = chunk_plaintext_len(plaintext_size, chunk_index)?;
    let mut ciphertext = vec![0u8; plaintext_len + TAG_LEN];
    reader.read_exact(&mut ciphertext)?;
    Ok(ciphertext)
}

/// 逐块执行明文 reader 到密文 writer 的加密循环。
///
/// 每轮：检查取消 → 读一块明文 → 派生该块 nonce/AAD → 加密 → 写密文 →
/// 推进进度。全部完成后再次检查取消并验证源文件没有多余尾部（读取端声明
/// 的明文大小必须与源文件精确一致）。
/// 入参：reader/writer 为明文输入与密文输出流的可变借用；data_key: &[u8; 32]、base_nonce: &[u8; 12] 为本次密钥上下文；
/// plaintext_size: u64 为预期明文字节数，控制块数与末块长度。
/// 返回值：全部分块写入且输入精确结束时 Ok(())。
/// 异常：取消、短读/写入、加密、分块格式或尾部检查失败返回 VeilError。
/// 注意事项：发生错误时 writer 可能已有前面完整块或当前部分块；这里不负责原子发布或回滚输出。
fn encrypt_reader(
    reader: &mut impl Read,
    writer: &mut impl Write,
    data_key: &[u8; DATA_KEY_LEN],
    base_nonce: &[u8; NONCE_LEN],
    plaintext_size: u64,
) -> Result<()> {
    let count = chunk_count(plaintext_size)?;
    // 复用同一块大小的缓冲，控制内存占用不随文件大小增长
    let mut buffer = vec![0u8; CHUNK_SIZE];

    // 遍历全部分块：每轮处理一个分块的"读明文 → 写密文"
    for chunk_index in 0..count {
        crate::progress::check_cancelled()?;
        let plaintext_len = chunk_plaintext_len(plaintext_size, chunk_index)?;
        let plaintext = &mut buffer[..plaintext_len];
        // 读满本块的明文（源文件声明大小必须与 metadata 一致）
        reader.read_exact(plaintext)?;

        let nonce = chunk_nonce(base_nonce, chunk_index);
        let aad = chunk_aad(chunk_index);
        let ciphertext = encrypt_chunk(data_key, &nonce, plaintext, &aad)?;
        writer.write_all(&ciphertext)?;
        // 进度按明文字节统计，与取消/展示口径一致
        crate::progress::advance(plaintext_len as u64);
        #[cfg(any(test, feature = "fault-injection"))]
        crate::fsutil::fault_point("during_cipher_write");
    }

    crate::progress::check_cancelled()?;
    // 源文件若比声明更大说明调用方元数据不一致，拒绝静默截断
    ensure_reader_eof(reader)
}

/// 逐块执行密文 reader 到明文 writer 的解密循环。
///
/// 每轮：检查取消 → 读一块密文（含标签）→ 派生 nonce/AAD → 解密认证 →
/// 写明文字节 → 推进进度。全部完成后验证密文精确结束。
/// 入参：reader/writer 为密文输入与认证明文输出的可变借用；data_key/base_nonce 为 32/12 字节上下文；
/// plaintext_size: u64 为数据库声明的明文大小。
/// 返回值：所有块通过认证、完整写出且密文精确结束时为 Ok(())。
/// 异常：取消、密文读取、认证、明文写入或长度格式异常返回 VeilError。
/// 注意事项：只有当前块通过认证才写出，但后面失败不会撤回前面明文；输出是否发布由调用方控制。
fn decrypt_reader(
    reader: &mut impl Read,
    writer: &mut impl Write,
    data_key: &[u8; DATA_KEY_LEN],
    base_nonce: &[u8; NONCE_LEN],
    plaintext_size: u64,
) -> Result<()> {
    let count = chunk_count(plaintext_size)?;

    // 遍历全部分块：每轮处理一个分块的"读密文 → 认证 → 写明文"
    for chunk_index in 0..count {
        crate::progress::check_cancelled()?;
        let ciphertext = read_chunk_ciphertext(reader, plaintext_size, chunk_index)?;
        let nonce = chunk_nonce(base_nonce, chunk_index);
        let aad = chunk_aad(chunk_index);
        // 当前块认证失败即报错，不写出本块明文；前面已认证的块可能已经交给 writer。
        let plaintext = decrypt_chunk(data_key, &nonce, &ciphertext, &aad)?;
        writer.write_all(&plaintext)?;
        crate::progress::advance(plaintext.len() as u64);
    }

    crate::progress::check_cancelled()?;
    // 密文若比元数据声明更长（尾部追加数据），拒绝接受未认证内容
    ensure_reader_eof(reader)
}

/// 逐块执行旧密钥密文到新密钥密文的重写循环。
///
/// 每轮：读旧密文块 → 旧 nonce 解密认证 → 新 nonce 加密 → 写新密文块。
/// 这是完整改密（密钥轮换）的单遍迁移；AAD 与块序号绑定，新旧密文共用同一 AAD。
/// 入参：reader/writer 为旧密文输入与新密文输出；old_key/new_key 为两代 32 字节密钥；
/// old_nonce/new_nonce 为各自 12 字节基础 nonce；plaintext_size: u64 为声明明文大小。
/// 返回值：所有块验证、重加密、写出且源精确结束时 Ok(())。
/// 异常：输入、认证、新加密、输出或格式失败返回 VeilError。
/// 注意事项：此循环不更新进度、不主动响应取消；中途失败时新输出可能不完整，交由上层临时文件机制处理。
fn rewrite_reader(
    reader: &mut impl Read,
    writer: &mut impl Write,
    old_key: &[u8; DATA_KEY_LEN],
    new_key: &[u8; DATA_KEY_LEN],
    old_nonce: &[u8; NONCE_LEN],
    new_nonce: &[u8; NONCE_LEN],
    plaintext_size: u64,
) -> Result<()> {
    let count = chunk_count(plaintext_size)?;

    // 遍历全部分块：每轮完成一块"旧解密 → 新加密"迁移
    for chunk_index in 0..count {
        let ciphertext = read_chunk_ciphertext(reader, plaintext_size, chunk_index)?;
        let old_chunk_nonce = chunk_nonce(old_nonce, chunk_index);
        let new_chunk_nonce = chunk_nonce(new_nonce, chunk_index);
        let aad = chunk_aad(chunk_index);
        let plaintext = decrypt_chunk(old_key, &old_chunk_nonce, &ciphertext, &aad)?;
        let new_ciphertext = encrypt_chunk(new_key, &new_chunk_nonce, &plaintext, &aad)?;
        writer.write_all(&new_ciphertext)?;
    }

    ensure_reader_eof(reader)
}

/// 确认 reader 已到 EOF，防止尾部存在未受认证的追加数据。
///
/// 业务含义：密文长度必须与元数据声明精确一致；多出的字节没有对应的
/// 认证承诺，视为密文被篡改，拒绝使用。
/// 入参：reader: &mut impl Read 为已处理完声明内容后的流。
/// 返回值：再读一个字节得到 0 时为 Ok(())。
/// 异常：仍有字节返回 InvalidFormat，底层读取失败传播 I/O 错误。
/// 注意事项：会消耗至多一个额外字节；错误文案称密文超长，但该工具也用于检查加密前的明文源长度。
fn ensure_reader_eof(reader: &mut impl Read) -> Result<()> {
    let mut extra = [0u8; 1];
    if reader.read(&mut extra)? != 0 {
        return Err(VeilError::InvalidFormat(
            "密文长度超出元数据声明".to_string(),
        ));
    }
    Ok(())
}

/// 在目标文件所在目录创建用于流式写入的临时文件。
///
/// 临时文件使用 `.veil-file-` 前缀（与清理器识别规则一致），并先持久化创建
/// 目标父目录，降低目录丢失风险；新建临时文件目录项本身尚未单独同步。
/// 入参：destination_path: &Path 为最终发布路径，用于选择同目录临时位置。
/// 返回值：Ok(NamedTempFile) 为 .veil-file- 前缀的新临时文件，尚未发布到目标。
/// 异常：父目录创建/同步或临时文件创建失败返回 I/O 错误。
/// 注意事项：不检查最终目标是否存在；守卫通常在未发布时析构清理，但异常退出可能残留临时文件。
fn create_atomic_output(destination_path: &Path) -> Result<tempfile::NamedTempFile> {
    let parent = parent_or_current(destination_path);
    crate::fsutil::create_dir_all_durable(parent)?;
    Ok(tempfile::Builder::new()
        .prefix(".veil-file-")
        .tempfile_in(parent)?)
}

/// 同步流式临时文件并原子发布到目标路径。
///
/// `replace` 为真时允许替换已有目标，供解密导出使用；为假时使用不覆盖发布，
/// 供新 `.enc` 文件创建和迁移使用。
///
/// `staged` 为真表示目标是暂存区文件：用排他重命名/排他创建+复制的发布路径
/// （见 fsutil::publish_staged_file），提交前对读取者不可见。
///
/// 发布后对目录做一次尽力而为的同步：失败只记日志不回滚（文件已是完整新内容）。
/// 入参：output: NamedTempFile 为已写完整的临时文件；destination_path: &Path 为最终目标；
/// replace: bool 允许覆盖，staged: bool 在禁止覆盖时选择可回退复制的暂存发布。
/// 返回值：完整发布成功时 Ok(())，发布后的目录同步失败只告警。
/// 异常：临时文件同步、持久化或暂存发布失败返回 VeilError；可识别的复制取消转换为 Cancelled。
/// 注意事项：replace=true 时 staged 不参与分支选择；复制回退失败可能留下目标，不能仅凭 Err 判断正式路径不存在。
fn publish_atomic_output(
    output: tempfile::NamedTempFile,
    destination_path: &Path,
    replace: bool,
    staged: bool,
) -> Result<()> {
    // 发布前请求同步完整临时内容；实际断电持久性仍取决于文件系统与存储设备。
    crate::fsutil::sync_file(output.as_file())?;
    let parent = parent_or_current(destination_path);
    if replace {
        // 解密导出：允许原子替换旧的导出文件
        output
            .persist(destination_path)
            .map_err(|error| VeilError::Io(error.error))?;
    } else {
        if staged {
            // 暂存发布：排他重命名，不支持时退回"排他创建 + 复制"
            crate::fsutil::publish_staged_file(output.path(), destination_path).map_err(
                |error| {
                    if crate::progress::is_cancelled_io(&error) {
                        VeilError::Cancelled
                    } else {
                        VeilError::Io(error)
                    }
                },
            )?;
        } else {
            // 普通新 .enc：目标已存在即失败，绝不覆盖旧密文
            output
                .persist_noclobber(destination_path)
                .map_err(|error| VeilError::Io(error.error))?;
        }
    }
    // 重命名后的目录同步失败无法安全回滚（文件已是完整新内容），按尽力而为处理
    if let Err(error) = crate::fsutil::sync_directory(parent) {
        crate::logging::failure(
            crate::logging::Level::Warn,
            "storage",
            "sync_published_content",
            "PUBLISHED_DIRECTORY_SYNC_FAILED",
            &error,
            &[
                ("destination", crate::logging::Field::Path(destination_path)),
                ("published", crate::logging::Field::Label("true")),
            ],
        );
    }
    Ok(())
}

/// 返回路径的父目录；没有父目录时使用当前目录。
/// 入参：path: &Path 为目标文件路径。
/// 返回值：非空父路径的借用；没有有效父级时为当前目录 Path。
/// 异常：只拆解路径，不返回错误。
/// 注意事项：不读取或规范化文件系统，不保证父目录存在或可写。
fn parent_or_current(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// 认证并流式计算明文摘要，用于判断覆盖操作是否真的改变内容。
///
/// 功能：解密全文件（每块都认证）的同时把明文喂给 BLAKE3，不落盘明文文件。
/// 摘要与"现有内容摘要"一致说明覆盖没有实质变化，按业务规则不追加版本。
///
/// 入参：`path` 密文路径；`key`/`nonce`/`size` 元数据中的密钥、nonce 与明文大小。
///
/// 返回值：`Ok(blake3::Hash)` 明文摘要；任何一块认证失败即返回解密错误。
/// 异常：密文读取、分块认证、长度检查或取消失败传播 VeilError，不返回部分摘要。
/// 注意事项：path/key/nonce/size 必须对应同一内容版本；哈希相同只支持业务上的内容比较，不代替目录身份或版本链检查。
pub fn plaintext_digest(
    path: &Path,
    key: &[u8; DATA_KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    size: u64,
) -> Result<blake3::Hash> {
    /// 认证明文的内存写入目标，内部仅保存 BLAKE3 状态，不保存文件内容副本。
    struct Digest(blake3::Hasher);
    impl Write for Digest {
        /// 功能：将已认证明文流送入摘要器，供业务判断覆盖内容是否实际变化。
        /// 入参：self: &mut Digest 为 BLAKE3 计算状态；bytes: &[u8] 为这一轮明文字节。
        /// 返回值：Ok(bytes.len())，所有输入都已进入摘要器。
        /// 异常：此内存写入器没有主动 I/O 错误返回分支。
        /// 注意事项：只更新摘要，不保存明文文件或累计进度；调用方仍需让解密链完整认证全部分块。
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        /// 功能：满足摘要写入器的刷新协议，不引入实际磁盘操作。
        /// 入参：self: &mut Digest，为没有输出缓冲的内存摘要器。
        /// 返回值：始终 Ok(())。
        /// 异常：没有主动失败分支。
        /// 注意事项：不完成 finalize，也不表示文件持久化；最终摘要由外层在完整认证后读取。
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut digest = Digest(blake3::Hasher::new());
    // 全文件认证解密的同时计算摘要
    decrypt_file_to_writer(path, &mut digest, key, nonce, size)?;
    Ok(digest.0.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// 验证取消发生在块边界：写到第 1 块密文后被取消，不再读第 2 块。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn cancellation_stops_encryption_at_a_chunk_boundary() {
        // 写入包装器：每写入一次就请求取消，模拟用户中途取消。
        /// 取消边界测试写入器：只累计模拟密文输出并请求取消，不创建真实文件。
        /// progress 指向被测任务，written 用于确认只写出预期分块。
        struct CancelAfterWrite {
            /// 需要在首次模拟写入后请求取消的任务状态。
            progress: crate::progress::Progress,
            /// 已接受的模拟密文字节数，供测试核对块边界。
            written: usize,
        }
        impl Write for CancelAfterWrite {
            /// 功能：模拟密文写出第一块后用户请求取消，验证加密流程的停止边界。
            /// 入参：self 为测试写入器；bytes 为本次待写出的密文字节。
            /// 返回值：返回 bytes.len()，表示测试接受整段数据，同时累计 written。
            /// 异常：取消请求未被接受时 assert! 使测试失败。
            /// 注意事项：只用于单元测试，不会将 bytes 实际落盘。
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.written += bytes.len();
                // 写完这一块密文后立刻请求取消
                assert!(self.progress.request_cancel());
                Ok(bytes.len())
            }
            /// 功能：满足取消测试写入器的刷新接口，不引入真实磁盘操作。
            /// 入参：self 为 CancelAfterWrite 测试对象。
            /// 返回值：始终返回 Ok(())。
            /// 异常：没有主动失败分支。
            /// 注意事项：这是测试替身的空刷新，不说明生产写入端的刷新语义。
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let progress = crate::progress::Progress::default();
        let _scope = progress.begin("import");
        crate::progress::start(Some((CHUNK_SIZE * 2) as u64));
        // 输入两块明文；写入器会在第一块密文落盘后触发取消
        let mut input = std::io::Cursor::new(vec![7; CHUNK_SIZE * 2]);
        let mut output = CancelAfterWrite {
            progress: progress.clone(),
            written: 0,
        };
        assert!(matches!(
            encrypt_reader(
                &mut input,
                &mut output,
                &[1; 32],
                &[2; 12],
                (CHUNK_SIZE * 2) as u64
            ),
            Err(VeilError::Cancelled)
        ));
        // 恰好写出 1 块（明文 + 标签），第 2 块未被读取
        assert_eq!(output.written, CHUNK_SIZE + TAG_LEN);
    }

    /// 验证进度以明文为口径统计：加密和解密都按明文大小推进。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn transfer_progress_counts_plaintext_for_encryption_and_decryption() {
        let root = TempDir::new().unwrap();
        let input = root.path().join("input");
        let encrypted = root.path().join("encrypted");
        let output = root.path().join("output");
        let size = CHUNK_SIZE + 37;
        fs::write(&input, vec![42; size]).unwrap();
        let progress = crate::progress::Progress::default();
        {
            // 加密场景：进度应推进到全部明文大小
            let _scope = progress.begin("import");
            crate::progress::start(Some(size as u64));
            encrypt_file_streaming(&input, &encrypted, &[1; 32], &[2; 12]).unwrap();
            assert_eq!(progress.snapshot().processed_bytes, size as u64);
        }
        {
            // 解密场景：进度同样以明文为口径
            let _scope = progress.begin("export");
            crate::progress::start(Some(size as u64));
            decrypt_file_streaming(&encrypted, &output, &[1; 32], &[2; 12], size as u64).unwrap();
            assert_eq!(progress.snapshot().processed_bytes, size as u64);
        }
        // 解密结果与原文逐字节一致
        assert_eq!(fs::read(input).unwrap(), fs::read(output).unwrap());
    }

    /// 功能：构造同时覆盖完整分块和不足一块尾部的可复现明文样本。
    /// 入参：无。
    /// 返回值：`Vec<u8>`，长度为两个 CHUNK_SIZE 加 123，内容按位置模 251 生成，恰好跨三个块。
    /// 异常：不返回业务错误，分配仍依赖测试进程可用内存。
    /// 注意事项：仅是确定性测试内容，不是安全随机数据；用于逐字节比对和末块边界验证。
    fn multi_chunk_plaintext() -> Vec<u8> {
        let length = CHUNK_SIZE * 2 + 123;
        // 循环取模生成可复现的内容，长度跨 3 个分块（末块 123 字节）
        (0..length).map(|index| (index % 251) as u8).collect()
    }

    /// 验证多分块文件的流式加密和解密。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn streaming_roundtrip_multiple_chunks() {
        let temp_dir = TempDir::new().unwrap();
        let plaintext_path = temp_dir.path().join("plain.bin");
        let encrypted_path = temp_dir.path().join("cipher.enc");
        let restored_path = temp_dir.path().join("restored.bin");
        let plaintext = multi_chunk_plaintext();
        fs::write(&plaintext_path, &plaintext).unwrap();

        let key = [7u8; DATA_KEY_LEN];
        let nonce = generate_base_nonce();
        // 加密 → 解密，往返后必须逐字节一致
        encrypt_file_streaming(&plaintext_path, &encrypted_path, &key, &nonce).unwrap();
        decrypt_file_streaming(
            &encrypted_path,
            &restored_path,
            &key,
            &nonce,
            plaintext.len() as u64,
        )
        .unwrap();

        assert_eq!(fs::read(restored_path).unwrap(), plaintext);
    }

    /// 验证旧密文可以直接流式重写为新密钥密文。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn streaming_rewrite_switches_data_key() {
        let temp_dir = TempDir::new().unwrap();
        let plaintext_path = temp_dir.path().join("plain.bin");
        let old_encrypted_path = temp_dir.path().join("old.enc");
        let new_encrypted_path = temp_dir.path().join("new.enc");
        let restored_path = temp_dir.path().join("restored.bin");
        let plaintext = multi_chunk_plaintext();
        fs::write(&plaintext_path, &plaintext).unwrap();

        let old_key = [3u8; DATA_KEY_LEN];
        let new_key = [9u8; DATA_KEY_LEN];
        let old_nonce = generate_base_nonce();
        let new_nonce = generate_base_nonce();
        // 先用旧密钥加密出旧密文；探测函数应命中第 2 把（索引 1）
        encrypt_file_streaming(&plaintext_path, &old_encrypted_path, &old_key, &old_nonce).unwrap();
        assert_eq!(
            detect_key_index(
                &old_encrypted_path,
                &[new_key, old_key],
                &old_nonce,
                plaintext.len() as u64,
            )
            .unwrap(),
            1
        );

        // 旧密文 → 新密文单遍重写；新密文应命中第 1 把（索引 0）
        rewrite_file_streaming(
            &old_key,
            &new_key,
            &old_encrypted_path,
            &new_encrypted_path,
            &old_nonce,
            &new_nonce,
            plaintext.len() as u64,
        )
        .unwrap();
        assert_eq!(
            detect_key_index(
                &new_encrypted_path,
                &[new_key, old_key],
                &new_nonce,
                plaintext.len() as u64,
            )
            .unwrap(),
            0
        );

        // 新密钥可直接解密出原文
        decrypt_file_streaming(
            &new_encrypted_path,
            &restored_path,
            &new_key,
            &new_nonce,
            plaintext.len() as u64,
        )
        .unwrap();
        assert_eq!(fs::read(restored_path).unwrap(), plaintext);
    }

    /// 验证篡改任意分块都会导致认证失败。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn streaming_decrypt_rejects_tampered_chunk() {
        let temp_dir = TempDir::new().unwrap();
        let plaintext_path = temp_dir.path().join("plain.bin");
        let encrypted_path = temp_dir.path().join("cipher.enc");
        let restored_path = temp_dir.path().join("restored.bin");
        let plaintext = multi_chunk_plaintext();
        fs::write(&plaintext_path, &plaintext).unwrap();

        let key = [5u8; DATA_KEY_LEN];
        let nonce = generate_base_nonce();
        encrypt_file_streaming(&plaintext_path, &encrypted_path, &key, &nonce).unwrap();

        // 翻转第 2 块密文中的 1 个字节（跳过第 1 块的密文 + 标签后再偏移 10）
        let mut encrypted = fs::read(&encrypted_path).unwrap();
        let tamper_offset = CHUNK_SIZE + TAG_LEN + 10;
        encrypted[tamper_offset] ^= 0xff;
        fs::write(&encrypted_path, encrypted).unwrap();

        // 解密必须失败（认证标签不匹配），且不留下部分明文目标
        assert!(
            decrypt_file_streaming(
                &encrypted_path,
                &restored_path,
                &key,
                &nonce,
                plaintext.len() as u64,
            )
            .is_err()
        );
        assert!(!restored_path.exists());
    }
}
