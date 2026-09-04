//! # format —— .veil 容器的头部(Header)与尾部(Footer)字节读写
//!
//! 只负责把固定的结构字段在「字节流 ↔ 数值」之间转换（对应 spec §3）：
//! - Header 在文件开头，存 magic/版本/密文私钥 `cip_pri_key`；
//! - Footer 在文件末尾且**定长**，存 Index 的位置，便于打开时直接跳到尾部定位。

use std::io::{Read, Seek, SeekFrom, Write};

use crate::error::{Result, VeilError};
use crate::kdf::KdfType;

/// Header 数据结构
#[derive(Debug, Clone)]
pub struct Header {
    /// CLI 版本字符串（创建容器时的版本）
    pub cli_version: String,
    /// 密文私钥
    pub cip_pri_key: Vec<u8>,
    /// KDF 类型（从 flags 字段解析）
    pub kdf_type: KdfType,
}

/// 文件开头魔数，用来一眼确认"这是个 veil 容器"
pub const MAGIC: &[u8; 8] = b"VEILPKG\0";
/// 文件末尾 Footer 的魔数
pub const FOOTER_MAGIC: &[u8; 8] = b"VEILEND\0";
/// 当前容器格式版本（将来格式演进就靠它做迁移）
pub const VERSION: u16 = 1;
/// Footer 定长：index_offset(8) + index_len(8) + footer_magic(8) = 24 字节
pub const FOOTER_LEN: u64 = 8 + 8 + 8;
/// Header 里固定字段长度：magic(8)+version(2)+flags(2) = 12 字节。
/// 后面跟变长字段：cli_version_len(4) + cli_version(N) + cip_pri_key_len(4) + cip_pri_key(M)
pub const HEADER_FIXED_LEN: u64 = 8 + 2 + 2;

/// 写 Header，返回写入的字节数（= 后续第一个 blob 的起始偏移）。
///
/// # 参数
/// - `writer`:      输出目标（文件或内存缓冲）
/// - `cli_version`: CLI 版本字符串（如 "1.1.0"）
/// - `cip_pri_key`: 密文私钥（keys::encrypt_pri_key 的产物）
/// - `kdf_type`:    KDF 类型（Argon2id）
///
/// # 返回
/// - `Ok(u64)`：写入的字节数（= Header 长度，也是后续第一个 blob 的起始偏移）
/// - `Err(VeilError)`：写入出错
///
/// 结构：
/// - 8 字节 magic：确认这是个 veil 容器
/// - 2 字节 version：当前容器格式版本
/// - 2 字节 flags：bit 0-3 = KDF 类型，bit 4-15 保留
/// - 4 字节 cli_version_len：CLI 版本字符串字节数
/// - N 字节 cli_version：CLI 版本字符串 UTF-8
/// - 4 字节 cip_pri_key_len：密文私钥字节数
/// - M 字节 cip_pri_key：密文私钥内容
pub fn write_header<W: Write>(
    writer: &mut W,
    cli_version: &str,
    cip_pri_key: &[u8],
    kdf_type: KdfType,
) -> Result<u64> {
    let cli_version_bytes = cli_version.as_bytes();

    writer.write_all(MAGIC)?;                                           // 8 字节
    writer.write_all(&VERSION.to_le_bytes())?;                          // 2 字节
    writer.write_all(&kdf_type.to_flags().to_le_bytes())?;             // 2 字节 flags（KDF 类型）
    writer.write_all(&(cli_version_bytes.len() as u32).to_le_bytes())?; // 4 字节 cli_version 长度
    writer.write_all(cli_version_bytes)?;                               // N 字节 cli_version 内容
    writer.write_all(&(cip_pri_key.len() as u32).to_le_bytes())?;       // 4 字节 cip_pri_key 长度
    writer.write_all(cip_pri_key)?;                                     // M 字节 cip_pri_key 内容

    Ok(8 + 2 + 2 + 4 + cli_version_bytes.len() as u64 + 4 + cip_pri_key.len() as u64)
}

/// 读 Header，校验 magic/version，返回 Header 结构（包含 cli_version、cip_pri_key 和 kdf_type）。
pub fn read_header<R: Read>(reader: &mut R) -> Result<Header> {
    let mut magic = [0u8; 8];
    reader.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(VeilError::Format("不是 veil 容器（magic 不匹配）".into()));
    }

    let mut u16buf = [0u8; 2];
    reader.read_exact(&mut u16buf)?;
    let version = u16::from_le_bytes(u16buf);
    if version != VERSION {
        return Err(VeilError::Format(format!("不支持的容器版本: {version}")));
    }

    // 读取 flags 并解析 KDF 类型
    reader.read_exact(&mut u16buf)?;
    let flags = u16::from_le_bytes(u16buf);
    let kdf_type = KdfType::from_flags(flags)?;

    // 读取 cli_version
    let mut u32buf = [0u8; 4];
    reader.read_exact(&mut u32buf)?;
    let cli_version_len = u32::from_le_bytes(u32buf) as usize;
    let mut cli_version_bytes = vec![0u8; cli_version_len];
    reader.read_exact(&mut cli_version_bytes)?;
    let cli_version = String::from_utf8(cli_version_bytes)
        .map_err(|e| VeilError::Format(format!("cli_version 不是有效的 UTF-8: {}", e)))?;

    // 读取 cip_pri_key
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

/// 写 Footer（定长 24 字节），记录 Index 的位置。
pub fn write_footer<W: Write>(writer: &mut W, index_offset: u64, index_len: u64) -> Result<()> {
    writer.write_all(&index_offset.to_le_bytes())?; // 8
    writer.write_all(&index_len.to_le_bytes())?;    // 8
    writer.write_all(FOOTER_MAGIC)?;                // 8
    Ok(())
}

/// 从文件**末尾**读定长 Footer，返回 (index_offset, index_len)。
///
/// 需要 `Seek`：先跳到"末尾往前 24 字节"再读，不必扫全文件。
pub fn read_footer<R: Read + Seek>(reader: &mut R) -> Result<(u64, u64)> {
    reader.seek(SeekFrom::End(-(FOOTER_LEN as i64)))?; // 定位到 Footer 起点
    let mut buf = [0u8; FOOTER_LEN as usize];
    reader.read_exact(&mut buf)?;

    // buf 是 [offset(8) | len(8) | magic(8)]，切片再转数值
    let index_offset = u64::from_le_bytes(buf[0..8].try_into().unwrap());
    let index_len = u64::from_le_bytes(buf[8..16].try_into().unwrap());
    if &buf[16..24] != FOOTER_MAGIC {
        return Err(VeilError::Format("Footer magic 不匹配（文件损坏？）".into()));
    }
    Ok((index_offset, index_len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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