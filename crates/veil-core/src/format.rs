//! `.veil` 容器的 Header 与 Footer 编解码。
//!
//! Header 位于文件开头，保存格式版本、KDF 标志、CLI 版本和加密后的容器私钥；
//! Footer 固定为 24 字节并位于文件末尾，记录目录索引的偏移和长度。小端序字段和
//! 固定魔数使打开容器时无需扫描整个文件即可完成格式校验和索引定位。

use std::io::{Read, Seek, SeekFrom, Write};

use crate::error::{Result, VeilError};
use crate::kdf::KdfType;

/// `.veil` 容器 Header 中已解析的字段。
#[derive(Debug, Clone)]
pub struct Header {
    /// 创建容器时写入的 CLI 版本。
    pub cli_version: String,
    /// 经用户密码保护后的容器私钥。
    pub cip_pri_key: Vec<u8>,
    /// 从 flags 低四位解析出的 KDF 类型。
    pub kdf_type: KdfType,
}

/// 容器文件开头的固定魔数。
pub const MAGIC: &[u8; 8] = b"VEILPKG\0";
/// 容器 Footer 的固定魔数。
pub const FOOTER_MAGIC: &[u8; 8] = b"VEILEND\0";
/// 当前支持的容器格式版本。
pub const VERSION: u16 = 1;
/// Footer 的固定长度：两个 `u64` 字段加 8 字节魔数。
pub const FOOTER_LEN: u64 = 8 + 8 + 8;
/// Header 固定前缀长度：magic、版本和 flags。
///
/// 前缀后依次为固定 4 字节的 `cli_version_len`、变长 `cli_version`、固定 4 字节的
/// `cip_pri_key_len` 和变长 `cip_pri_key`。
pub const HEADER_FIXED_LEN: u64 = 8 + 2 + 2;

/// 按当前格式写入容器 Header。
///
/// # 参数
/// - `writer`：Header 的输出目标。
/// - `cli_version`：创建容器的 CLI 版本。
/// - `cip_pri_key`：由 [`crate::keys::encrypt_pri_key`] 生成的受保护私钥。
/// - `kdf_type`：写入 flags 的 KDF 类型。
///
/// # 返回
/// 成功时返回 Header 总长度，该值也是首个 blob 的起始偏移。
///
/// # 错误
/// 底层 `Write` 失败时返回 [`VeilError::Io`]。
pub fn write_header<W: Write>(
    writer: &mut W,
    cli_version: &str,
    cip_pri_key: &[u8],
    kdf_type: KdfType,
) -> Result<u64> {
    let cli_version_bytes = cli_version.as_bytes();

    // 固定前缀定义容器身份和格式版本，读取端据此选择解析规则。
    writer.write_all(MAGIC)?;
    writer.write_all(&VERSION.to_le_bytes())?;
    writer.write_all(&kdf_type.to_flags().to_le_bytes())?;
    // 两个变长字段都采用“u32 长度 + 原始字节”的形式，便于顺序解析。
    writer.write_all(&(cli_version_bytes.len() as u32).to_le_bytes())?;
    writer.write_all(cli_version_bytes)?;
    writer.write_all(&(cip_pri_key.len() as u32).to_le_bytes())?;
    writer.write_all(cip_pri_key)?;

    Ok(8 + 2 + 2 + 4 + cli_version_bytes.len() as u64 + 4 + cip_pri_key.len() as u64)
}

/// 从字节流读取并校验容器 Header。
///
/// # 错误
/// 魔数或版本不匹配、flags 中的 KDF 未知、CLI 版本不是 UTF-8，或底层读取失败时返回错误。
pub fn read_header<R: Read>(reader: &mut R) -> Result<Header> {
    // 先读取并验证固定字段，后续所有长度都建立在此格式版本之上。
    let mut magic = [0u8; 8];
    reader.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(VeilError::Format("不是 veil 容器（magic 不匹配）".into()));
    }

    let mut u16buf = [0u8; 2];
    // flags 的低四位编码 KDF，高位当前保留但不会影响本版本解析。
    reader.read_exact(&mut u16buf)?;
    let version = u16::from_le_bytes(u16buf);
    if version != VERSION {
        return Err(VeilError::Format(format!("不支持的容器版本: {version}")));
    }

    reader.read_exact(&mut u16buf)?;
    let flags = u16::from_le_bytes(u16buf);
    let kdf_type = KdfType::from_flags(flags)?;

    // CLI 版本是带长度前缀的 UTF-8 文本。
    let mut u32buf = [0u8; 4];
    // 密文私钥紧跟在 CLI 版本之后，读取时保持原始字节不做解释。
    reader.read_exact(&mut u32buf)?;
    let cli_version_len = u32::from_le_bytes(u32buf) as usize;
    let mut cli_version_bytes = vec![0u8; cli_version_len];
    reader.read_exact(&mut cli_version_bytes)?;
    let cli_version = String::from_utf8(cli_version_bytes)
        .map_err(|e| VeilError::Format(format!("cli_version 不是有效的 UTF-8: {}", e)))?;

    reader.read_exact(&mut u32buf)?;
    let cip_pri_key_len = u32::from_le_bytes(u32buf) as usize;
    let mut cip_pri_key = vec![0u8; cip_pri_key_len];
    reader.read_exact(&mut cip_pri_key)?;

    Ok(Header {
        cli_version,
        cip_pri_key,
        kdf_type,
    })
}

