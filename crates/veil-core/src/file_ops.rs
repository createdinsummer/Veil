//! 工作区文件操作的统一入口。
//!
//! 上层文件流程不得直接读取或写入 `.enc`，所有文件内容的读取、写入、解密导出和
//! 密钥轮换重写都应经过本模块。`.enc` 文件由固定大小的独立 AEAD 分块依次拼接，
//! 每个分块单独认证，因此内存占用只与分块大小有关，不随文件总大小增长。
//!
//! 后续增加文件级校验、转换、复制或读写组合时，也应优先在本模块实现并复用这里
//! 的分块格式与原子发布逻辑。

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use chacha20poly1305::{
    ChaCha20Poly1305,
    aead::{Aead, KeyInit, Payload},
};

use crate::error::{Result, VeilError};

/// 工作区数据主密钥长度，单位为字节。
pub const DATA_KEY_LEN: usize = 32;

/// 工作区文件分块 nonce 长度，单位为字节。
pub const NONCE_LEN: usize = 12;

/// 单个分块的明文大小，单位为字节。
pub const CHUNK_SIZE: usize = 1024 * 1024;

/// ChaCha20-Poly1305 认证标签长度，单位为字节。
pub const TAG_LEN: usize = 16;

/// 分块 AAD 前缀，用于区分普通文件分块和未来其他 AEAD 用途。
const CHUNK_AAD_PREFIX: &[u8; 8] = b"VEILCHNK";

/// 使用系统随机源生成文件级 base nonce。
///
/// 该值不直接用相同内容重复加密；真正的分块 nonce 还会混入块序号。
pub fn generate_base_nonce() -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce).expect("生成文件 nonce 失败");
    nonce
}

/// 将明文文件流式加密为工作区 `.enc` 文件。
///
/// 输入是明文源文件，输出是分块 ChaCha20-Poly1305 密文文件。目标通过同目录
/// 临时文件原子发布，目标已存在时拒绝覆盖。返回源文件的实际明文大小。
pub fn encrypt_file_streaming(
    source_path: &Path,
    destination_path: &Path,
    data_key: &[u8; DATA_KEY_LEN],
    base_nonce: &[u8; NONCE_LEN],
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
    publish_atomic_output(output, destination_path, false)?;
    Ok(plaintext_size)
}

/// 将工作区 `.enc` 文件流式解密到目标路径。
///
/// 目标允许被原子替换。源文件必须与元数据声明的 `plaintext_size` 和分块格式
/// 一致，否则不会发布输出文件。
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
    publish_atomic_output(output, destination_path, true)
}

/// 在读取旧密文的同时用新密钥写入新密文文件。
///
/// 这是完整改密使用的单遍重写入口：逐块完成“旧密钥解密、新密钥加密、写入新
/// 文件”。文件不会以完整明文形式加载到内存；每个分块只在内存中存在一次。新旧
/// nonce 必须属于各自的数据主密钥，新目标文件已存在时拒绝覆盖。
pub fn rewrite_file_streaming(
    old_key: &[u8; DATA_KEY_LEN],
    new_key: &[u8; DATA_KEY_LEN],
    old_path: &Path,
    new_path: &Path,
    old_nonce: &[u8; NONCE_LEN],
    new_nonce: &[u8; NONCE_LEN],
    plaintext_size: u64,
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
    publish_atomic_output(new_file, new_path, false)
}

/// 通过首个分块判断工作区文件可以使用哪一把数据主密钥解密。
///
/// 返回 `keys` 中命中密钥的索引。该函数只读取首个分块，不写出明文；完整改密可
/// 据此判断文件是否已经迁移到当前数据主密钥。
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
    let first_ciphertext = read_chunk_ciphertext(&mut source, plaintext_size, 0)?;

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
fn chunk_count(plaintext_size: u64) -> Result<u32> {
    let count = if plaintext_size == 0 {
        1
    } else {
        plaintext_size.saturating_add(CHUNK_SIZE as u64 - 1) / CHUNK_SIZE as u64
    };
    u32::try_from(count).map_err(|_| VeilError::InvalidFormat("文件分块数量过多".to_string()))
}

