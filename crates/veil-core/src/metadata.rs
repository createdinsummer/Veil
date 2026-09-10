//! 容器元数据模块
//!
//! .veil-meta 文件格式：
//! - 明文头部（TLV 格式）：magic, header_len, salt, nonce, algorithm 等
//! - 加密数据：JSON 格式的元数据（文件列表、容器信息等）

use crate::error::VeilError;
use serde::{Deserialize, Serialize};

/// 元数据文件魔数
pub const MAGIC: &[u8; 8] = b"VEILMETA";

/// 元数据 Tag 类型
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaTag {
    Version = 0x01,
    Salt = 0x02,
    Algorithm = 0x03,
    KdfParams = 0x04,
    Nonce = 0x05,
    Custom = 0xF0,
    EndMarker = 0xFF,
}

impl MetaTag {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x01 => Some(Self::Version),
            0x02 => Some(Self::Salt),
            0x03 => Some(Self::Algorithm),
            0x04 => Some(Self::KdfParams),
            0x05 => Some(Self::Nonce),
            0xF0 => Some(Self::Custom),
            0xFF => Some(Self::EndMarker),
            _ => None,
        }
    }
}

/// 算法 ID
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlgorithmId {
    Aes256Gcm = 0x01,
    ChaCha20Poly1305 = 0x02,
}

impl AlgorithmId {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x01 => Some(Self::Aes256Gcm),
            0x02 => Some(Self::ChaCha20Poly1305),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Aes256Gcm => "AES-256-GCM",
            Self::ChaCha20Poly1305 => "ChaCha20-Poly1305",
        }
    }
}

/// 元数据文件头部
#[derive(Debug, Clone)]
pub struct MetaHeader {
    /// Salt（用于密钥派生）
    pub salt: [u8; 32],

    /// Nonce（用于加密元数据）
    pub nonce: [u8; 12],

    /// 算法 ID
    pub algorithm: AlgorithmId,

    /// 格式版本
    pub version: u16,
}

impl MetaHeader {
    /// 创建新的头部
    pub fn new(salt: [u8; 32], nonce: [u8; 12], algorithm: AlgorithmId) -> Self {
        Self {
            salt,
            nonce,
            algorithm,
            version: 1,
        }
    }

    /// 序列化头部为字节
    pub fn to_bytes(&self) -> Result<Vec<u8>, VeilError> {
        let mut buf = Vec::new();

        // Magic
        buf.extend_from_slice(MAGIC);

        // 预留 header_len 位置
        let header_len_pos = buf.len();
        buf.extend_from_slice(&[0u8; 2]);

        // 写入 TLV 字段
        write_tlv(&mut buf, MetaTag::Version, &self.version.to_le_bytes())?;
        write_tlv(&mut buf, MetaTag::Salt, &self.salt)?;
        write_tlv(&mut buf, MetaTag::Algorithm, &[self.algorithm as u8])?;
        write_tlv(&mut buf, MetaTag::Nonce, &self.nonce)?;

        // 回填 header_len
        let header_len = (buf.len() - 10) as u16;
        buf[header_len_pos..header_len_pos + 2].copy_from_slice(&header_len.to_le_bytes());

        Ok(buf)
    }

    /// 从字节反序列化头部
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, VeilError> {
        if bytes.len() < 10 {
            return Err(VeilError::InvalidFormat("元数据文件过小".to_string()));
        }

        // 验证 magic
        if &bytes[0..8] != MAGIC {
            return Err(VeilError::InvalidFormat("无效的元数据文件魔数".to_string()));
        }

        // 读取 header_len
        let header_len = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        let header_end = 10 + header_len;

        if bytes.len() < header_end {
            return Err(VeilError::InvalidFormat("元数据文件头部不完整".to_string()));
        }

        // 解析 TLV
        let mut version = None;
        let mut salt = None;
        let mut algorithm = None;
        let mut nonce = None;
        let mut pos = 10;

        while pos < header_end {
            let tag = bytes[pos];
            let len = u16::from_le_bytes([bytes[pos + 1], bytes[pos + 2]]) as usize;
            let value = &bytes[pos + 3..pos + 3 + len];

            match MetaTag::from_u8(tag) {
                Some(MetaTag::Version) if len == 2 => {
                    version = Some(u16::from_le_bytes([value[0], value[1]]));
                }
                Some(MetaTag::Salt) if len == 32 => {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(value);
                    salt = Some(arr);
                }
                Some(MetaTag::Algorithm) if len == 1 => {
                    algorithm = AlgorithmId::from_u8(value[0]);
                }
                Some(MetaTag::Nonce) if len == 12 => {
                    let mut arr = [0u8; 12];
                    arr.copy_from_slice(value);
                    nonce = Some(arr);
                }
                _ => {} // 跳过未知或长度不匹配的 tag
            }

            pos += 3 + len;
        }

        Ok(Self {
            salt: salt.ok_or_else(|| VeilError::InvalidFormat("缺少 salt".to_string()))?,
            nonce: nonce.ok_or_else(|| VeilError::InvalidFormat("缺少 nonce".to_string()))?,
            algorithm: algorithm.ok_or_else(|| VeilError::InvalidFormat("缺少算法 ID".to_string()))?,
            version: version.unwrap_or(1),
        })
    }

