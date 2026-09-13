//! 工作区容器元数据模型。
//!
//! .veil-meta 文件格式：
//! - 明文头部（TLV 格式）：magic, header_len, salt, nonce, algorithm 等
//! - 加密数据：JSON 格式的元数据（文件列表、容器信息、数据主密钥等）
//!
//! `.veil-meta` 是容器身份和内容清单的权威来源，不保存任何绝对路径。明文头部
//! 仅保留恢复和识别所需字段，文件清单与业务元数据统一加密。

use crate::error::VeilError;
pub use crate::file_ops::DATA_KEY_LEN;
use serde::{Deserialize, Serialize};
use std::fmt;
use zeroize::Zeroize;

/// 元数据文件固定魔数。
pub const MAGIC: &[u8; 8] = b"VEILMETA";
/// 当前支持的元数据格式版本。
pub const VERSION: u16 = 3;

/// 元数据中最多保存的数据主密钥数量。
///
/// 第一个是当前主密钥，第二个只用于完整改密过程中兼容尚未迁移的文件。
pub const MAX_DATA_KEYS: usize = 2;

/// `.veil-meta` 明文 TLV 头部使用的字段标签。
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaTag {
    /// 元数据格式版本。
    Version = 0x01,
    /// 密码保护密钥派生使用的盐值。
    Salt = 0x02,
    /// 元数据加密算法 ID。
    Algorithm = 0x03,
    /// 预留的 KDF 参数标签；当前写入流程不生成该字段。
    KdfParams = 0x04,
    /// 元数据加密使用的 nonce。
    Nonce = 0x05,
    /// 容器稳定身份。
    VeilId = 0x06,
    /// 明文容器展示名称。
    ContainerName = 0x07,
    /// 明文工作区类型。
    WorkspaceType = 0x08,
    /// 供扩展字段使用的预留标签。
    Custom = 0xF0,
    /// 预留的 TLV 结束标记；当前写入流程不生成该字段。
    EndMarker = 0xFF,
}

impl MetaTag {
    /// 将 TLV 标签字节转换为已知标签。
    pub fn from_u8(value: u8) -> Option<Self> {
        // 标签值与磁盘格式一一对应；未知值返回 None 由解析器跳过。
        match value {
            0x01 => Some(Self::Version),
            0x02 => Some(Self::Salt),
            0x03 => Some(Self::Algorithm),
            0x04 => Some(Self::KdfParams),
            0x05 => Some(Self::Nonce),
            0x06 => Some(Self::VeilId),
            0x07 => Some(Self::ContainerName),
            0x08 => Some(Self::WorkspaceType),
            0xF0 => Some(Self::Custom),
            0xFF => Some(Self::EndMarker),
            _ => None,
        }
    }
}

/// 元数据加密算法 ID。
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlgorithmId {
    /// AES-256-GCM 标识值。
    Aes256Gcm = 0x01,
    /// ChaCha20-Poly1305 标识值。
    ChaCha20Poly1305 = 0x02,
}

impl AlgorithmId {
    /// 将字节值转换为已知算法 ID。
    pub fn from_u8(value: u8) -> Option<Self> {
        // 未知算法只表示当前解析器不识别，不代表文件一定损坏。
        match value {
            0x01 => Some(Self::Aes256Gcm),
            0x02 => Some(Self::ChaCha20Poly1305),
            _ => None,
        }
    }

    /// 返回用于展示的算法名称。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Aes256Gcm => "AES-256-GCM",
            Self::ChaCha20Poly1305 => "ChaCha20-Poly1305",
        }
    }
}

/// `.veil-meta` 的明文头部和恢复身份信息。
#[derive(Debug, Clone)]
pub struct MetaHeader {
    /// Salt（用于密钥派生）
    pub salt: [u8; 32],

    /// Nonce（用于加密元数据）
    pub nonce: [u8; 12],

    /// 加密元数据时使用的算法标识。
    pub algorithm: AlgorithmId,

    /// 格式版本
    pub version: u16,

    /// 恢复用明文容器 ID；不包含路径或敏感内容。
    pub veil_id: String,

    /// 恢复用明文容器名称；仅用于识别容器。
    pub container_name: String,

    /// 恢复用明文工作区类型。
    pub workspace_type: String,
}