/// 返回指定分块的明文长度。
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
fn chunk_aad(chunk_index: u32) -> [u8; 12] {
    let mut aad = [0u8; 12];
    aad[..8].copy_from_slice(CHUNK_AAD_PREFIX);
    aad[8..].copy_from_slice(&chunk_index.to_be_bytes());
    aad
}

/// 使用指定密钥、分块 nonce 和 AAD 加密一个分块。
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
fn encrypt_reader(
    reader: &mut impl Read,
    writer: &mut impl Write,
    data_key: &[u8; DATA_KEY_LEN],
    base_nonce: &[u8; NONCE_LEN],
    plaintext_size: u64,
) -> Result<()> {
    let count = chunk_count(plaintext_size)?;
    let mut buffer = vec![0u8; CHUNK_SIZE];

    for chunk_index in 0..count {
        let plaintext_len = chunk_plaintext_len(plaintext_size, chunk_index)?;
        let plaintext = &mut buffer[..plaintext_len];
        reader.read_exact(plaintext)?;

        let nonce = chunk_nonce(base_nonce, chunk_index);
        let aad = chunk_aad(chunk_index);
        let ciphertext = encrypt_chunk(data_key, &nonce, plaintext, &aad)?;
        writer.write_all(&ciphertext)?;
    }

    ensure_reader_eof(reader)
}

/// 逐块执行密文 reader 到明文 writer 的解密循环。
fn decrypt_reader(
    reader: &mut impl Read,
    writer: &mut impl Write,
    data_key: &[u8; DATA_KEY_LEN],
    base_nonce: &[u8; NONCE_LEN],
    plaintext_size: u64,
) -> Result<()> {
    let count = chunk_count(plaintext_size)?;

    for chunk_index in 0..count {
        let ciphertext = read_chunk_ciphertext(reader, plaintext_size, chunk_index)?;
        let nonce = chunk_nonce(base_nonce, chunk_index);
        let aad = chunk_aad(chunk_index);
        let plaintext = decrypt_chunk(data_key, &nonce, &ciphertext, &aad)?;
        writer.write_all(&plaintext)?;
    }

    ensure_reader_eof(reader)
}

/// 逐块执行旧密钥密文到新密钥密文的重写循环。
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
fn publish_atomic_output(
    output: tempfile::NamedTempFile,
    destination_path: &Path,
    replace: bool,
) -> Result<()> {
    output.as_file().sync_all()?;
    let parent = parent_or_current(destination_path);
    if replace {
        output
            .persist(destination_path)
            .map_err(|error| VeilError::Io(error.error))?;
    } else {
        output
            .persist_noclobber(destination_path)
            .map_err(|error| VeilError::Io(error.error))?;
    }
    let _ = crate::fsutil::sync_directory(parent);
    Ok(())
}

/// 返回路径的父目录；没有父目录时使用当前目录。
fn parent_or_current(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// 构造跨多个分块的确定性测试数据。
    fn multi_chunk_plaintext() -> Vec<u8> {
        let length = CHUNK_SIZE * 2 + 123;
        (0..length).map(|index| (index % 251) as u8).collect()
    }

    /// 验证多分块文件的流式加密和解密。
    #[test]
    fn streaming_roundtrip_multiple_chunks() {
        let temp_dir = TempDir::new().unwrap();
        let plaintext_path = temp_dir.path().join("plain.bin");
        let encrypted_path = temp_dir.path().join("cipher.enc");
        let restored_path = temp_dir.path().join("restored.bin");
        let plaintext = multi_chunk_plaintext();
        fs::write(&plaintext_path, &plaintext).unwrap();

        let key = [7u8; DATA_KEY_LEN];
        let nonce = generate_base_nonce();
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

        let mut encrypted = fs::read(&encrypted_path).unwrap();
        let tamper_offset = CHUNK_SIZE + TAG_LEN + 10;
        encrypted[tamper_offset] ^= 0xff;
        fs::write(&encrypted_path, encrypted).unwrap();

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