/// 写入固定长度的 Footer，记录目录索引的位置。
///
/// # 错误
/// 底层 `Write` 失败时返回 [`VeilError::Io`]。
pub fn write_footer<W: Write>(writer: &mut W, index_offset: u64, index_len: u64) -> Result<()> {
    // 顺序与 read_footer 的切片偏移严格对应：offset、len、magic。
    writer.write_all(&index_offset.to_le_bytes())?;
    writer.write_all(&index_len.to_le_bytes())?;
    writer.write_all(FOOTER_MAGIC)?;
    Ok(())
}

/// 从文件末尾读取 Footer，返回索引偏移和长度。
///
/// # 错误
/// 文件短于 Footer、底层读取失败或末尾魔数不匹配时返回错误。
pub fn read_footer<R: Read + Seek>(reader: &mut R) -> Result<(u64, u64)> {
    // Footer 固定长度，直接从文件末尾反向定位，无需扫描前面的 blob。
    reader.seek(SeekFrom::End(-(FOOTER_LEN as i64)))?;
    let mut buf = [0u8; FOOTER_LEN as usize];
    reader.read_exact(&mut buf)?;

    // 魔数最后校验，保证只有完整且位置正确的 Footer 才会被接受。
    let index_offset = u64::from_le_bytes(buf[0..8].try_into().unwrap());
    let index_len = u64::from_le_bytes(buf[8..16].try_into().unwrap());
    if &buf[16..24] != FOOTER_MAGIC {
        return Err(VeilError::Format("Footer magic 不匹配（文件损坏？）".into()));
    }
    Ok((index_offset, index_len))
}

/// Header 与 Footer 编解码的单元测试。
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// 验证 Header、Footer 写入后能按原字段读回。
    #[test]
    fn header_footer_roundtrip() {
        // Cursor<Vec<u8>> 是"内存里的假文件"，同时实现 Read+Write+Seek，测试很方便
        let mut buf = Cursor::new(Vec::new());

        let fake_cip_pri_key = b"pretend-this-is-cip-pri-key";
        let cli_version = "1.1.0";
        let header_len = write_header(&mut buf, cli_version, fake_cip_pri_key, KdfType::Argon2id).unwrap();
        write_footer(&mut buf, 111, 222).unwrap();
        println!("header 长度 = {header_len} 字节");

        // 回到开头读 Header
        buf.set_position(0);
        let header = read_header(&mut buf).unwrap();
        assert_eq!(header.cip_pri_key, fake_cip_pri_key);
        assert_eq!(header.cli_version, cli_version);
        assert_eq!(header.kdf_type, KdfType::Argon2id);

        // 读末尾 Footer
        let (offset, len) = read_footer(&mut buf).unwrap();
        assert_eq!((offset, len), (111, 222));
    }

    /// 验证 Argon2id 标志位和密文私钥能够往返解析。
    #[test]
    fn header_argon2_roundtrip() {
        let mut buf = Cursor::new(Vec::new());
        let fake_cip_pri_key = b"argon2-encrypted-key";
        let cli_version = "2.0.0";

        write_header(&mut buf, cli_version, fake_cip_pri_key, KdfType::Argon2id).unwrap();

        buf.set_position(0);
        let header = read_header(&mut buf).unwrap();
        assert_eq!(header.kdf_type, KdfType::Argon2id);
        assert_eq!(header.cip_pri_key, fake_cip_pri_key);
    }
}