impl MetaHeader {
    /// 使用当前格式版本构造元数据头部。
    ///
    /// `veil_id`、`container_name` 和 `workspace_type` 必须非空，具体约束在
    /// [`MetaHeader::to_bytes`] 中统一校验。
    pub fn new(
        salt: [u8; 32],
        nonce: [u8; 12],
        algorithm: AlgorithmId,
        veil_id: String,
        container_name: String,
        workspace_type: String,
    ) -> Self {
        Self {
            salt,
            nonce,
            algorithm,
            version: VERSION,
            veil_id,
            container_name,
            workspace_type,
        }
    }

    /// 将头部编码为 `magic + header_len + TLV 字段`。
    ///
    /// # 错误
    /// 三个明文身份字段任一为空时返回 [`VeilError::InvalidFormat`]。
    pub fn to_bytes(&self) -> Result<Vec<u8>, VeilError> {
        if self.veil_id.is_empty() {
            return Err(VeilError::InvalidFormat("缺少 veil_id".to_string()));
        }
        if self.container_name.is_empty() {
            return Err(VeilError::InvalidFormat("缺少容器名称".to_string()));
        }
        if self.workspace_type.is_empty() {
            return Err(VeilError::InvalidFormat("缺少工作区类型".to_string()));
        }

        let mut buf = Vec::new();

        buf.extend_from_slice(MAGIC);

        // 先保留长度字段，待所有 TLV 写完后回填。
        let header_len_pos = buf.len();
        buf.extend_from_slice(&[0u8; 2]);

        write_tlv(&mut buf, MetaTag::Version, &self.version.to_le_bytes())?;
        write_tlv(&mut buf, MetaTag::Salt, &self.salt)?;
        write_tlv(&mut buf, MetaTag::Algorithm, &[self.algorithm as u8])?;
        write_tlv(&mut buf, MetaTag::Nonce, &self.nonce)?;
        write_tlv(&mut buf, MetaTag::VeilId, self.veil_id.as_bytes())?;
        write_tlv(
            &mut buf,
            MetaTag::ContainerName,
            self.container_name.as_bytes(),
        )?;
        write_tlv(
            &mut buf,
            MetaTag::WorkspaceType,
            self.workspace_type.as_bytes(),
        )?;

        // header_len 只统计 TLV 区域，不包含 magic 和长度字段本身。
        let header_len = u16::try_from(buf.len() - 10)
            .map_err(|_| VeilError::InvalidFormat("元数据头部过长".to_string()))?;
        buf[header_len_pos..header_len_pos + 2].copy_from_slice(&header_len.to_le_bytes());

        Ok(buf)
    }

    /// 从字节解析元数据头部。
    ///
    /// 解析器忽略未知标签和字段长度不匹配的已知标签，最后统一校验必需字段。
    ///
    /// # 错误
    /// 输入过短、魔数无效、声明的头部超过输入、文本字段不是 UTF-8，或必需字段缺失时
    /// 返回 [`VeilError::InvalidFormat`]。
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, VeilError> {
        if bytes.len() < 10 {
            return Err(VeilError::InvalidFormat("元数据文件过小".to_string()));
        }

        if &bytes[0..8] != MAGIC {
            return Err(VeilError::InvalidFormat("无效的元数据文件魔数".to_string()));
        }

        let header_len = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        let header_end = 10 + header_len;

        if bytes.len() < header_end {
            return Err(VeilError::InvalidFormat("元数据文件头部不完整".to_string()));
        }

        let mut version = None;
        let mut salt = None;
        let mut algorithm = None;
        let mut nonce = None;
        let mut veil_id = String::new();
        let mut container_name = String::new();
        let mut workspace_type = String::new();
        let mut pos = 10;

        // TLV 区域按 tag、u16 长度、value 的固定布局连续解析。
        while pos < header_end {
            let tag_end = pos
                .checked_add(3)
                .ok_or_else(|| VeilError::InvalidFormat("元数据头部长度溢出".to_string()))?;
            if tag_end > header_end {
                return Err(VeilError::InvalidFormat(
                    "元数据头部包含不完整的 TLV 字段".to_string(),
                ));
            }

            let tag = bytes[pos];
            let len = u16::from_le_bytes([bytes[pos + 1], bytes[pos + 2]]) as usize;
            let value_end = tag_end
                .checked_add(len)
                .ok_or_else(|| VeilError::InvalidFormat("元数据字段长度溢出".to_string()))?;
            if value_end > header_end {
                return Err(VeilError::InvalidFormat(
                    "元数据字段长度超出头部范围".to_string(),
                ));
            }
            let value = &bytes[tag_end..value_end];

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
                Some(MetaTag::VeilId) => {
                    veil_id = String::from_utf8(value.to_vec()).map_err(|error| {
                        VeilError::InvalidFormat(format!("veil_id 不是有效的 UTF-8: {}", error))
                    })?;
                }
                Some(MetaTag::ContainerName) => {
                    container_name = String::from_utf8(value.to_vec()).map_err(|error| {
                        VeilError::InvalidFormat(format!("容器名称不是有效的 UTF-8: {}", error))
                    })?;
                }
                Some(MetaTag::WorkspaceType) => {
                    workspace_type = String::from_utf8(value.to_vec()).map_err(|error| {
                        VeilError::InvalidFormat(format!("工作区类型不是有效的 UTF-8: {}", error))
                    })?;
                }
                // 未知标签和长度不符的已知标签都跳过，为后续格式扩展保留空间。
                _ => {}
            }

            // 无论是否识别该字段，都按声明的长度推进到下一个 TLV。
            pos = value_end;
        }