    /// 获取头部长度（用于计算加密数据起始位置）
    pub fn header_len(bytes: &[u8]) -> Result<usize, VeilError> {
        if bytes.len() < 10 {
            return Err(VeilError::InvalidFormat("元数据文件过小".to_string()));
        }
        let header_len = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        Ok(10 + header_len)
    }
}

/// 容器元数据（加密存储）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaData {
    /// 容器名称
    pub container_name: String,

    /// 工作区类型
    pub workspace_type: String,

    /// 创建时间
    pub created_at: String,

    /// 文件列表
    pub files: Vec<FileEntry>,
}

impl MetaData {
    /// 创建新的元数据
    pub fn new(container_name: String, workspace_type: String) -> Self {
        Self {
            container_name,
            workspace_type,
            created_at: chrono::Utc::now().to_rfc3339(),
            files: Vec::new(),
        }
    }

    /// 序列化为 JSON
    pub fn to_json(&self) -> Result<Vec<u8>, VeilError> {
        serde_json::to_vec(self).map_err(|e| {
            VeilError::SerializationError(format!("序列化元数据失败: {}", e))
        })
    }

    /// 从 JSON 反序列化
    pub fn from_json(bytes: &[u8]) -> Result<Self, VeilError> {
        serde_json::from_slice(bytes).map_err(|e| {
            VeilError::SerializationError(format!("反序列化元数据失败: {}", e))
        })
    }

    /// 添加文件
    pub fn add_file(&mut self, entry: FileEntry) {
        self.files.push(entry);
    }

    /// 删除文件
    pub fn remove_file(&mut self, encrypted_name: &str) -> Option<FileEntry> {
        if let Some(pos) = self.files.iter().position(|f| f.encrypted_name == encrypted_name) {
            Some(self.files.remove(pos))
        } else {
            None
        }
    }

    /// 查找文件
    pub fn find_file(&self, original_name: &str) -> Option<&FileEntry> {
        self.files.iter().find(|f| f.original_name == original_name)
    }

    /// 通过加密名查找文件
    pub fn find_file_by_encrypted_name(&self, encrypted_name: &str) -> Option<&FileEntry> {
        self.files.iter().find(|f| f.encrypted_name == encrypted_name)
    }
}

/// 文件条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// 加密后的文件名（随机 ID）
    pub encrypted_name: String,

    /// 原始文件名
    pub original_name: String,

    /// 文件大小（字节）
    pub size: u64,

    /// 该文件的 nonce
    pub nonce: [u8; 12],

    /// 加密时间
    pub encrypted_at: String,

    /// 文件哈希（可选）
    pub hash: Option<String>,
}

impl FileEntry {
    /// 创建新的文件条目
    pub fn new(
        encrypted_name: String,
        original_name: String,
        size: u64,
        nonce: [u8; 12],
    ) -> Self {
        Self {
            encrypted_name,
            original_name,
            size,
            nonce,
            encrypted_at: chrono::Utc::now().to_rfc3339(),
            hash: None,
        }
    }
}

/// 写入 TLV 字段
fn write_tlv(buf: &mut Vec<u8>, tag: MetaTag, value: &[u8]) -> Result<(), VeilError> {
    buf.push(tag as u8);
    buf.extend_from_slice(&(value.len() as u16).to_le_bytes());
    buf.extend_from_slice(value);
    Ok(())
}
