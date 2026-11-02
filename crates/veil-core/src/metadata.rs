//! service 查询得到的容器展示快照与稳定身份、版本 UUID 工具。
//! 此处的 DTO 不定义磁盘格式；容器业务状态由 SQLCipher 数据库管理。
use crate::error::VeilError;
pub use crate::file_ops::DATA_KEY_LEN;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fmt};
use zeroize::Zeroize;
pub const MAX_DATA_KEYS: usize = 2;
/// 容器元数据（加密存储）。
///
/// 这是容器的权威身份与内容清单：身份字段用于恢复和去重，`files` 记录逻辑
/// 目录树中的全部文件，但所有路径都相对于容器根目录。
#[derive(Clone, Serialize, Deserialize)]
pub struct MetaData {
    /// 不随容器名称变化的稳定身份。
    pub veil_id: String,

    /// 容器名称
    pub veil_name: String,

    /// 创建时间
    pub created_at: String,

    /// 内容清单。
    ///
    /// 逻辑上是树状结构：目录层级由 [`FileEntry::original_name`] 中的相对路径
    /// 分段表达，支持多级目录和文件；物理上使用扁平列表，便于逐文件加密、
    /// 随机访问和增量增删。不得保存绝对路径。
    pub files: Vec<FileEntry>,
    /// 从父子关系查询生成的目录展示路径，不作为持久化状态。
    pub directories: Vec<DirectoryEntry>,

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
            .field("veil_name", &self.veil_name)
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
    /// `veil_name` 是展示名称，元数据不记录工作区归属或容器位置。
    pub fn new(veil_name: String) -> Self {
        // 随机身份与调用方提供的展示信息分离，名称变化不影响容器身份。
        Self::with_veil_id(generate_veil_id(), veil_name)
    }

    /// 使用调用方指定的稳定 ID 创建空元数据。
    ///
    /// `veil_id` 应来自已分配的身份，以保证链接和配置在路径变化后仍能定位容器。
    pub fn with_veil_id(veil_id: String, veil_name: String) -> Self {
        let mut data_key = [0u8; DATA_KEY_LEN];
        getrandom::getrandom(&mut data_key).expect("无法生成数据主密钥");

        Self {
            veil_id,
            veil_name,
            // 初始清单为空，文件条目由后续 add_file 逐步追加。
            created_at: chrono::Utc::now().to_rfc3339(),
            files: Vec::new(),
            directories: Vec::new(),
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

        let mut file_ids = HashSet::new();
        for file in &self.files {
            if file.file_id.trim().is_empty() {
                return Err(VeilError::InvalidFormat("文件缺少 file_id".to_string()));
            }
            if !file_ids.insert(file.file_id.as_str()) {
                return Err(VeilError::InvalidFormat(format!(
                    "文件 ID 重复: {}",
                    file.file_id
                )));
            }
            if file.version_history.is_empty() {
                return Err(VeilError::InvalidFormat("文件版本历史为空".to_string()));
            }
            let mut versions = HashSet::new();
            for version in &file.version_history {
                if !is_uuid_v4(version) || !versions.insert(version) {
                    return Err(VeilError::InvalidFormat(
                        "文件版本 UUID 无效或重复".to_string(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// 将元数据序列化为紧凑的 UTF-8 JSONC，省略注释以减少加密数据体积。
    ///
    /// # 错误
    /// 字段无法序列化时返回 [`VeilError::SerializationError`]。
    pub fn to_jsonc(&self) -> Result<Vec<u8>, VeilError> {
        // JSONC 明文只在内存中短时存在，调用方随后应立即加密。
        crate::jsonc::to_vec(self)
    }

    /// 从 UTF-8 JSONC 字节反序列化元数据。
    ///
    /// # 错误
    /// JSONC 无效、字段缺失或类型不匹配时返回 [`VeilError::SerializationError`]。
    pub fn from_jsonc(bytes: &[u8]) -> Result<Self, VeilError> {
        // 反序列化要求字段完整；这是解密后内容损坏与版本不兼容的统一入口。
        let content = std::str::from_utf8(bytes).map_err(|error| {
            VeilError::SerializationError(format!("元数据不是有效 UTF-8: {error}"))
        })?;
        let metadata: Self = crate::jsonc::from_str(content)?;
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

/// 从容器业务表查询生成的文件展示条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// 不随路径、重命名或完整改密变化的稳定文件 ID。
    pub file_id: String,

    /// 按提交顺序排列的文件内容版本 UUID。
    pub version_history: Vec<String>,

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
        Self::with_file_id(
            generate_file_id(),
            encrypted_name,
            original_name,
            size,
            nonce,
        )
    }

    /// 使用已有稳定文件 ID 创建新版本条目。
    pub fn with_file_id(
        file_id: String,
        encrypted_name: String,
        original_name: String,
        size: u64,
        nonce: [u8; 12],
    ) -> Self {
        Self {
            file_id,
            version_history: vec![generate_version_id()],
            encrypted_name,
            original_name,
            size,
            nonce,
            // 条目创建即记录加密时间，恢复和展示都使用该值。
            encrypted_at: chrono::Utc::now().to_rfc3339(),
            hash: None,
        }
    }

    /// 基于当前文件生成内容更新后的版本历史。
    pub fn advance_version(&mut self) {
        self.version_history.push(generate_version_id());
    }
}

/// 使用系统随机源生成标准 UUID v4。
pub fn generate_version_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("无法生成版本 UUID");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = hex::encode(bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

pub fn is_uuid_v4(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            14 => *byte == b'4',
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(byte),
        })
}

/// 生成 `file-` 前缀的随机逻辑文件身份。
///
/// # Panics
/// 系统随机源不可用时 panic，因为文件身份是后续版本和元数据更新的必要前提。
pub fn generate_file_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("无法生成文件 ID");
    format!("file-{}", hex::encode(bytes))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub id: String,
    pub path: String,
}