        let version = version.ok_or_else(|| VeilError::InvalidFormat("缺少版本号".to_string()))?;
        if version != VERSION {
            return Err(VeilError::InvalidFormat(format!(
                "不支持的元数据版本: {}",
                version
            )));
        }

        Ok(Self {
            salt: salt.ok_or_else(|| VeilError::InvalidFormat("缺少 salt".to_string()))?,
            nonce: nonce.ok_or_else(|| VeilError::InvalidFormat("缺少 nonce".to_string()))?,
            algorithm: algorithm
                .ok_or_else(|| VeilError::InvalidFormat("缺少算法 ID".to_string()))?,
            version,
            veil_id: (!veil_id.is_empty())
                .then_some(veil_id)
                .ok_or_else(|| VeilError::InvalidFormat("缺少 veil_id".to_string()))?,
            container_name: (!container_name.is_empty())
                .then_some(container_name)
                .ok_or_else(|| VeilError::InvalidFormat("缺少容器名称".to_string()))?,
            workspace_type: (!workspace_type.is_empty())
                .then_some(workspace_type)
                .ok_or_else(|| VeilError::InvalidFormat("缺少工作区类型".to_string()))?,
        })
    }

    /// 读取头部声明的总长度，用于定位加密数据起点。
    ///
    /// # 错误
    /// 输入不足 10 字节、魔数不匹配，或声明长度超出实际输入时返回
    /// [`VeilError::InvalidFormat`]。
    pub fn header_len(bytes: &[u8]) -> Result<usize, VeilError> {
        if bytes.len() < 10 {
            return Err(VeilError::InvalidFormat("元数据文件过小".to_string()));
        }
        if &bytes[0..8] != MAGIC {
            return Err(VeilError::InvalidFormat("无效的元数据文件魔数".to_string()));
        }

        // 长度字段只覆盖 TLV 区域，总偏移需再加 magic 与长度字段自身。
        let header_len = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        let total_len = 10usize
            .checked_add(header_len)
            .ok_or_else(|| VeilError::InvalidFormat("元数据头部长度溢出".to_string()))?;
        if total_len > bytes.len() {
            return Err(VeilError::InvalidFormat(
                "元数据头部长度超出文件范围".to_string(),
            ));
        }

        Ok(total_len)
    }
}

/// 容器元数据（加密存储）。
///
/// 这是容器的权威身份与内容清单：身份字段用于恢复和去重，`files` 记录逻辑
/// 目录树中的全部文件，但所有路径都相对于容器根目录。
#[derive(Clone, Serialize, Deserialize)]
pub struct MetaData {
    /// 不随容器名称变化的稳定身份。
    pub veil_id: String,

    /// 容器名称
    pub container_name: String,

    /// 工作区类型
    pub workspace_type: String,

    /// 创建时间
    pub created_at: String,

    /// 内容清单。
    ///
    /// 逻辑上是树状结构：目录层级由 [`FileEntry::original_name`] 中的相对路径
    /// 分段表达，支持多级目录和文件；物理上使用扁平列表，便于逐文件加密、
    /// 随机访问和增量增删。不得保存绝对路径。
    pub files: Vec<FileEntry>,

    /// 按优先级排列的数据主密钥。
    ///
    /// 索引 0 是当前写入密钥；索引 1 是完整改密时的旧密钥回退槽位。数据主密钥
    /// 本身由密码派生密钥保护的元数据包住，不会以明文落盘。
    pub data_keys: Vec<[u8; DATA_KEY_LEN]>,
}

impl fmt::Debug for MetaData {
    /// 输出元数据摘要，始终隐藏数据主密钥内容。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MetaData")
            .field("veil_id", &self.veil_id)
            .field("container_name", &self.container_name)
            .field("workspace_type", &self.workspace_type)
            .field("created_at", &self.created_at)
            .field("files", &self.files)
            .field(
                "data_keys",
                &format_args!("[REDACTED; {}]", self.data_keys.len()),
            )
            .finish()
    }
}

impl MetaData {
    /// 创建带随机容器身份和空文件清单的元数据。
    ///
    /// `container_name` 是展示名称，`workspace_type` 记录默认、自定义或专属类型。
    pub fn new(container_name: String, workspace_type: String) -> Self {
        // 随机身份与调用方提供的展示信息分离，名称变化不影响容器身份。
        Self::with_veil_id(generate_veil_id(), container_name, workspace_type)
    }

    /// 使用调用方指定的稳定 ID 创建空元数据。
    ///
    /// `veil_id` 应来自已分配的身份，以保证链接和配置在路径变化后仍能定位容器。
    pub fn with_veil_id(veil_id: String, container_name: String, workspace_type: String) -> Self {
        let mut data_key = [0u8; DATA_KEY_LEN];
        getrandom::getrandom(&mut data_key).expect("无法生成数据主密钥");

        Self {
            veil_id,
            container_name,
            workspace_type,
            // 初始清单为空，文件条目由后续 add_file 逐步追加。
            created_at: chrono::Utc::now().to_rfc3339(),
            files: Vec::new(),
            data_keys: vec![data_key],
        }
    }

    /// 返回当前使用的主数据密钥。
    pub fn primary_data_key(&self) -> Result<&[u8; DATA_KEY_LEN], VeilError> {
        self.validate()?;
        self.data_keys
            .first()
            .ok_or_else(|| VeilError::InvalidFormat("缺少数据主密钥".to_string()))
    }

    /// 校验数据主密钥槽位。
    fn validate(&self) -> Result<(), VeilError> {
        if self.data_keys.is_empty() {
            return Err(VeilError::InvalidFormat("缺少数据主密钥".to_string()));
        }
        if self.data_keys.len() > MAX_DATA_KEYS {
            return Err(VeilError::InvalidFormat(format!(
                "数据主密钥数量超过上限: {}",
                self.data_keys.len()
            )));
        }
        for (index, key) in self.data_keys.iter().enumerate() {
            if self.data_keys[..index].contains(key) {
                return Err(VeilError::InvalidFormat("数据主密钥重复".to_string()));
            }
        }
        Ok(())
    }

    /// 将元数据序列化为 UTF-8 JSON。
    ///
    /// # 错误
    /// 字段无法序列化时返回 [`VeilError::SerializationError`]。
    pub fn to_json(&self) -> Result<Vec<u8>, VeilError> {
        // JSON 明文只在内存中短时存在，调用方随后应立即加密。
        serde_json::to_vec(self)
            .map_err(|e| VeilError::SerializationError(format!("序列化元数据失败: {}", e)))
    }

    /// 从 UTF-8 JSON 字节反序列化元数据。
    ///
    /// # 错误
    /// JSON 无效、字段缺失或类型不匹配时返回 [`VeilError::SerializationError`]。
    pub fn from_json(bytes: &[u8]) -> Result<Self, VeilError> {
        // 反序列化要求字段完整；这是解密后内容损坏与版本不兼容的统一入口。
        let metadata: Self = serde_json::from_slice(bytes)
            .map_err(|e| VeilError::SerializationError(format!("反序列化元数据失败: {}", e)))?;
        metadata.validate()?;
        Ok(metadata)
    }

    /// 将文件条目追加到内容清单。
    ///
    /// 本方法不检查原始路径或加密名称是否重复。
    pub fn add_file(&mut self, entry: FileEntry) {
        // 清单按追加顺序保存，磁盘上的加密文件名负责建立唯一映射。
        self.files.push(entry);
    }

    /// 按加密名称删除文件并返回原条目。
    ///
    /// 未找到时返回 `None`。
    pub fn remove_file(&mut self, encrypted_name: &str) -> Option<FileEntry> {
        // 外部名称可能变化，密文文件名才是磁盘文件的稳定引用。
        if let Some(pos) = self
            .files
            .iter()
            .position(|f| f.encrypted_name == encrypted_name)
        {
            Some(self.files.remove(pos))
        } else {
            None
        }
    }

    /// 按原始相对路径查找文件条目。
    ///
    /// 同名条目同时存在时返回第一个匹配项。
    pub fn find_file(&self, original_name: &str) -> Option<&FileEntry> {
        // 用户操作使用原始相对路径，元数据内部按线性顺序查找。
        self.files.iter().find(|f| f.original_name == original_name)
    }

    /// 按磁盘上的加密文件名查找文件条目。
    pub fn find_file_by_encrypted_name(&self, encrypted_name: &str) -> Option<&FileEntry> {
        // 磁盘扫描或恢复流程通常只拿到加密文件名。
        self.files
            .iter()
            .find(|f| f.encrypted_name == encrypted_name)
    }
}

impl Drop for MetaData {
    /// 清除内存中的数据主密钥副本。
    fn drop(&mut self) {
        for key in &mut self.data_keys {
            key.zeroize();
        }
    }
}

/// 生成 `veil-` 前缀的随机容器身份。
///
/// # Panics
/// 系统随机源不可用时 panic，因为生成唯一身份是创建容器的必要前提。
pub fn generate_veil_id() -> String {
    let mut bytes = [0u8; 16];
    // 128 位随机 ID 足以避免正常使用中的碰撞。
    getrandom::getrandom(&mut bytes).expect("无法生成容器 ID");
    format!("veil-{}", hex::encode(bytes))
}

/// `.veil-meta` 中记录的文件条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// 加密后的文件名（随机 ID）
    pub encrypted_name: String,

    /// 相对于容器根目录的原始路径，使用 `/` 分隔多级目录。
    pub original_name: String,

    /// 明文文件大小，单位为字节。
    pub size: u64,

    /// 文件分块流加密使用的 12 字节 base nonce，块 nonce 由它和块序号派生。
    pub nonce: [u8; 12],

    /// RFC 3339 格式的加密时间。
    pub encrypted_at: String,

    /// 可选的预留文件哈希字段。
    pub hash: Option<String>,
}

impl FileEntry {
    /// 创建文件条目并写入当前 UTC 时间，哈希字段默认留空。
    pub fn new(encrypted_name: String, original_name: String, size: u64, nonce: [u8; 12]) -> Self {
        Self {
            encrypted_name,
            original_name,
            size,
            nonce,
            // 条目创建即记录加密时间，恢复和展示都使用该值。
            encrypted_at: chrono::Utc::now().to_rfc3339(),
            hash: None,
        }
    }
}

/// 将一个 TLV 字段追加到头部缓冲区。
///
/// 字段长度使用 `u16` 编码，调用方需保证 `value` 不超过该上限。
fn write_tlv(buf: &mut Vec<u8>, tag: MetaTag, value: &[u8]) -> Result<(), VeilError> {
    // TLV 顺序固定为 tag、u16 长度、value 原文。
    let value_len = u16::try_from(value.len())
        .map_err(|_| VeilError::InvalidFormat("元数据字段过长".to_string()))?;
    buf.push(tag as u8);
    buf.extend_from_slice(&value_len.to_le_bytes());
    buf.extend_from_slice(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个头部长度合法、但末尾 TLV 被截断的元数据文件。
    fn truncated_tlv_header() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&3u16.to_le_bytes());
        bytes.extend_from_slice(&[MetaTag::Version as u8, 2, 0]);
        bytes
    }

    /// 验证截断 TLV 返回格式错误而不是 panic。
    #[test]
    fn truncated_tlv_is_rejected() {
        let bytes = truncated_tlv_header();
        assert!(MetaHeader::from_bytes(&bytes).is_err());
    }

    /// 验证声明长度超出实际文件时会被拒绝。
    #[test]
    fn header_length_beyond_file_is_rejected() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&u16::MAX.to_le_bytes());
        assert!(MetaHeader::header_len(&bytes).is_err());
    }

    /// 验证旧元数据版本不会被静默接受。
    #[test]
    fn old_metadata_version_is_rejected() {
        let header = MetaHeader::new(
            [8u8; 32],
            [9u8; 12],
            AlgorithmId::ChaCha20Poly1305,
            "veil-test".to_string(),
            "test".to_string(),
            "default".to_string(),
        );
        let mut bytes = header.to_bytes().unwrap();
        bytes[13..15].copy_from_slice(&2u16.to_le_bytes());
        assert!(MetaHeader::from_bytes(&bytes).is_err());
    }
}
