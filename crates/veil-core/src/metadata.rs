//! service 查询得到的容器展示快照与稳定身份、版本 UUID 工具。
//! 此处的 DTO 不定义磁盘格式；容器业务状态由 SQLCipher 数据库管理。
//!
//! # 业务定位
//!
//! 本模块定义三类纯数据模型与身份工具：
//! 1. `MetaData`：容器元数据快照，汇集容器稳定身份、名称、创建时间、
//!    文件清单与数据主密钥槽位；当前容器业务状态由数据库保存。
//! 2. `FileEntry` / `DirectoryEntry`：从容器业务表查询生成的文件/目录展示条目，
//!    是查询结果快照，不定义磁盘格式。
//! 3. 身份与 UUID 工具：容器 ID、文件 ID、版本 UUID 的生成与校验。
//!
//! # 与数据库的关系
//!
//! 容器业务状态（目录树、版本链、文件信息）的权威来源是 SQLCipher 加密数据库
//! （SQLCipher=SQLite 的整库加密扩展）；本文件的 DTO 只是展示快照，磁盘上
//! 的密文文件名（encrypted_name）与稳定 ID 负责建立映射。
//!
//! 核心函数：MetaData 的构造/校验/JSONC 转换与内存清单操作；FileEntry 的初始化与追加版本；
//! generate_veil_id / generate_file_id / generate_version_id 分配身份，is_uuid_v4 检查规范文本。
//! 依赖：Serde 和 jsonc 模块负责序列化，系统随机源生成身份/密钥，Chrono 提供时间，zeroize 清理已持有密钥。
use crate::error::VeilError;
pub use crate::file_ops::DATA_KEY_LEN;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fmt};
use zeroize::Zeroize;
/// 数据主密钥槽位上限：完整改密期间最多两把（1 把 active 新密钥 + 1 把 retiring
/// 旧密钥），与容器库 `div_info.data_keys` 的双密钥轮值约定一致。
pub const MAX_DATA_KEYS: usize = 2;
/// 容器身份、文件清单和密钥材料的内存快照。
///
/// 身份字段用于恢复和去重，`files` 记录逻辑目录树中的文件，路径相对于容器根目录。
/// 从现有容器读取时，快照由容器数据库中的记录组装；数据库才是持久化状态的来源。
///
/// `data_keys` 是文件内容的数据主密钥，与 SQLCipher 的数据库密码属于两层能力，
/// 不能混用；包含密钥的快照应按敏感数据处理。
#[derive(Clone, Serialize, Deserialize)]
pub struct MetaData {
    /// 不随容器名称变化的稳定身份。
    /// 业务含义：容器改名、移动后仍以 `veil-` 前缀 ID 定位与去重。
    pub veil_id: String,

    /// 容器名称
    /// 业务含义：纯展示名称，元数据不记录工作区归属或容器位置。
    pub veil_name: String,

    /// 创建时间
    /// 业务含义：RFC 3339 格式的原始创建时间，用于展示与排序。
    pub created_at: String,

    /// 内容清单。
    ///
    /// 逻辑上是树状结构：目录层级由 [`FileEntry::original_name`] 中的相对路径
    /// 分段表达，支持多级目录和文件；物理上使用扁平列表，便于逐文件加密、
    /// 随机访问和增量增删。不得保存绝对路径。
    ///
    /// 集合语义：每个元素是容器内一个文件的展示条目，遍历用于序列化、校验
    /// 与按名称查找。
    pub files: Vec<FileEntry>,
    /// 从父子关系查询生成的目录展示路径，不作为持久化状态。
    /// 集合语义：每个元素是容器内一个目录的展示条目（id + 展示路径），
    /// 由数据库父子关系查询生成，磁盘格式不保存它。
    pub directories: Vec<DirectoryEntry>,

    /// 按优先级排列的数据主密钥。
    ///
    /// 索引 0 是当前写入密钥；索引 1 是完整改密时的旧密钥回退槽位。数据主密钥
    /// 保存在加密容器数据库中，不应输出到日志或其他明文介质。
    ///
    /// 集合语义：槽位列表。正常状态只有索引 0 一把；完整改密（轮换文件密钥）
    /// 期间最多两把，新写入用 active 密钥，旧密钥等引用清空后才能移除。
    pub data_keys: Vec<[u8; DATA_KEY_LEN]>,
}

impl fmt::Debug for MetaData {
    /// 功能：输出容器身份和文件清单，供排查快照问题，同时隐藏数据主密钥内容。
    /// 入参：self: &MetaData 为待诊断快照；formatter: &mut fmt::Formatter 为输出器。
    /// 返回值：输出成功返回 Ok(())，输出端失败返回 fmt::Error。
    /// 异常：不执行数据库或文件操作，格式化失败通过 fmt::Result 传播。
    /// 注意事项：files 仍包含明文路径等业务信息；隐藏密钥不等于整份输出已脱敏，日志须经过统一脱敏入口。
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
    /// 功能：创建一个新的容器展示快照，分配随机容器身份和初始文件内容密钥。
    /// 入参：veil_name: String 为用户可见名称，按值保存；本方法不校验空名称或名称规则。
    /// 返回值：MetaData，包含新 veil_id、当前 UTC 创建时间、一把数据主密钥及空文件/目录列表。
    /// 异常：生成身份或密钥时系统随机源失败会 panic。
    /// 注意事项：只建立内存数据，不创建容器目录或数据库；快照含明文密钥，应限制复制和持有时间。
    pub fn new(veil_name: String) -> Self {
        // 随机身份与调用方提供的展示信息分离，名称变化不影响容器身份。
        Self::with_veil_id(generate_veil_id(), veil_name)
    }

    /// 功能：使用指定容器身份创建全新的空快照，供上层在已分配身份后初始化数据。
    /// 入参：veil_id: String 为调用方分配的稳定身份；veil_name: String 为展示名称，均按值保存。
    /// 返回值：MetaData，内容列表为空，创建时间为当前 UTC，并新生成一把文件数据主密钥。
    /// 异常：系统随机源不可用时 panic；本方法不以 Result 报告身份或名称格式问题。
    /// 注意事项：不会恢复旧创建时间、旧文件或旧密钥；不能仅凭旧 ID 调用它来“恢复”原有容器内容。
    /// 指定身份不在这里验证，数据库层需另行检查；文件数据密钥与 SQLCipher 数据库密码是不同资源。
    pub fn with_veil_id(veil_id: String, veil_name: String) -> Self {
        // 先从系统随机源取得文件内容密钥；移入快照后的存储会由 MetaData::drop 清零。
        // 此处的普通栈数组和编译器临时副本没有独立的显式清零守卫，不能承诺全部副本都已擦除。
        let mut data_key = [0u8; DATA_KEY_LEN];
        getrandom::getrandom(&mut data_key).expect("无法生成数据主密钥");

        Self {
            veil_id,
            veil_name,
            // 初始清单为空，文件条目由后续 add_file 逐步追加。
            created_at: chrono::Utc::now().to_rfc3339(),
            files: Vec::new(),
            directories: Vec::new(),
            // 初始只有一把当前写入密钥，占用槽位 0。
            data_keys: vec![data_key],
        }
    }

    /// 功能：在完整快照校验通过后，提供当前新写入文件应使用的数据主密钥。
    /// 入参：self: &MetaData 为包含密钥槽位和文件清单的快照。
    /// 返回值：Ok(&[u8; DATA_KEY_LEN]) 借用槽位 0 的 32 字节密钥，借用期不能超过快照生命周期。
    /// 异常：密钥列表为空、超过两把、重复，或文件身份/版本链不合规则返回 InvalidFormat。
    /// 注意事项：此方法会遍历并校验整个文件清单，非只检查一个槽位；不查询数据库以确认快照是否仍为最新。
    pub fn primary_data_key(&self) -> Result<&[u8; DATA_KEY_LEN], VeilError> {
        self.validate()?;
        self.data_keys
            .first()
            .ok_or_else(|| VeilError::InvalidFormat("缺少数据主密钥".to_string()))
    }

    /// 功能：拒绝无法安全解释的快照，检查密钥槽位以及每个文件的身份和内容版本历史。
    /// 入参：self: &MetaData；data_keys 每项为一把文件密钥，files 每项为一个文件的展示记录。
    /// 返回值：密钥为 1～2 把且互异、文件 ID 非空且互异、各文件版本链非空且 UUID v4 合法无重复时返回 Ok(())。
    /// 异常：任一上述规则失败返回 InvalidFormat，说明中可能包含失败的文件 ID。
    /// 注意事项：不检查容器 ID、路径、密文文件是否存在、nonce 是否复用或文件 ID 的格式；通过不代表整份业务状态已完整验证。
    fn validate(&self) -> Result<(), VeilError> {
        if self.data_keys.is_empty() {
            return Err(VeilError::InvalidFormat("缺少数据主密钥".to_string()));
        }
        // 最多两把：1 把 active + 1 把 retiring，超出即数据异常。
        if self.data_keys.len() > MAX_DATA_KEYS {
            return Err(VeilError::InvalidFormat(format!(
                "数据主密钥数量超过上限: {}",
                self.data_keys.len()
            )));
        }
        // 逐槽位检查重复：槽位 i 的密钥不能与它之前的任何槽位相同。
        for (index, key) in self.data_keys.iter().enumerate() {
            if self.data_keys[..index].contains(key) {
                return Err(VeilError::InvalidFormat("数据主密钥重复".to_string()));
            }
        }

        // file_ids 集合记录已见过的文件 ID，用于检测重复。
        let mut file_ids = HashSet::new();
        // 遍历文件清单：每条是一份文件展示条目，检查身份与版本链合法性。
        for file in &self.files {
            if file.file_id.trim().is_empty() {
                return Err(VeilError::InvalidFormat("文件缺少 file_id".to_string()));
            }
            // insert 返回 false 表示集合里已有同一 ID，即出现重复文件身份。
            if !file_ids.insert(file.file_id.as_str()) {
                return Err(VeilError::InvalidFormat(format!(
                    "文件 ID 重复: {}",
                    file.file_id
                )));
            }
            // 内容版本链必须非空：任何真实存在的文件至少有一个版本。
            if file.version_history.is_empty() {
                return Err(VeilError::InvalidFormat("文件版本历史为空".to_string()));
            }
            // versions 集合记录该文件版本链内已见过的 UUID，用于检测链内重复。
            let mut versions = HashSet::new();
            for version in &file.version_history {
                // 每个版本必须是合法 UUID v4，且链内不重复。
                if !is_uuid_v4(version) || !versions.insert(version) {
                    return Err(VeilError::InvalidFormat(
                        "文件版本 UUID 无效或重复".to_string(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// 功能：把容器快照编码为可供后续加密或传输的紧凑 JSON 字节。
    /// 入参：self: &MetaData，包含容器身份、文件/目录列表及数据主密钥。
    /// 返回值：Ok(`Vec<u8>`) 为 UTF-8 JSON 文本；JSON 是 JSONC 的子集，输出不含注释。
    /// 异常：模型无法序列化时返回 SerializationError。
    /// 注意事项：本方法不调用 validate，不自动加密或写盘；输出包含明文密钥，调用方负责保存范围与缓冲区清理。
    pub fn to_jsonc(&self) -> Result<Vec<u8>, VeilError> {
        // JSONC 明文只在内存中短时存在，调用方随后应立即加密。
        crate::jsonc::to_vec(self)
    }

    /// 功能：从解码后的文本重建容器快照，并拒绝非法密钥槽位、文件身份或版本链。
    /// 入参：bytes: &[u8] 为完整 UTF-8 JSONC 文本，允许 jsonc 模块规定的注释和尾逗号。
    /// 返回值：Ok(MetaData) 为反序列化且经过 validate 检查的快照。
    /// 异常：非 UTF-8、JSONC 语法或字段类型不匹配返回 SerializationError；业务校验失败返回 InvalidFormat。
    /// 注意事项：不会解密输入，也不验证数据库/磁盘内容；校验范围以 validate 为准，不自动补齐缺失业务字段。
    pub fn from_jsonc(bytes: &[u8]) -> Result<Self, VeilError> {
        // 反序列化要求字段完整；这是解密后内容损坏与版本不兼容的统一入口。
        let content = std::str::from_utf8(bytes).map_err(|error| {
            VeilError::SerializationError(format!("元数据不是有效 UTF-8: {error}"))
        })?;
        let metadata: Self = crate::jsonc::from_str(content)?;
        metadata.validate()?;
        Ok(metadata)
    }

    /// 功能：把一条已准备的文件展示记录加入当前内存快照。
    /// 入参：self: &mut MetaData 为可修改快照；entry: FileEntry 为待追加记录，取得其所有权。
    /// 返回值：无；entry 加到 files 末尾，原有条目顺序保持不变。
    /// 异常：没有业务错误返回，不验证路径、身份、版本历史或重复名称。
    /// 注意事项：只改内存，不提交数据库、不创建密文；上层必须先处理冲突并保证持久化状态一致。
    pub fn add_file(&mut self, entry: FileEntry) {
        // 清单按追加顺序保存，磁盘上的加密文件名负责建立唯一映射。
        self.files.push(entry);
    }

    /// 功能：从内存文件清单中移除引用指定密文文件的第一条记录。
    /// 入参：self: &mut MetaData 为当前快照；encrypted_name: &str 为要匹配的完整密文文件名，按字符串精确比较。
    /// 返回值：找到时返回 Some(FileEntry) 并转交被移除记录的所有权；找不到时返回 None。
    /// 异常：不访问文件系统，不返回 I/O 或数据库错误。
    /// 注意事项：不删除磁盘密文或数据库行；密文名标识一份具体密文，不是文件跨内容更新或重加密的永久身份。
    /// 若清单中存在重复引用，仅移除第一项；其余条目保持相对顺序。
    pub fn remove_file(&mut self, encrypted_name: &str) -> Option<FileEntry> {
        // 此次查找针对具体密文引用；逻辑文件跨版本的稳定身份是 file_id，而非密文名。
        if let Some(pos) = self
            .files
            .iter()
            .position(|f| f.encrypted_name == encrypted_name)
        {
            // remove 把该位置的条目移出清单并交还所有权给调用方。
            Some(self.files.remove(pos))
        } else {
            None
        }
    }

    /// 功能：按用户可见的相对路径从快照中查找文件信息。
    /// 入参：self: &MetaData 为待查快照；original_name: &str 为相对容器根的展示路径，通常用 / 分隔。
    /// 返回值：首次精确匹配的 &FileEntry；没有匹配项时为 None。
    /// 异常：线性内存查找，不返回错误。
    /// 注意事项：不规范化路径、大小写或分隔符，不校验路径合法性；结果只反映快照，不保证文件仍存在。
    pub fn find_file(&self, original_name: &str) -> Option<&FileEntry> {
        // 用户操作使用原始相对路径，元数据内部按线性顺序查找。
        self.files.iter().find(|f| f.original_name == original_name)
    }

    /// 功能：在扫描或恢复时，用一份磁盘密文的名称反查对应文件展示记录。
    /// 入参：self: &MetaData 为待查快照；encrypted_name: &str 为密文名称，按完整字符串精确匹配。
    /// 返回值：第一条匹配记录的借用 Some(&FileEntry)，无匹配为 None。
    /// 异常：不读取磁盘或数据库，不返回错误。
    /// 注意事项：线性遍历 files，每项代表一个文件引用；不检测重复引用，不验证密文完整性或最新内容版本。
    pub fn find_file_by_encrypted_name(&self, encrypted_name: &str) -> Option<&FileEntry> {
        // 磁盘扫描或恢复流程通常只拿到加密文件名。
        self.files
            .iter()
            .find(|f| f.encrypted_name == encrypted_name)
    }
}

impl Drop for MetaData {
    /// 功能：释放快照时清零它当前持有的每把文件数据主密钥，缩短敏感材料驻留时间。
    /// 入参：self: &mut MetaData 为即将析构的快照；data_keys 每个元素为一把 32 字节文件密钥。
    /// 返回值：无；逐项调用 zeroize 后由正常字段析构释放列表。
    /// 异常：不执行 I/O，不返回业务错误。
    /// 注意事项：Clone、序列化字节和其他缓冲区中的密钥副本有各自生命周期，不会随此对象一并清理。
    fn drop(&mut self) {
        for key in &mut self.data_keys {
            key.zeroize();
        }
    }
}

/// 功能：为新容器分配不随名称或位置变化的随机身份。
/// 入参：无。
/// 返回值：String，格式为 veil- 加 16 字节随机值的小写十六进制文本，共 37 个 ASCII 字符。
/// 异常：系统随机源不可用时 panic，不使用时间戳等弱随机回退。
/// 注意事项：只生成标识，不登记数据库或检查现有身份；随机碰撞概率很低但不等于数学上不可能。
pub fn generate_veil_id() -> String {
    let mut bytes = [0u8; 16];
    // 128 位随机 ID 足以避免正常使用中的碰撞。
    getrandom::getrandom(&mut bytes).expect("无法生成容器 ID");
    format!("veil-{}", hex::encode(bytes))
}

/// 从容器业务表查询生成的文件展示条目。
///
/// 业务定位：容器内单个文件的展示快照，把数据库 `file_info` 查询结果组装成
/// 客户端可直接展示的对象；不定义磁盘格式，磁盘上只有随机命名的密文文件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// 不随路径、重命名或完整改密变化的稳定文件 ID。
    /// 业务含义：`file-` 前缀的稳定身份，改名/移动/重新加密都不变。
    pub file_id: String,

    /// 按提交顺序排列的文件内容版本 UUID。
    /// 集合语义：内容版本链，每追加一条代表内容发生一次真实变化；
    /// 改名/移动/重新加密不追加。
    pub version_history: Vec<String>,

    /// 加密后的文件名（随机 ID）
    /// 业务含义：指向当前那份磁盘密文；内容更新或重加密可更换此名称，稳定逻辑身份应使用 file_id。
    pub encrypted_name: String,

    /// 相对于容器根目录的原始路径，使用 `/` 分隔多级目录。
    /// 业务含义：用户可见的明文相对路径（含目录层级），展示与查找用。
    pub original_name: String,

    /// 明文文件大小，单位为字节。
    /// 业务含义：解密后的内容长度，用于展示与解密时预检。
    pub size: u64,

    /// 文件分块流加密使用的 12 字节 base nonce，块 nonce 由它和块序号派生。
    /// 业务含义：AEAD（加密同时防篡改）格式的基础随机数，配合块序号派生每块 nonce。
    pub nonce: [u8; 12],

    /// RFC 3339 格式的加密时间。
    /// 业务含义：当前密文的生成时间，用于展示与排序。
    pub encrypted_at: String,

    /// 可选的明文 BLAKE3 哈希；容器列表读取时由数据库填充。
    /// 业务含义：内容完整性指纹；未填充时为空（None）。
    pub hash: Option<String>,
}

impl FileEntry {
    /// 功能：为新加入的逻辑文件创建展示记录，并分配首个内容版本。
    /// 入参：encrypted_name: String 为当前密文文件名；original_name: String 为用户可见相对路径；
    /// size: u64 为完整明文字节数，允许为零；nonce: [u8; 12] 为该密文的基础随机数。
    /// 返回值：FileEntry，包含随机 file_id、仅有一个 UUID 的版本链、当前 UTC 加密时间，hash 为 None。
    /// 异常：生成文件 ID 或版本 UUID 时随机源失败会 panic。
    /// 注意事项：不读取文件、不加密内容，也不验证名称、大小与真实密文是否一致；哈希由上层另行填入。
    pub fn new(encrypted_name: String, original_name: String, size: u64, nonce: [u8; 12]) -> Self {
        Self::with_file_id(
            generate_file_id(),
            encrypted_name,
            original_name,
            size,
            nonce,
        )
    }

    /// 功能：使用已分配的逻辑文件身份初始化一条文件展示记录。
    /// 入参：file_id: String 为调用方给定身份；encrypted_name: String 为具体密文名；original_name: String 为相对路径；
    /// size: u64 为明文字节数；nonce: [u8; 12] 为基础 nonce，均由调用方保证对应同一份内容。
    /// 返回值：FileEntry，版本链重新初始化为一枚新 UUID，时间设为当前 UTC，hash 为 None。
    /// 异常：生成首个版本 UUID 时系统随机源失败会 panic；名称和身份格式不在此校验。
    /// 注意事项：不会继承旧版本链；更新已有文件时应保留原链并调用 advance_version 追加，不能把此初始化直接当作覆盖提交。
    pub fn with_file_id(
        file_id: String,
        encrypted_name: String,
        original_name: String,
        size: u64,
        nonce: [u8; 12],
    ) -> Self {
        Self {
            file_id,
            // 新条目以一个新的版本 UUID 作为版本链起点。
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

    /// 功能：记录当前逻辑文件发生一次真实内容更新，保持原有历史并追加新版本身份。
    /// 入参：self: &mut FileEntry，为已保留原内容版本链的文件记录。
    /// 返回值：无；version_history 末尾增加一枚随机 UUID v4。
    /// 异常：随机源不可用时 panic；不验证原链是否为空、非法或含重复值。
    /// 注意事项：不改密文名、大小、nonce、时间或哈希，也不比较内容；改名、移动和只重加密不应追加版本。
    pub fn advance_version(&mut self) {
        self.version_history.push(generate_version_id());
    }
}

/// 功能：为一次真实内容变化生成随机版本身份，供版本链表达继承关系。
/// 入参：无。
/// 返回值：String，36 字节小写 UUID v4，按 8-4-4-4-12 分组，包含标准版本位与变体位。
/// 异常：系统随机源不可用时 panic。
/// 注意事项：UUID 不包含时间或大小顺序，版本先后只由链的前缀关系判断；不检查全库是否出现过同一 ID。
pub fn generate_version_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("无法生成版本 UUID");
    // 按 RFC 4122 写入版本位（第 7 字节高 4 位=0100，表示版本 4）。
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    // 写入变体位（第 9 字节高 2 位=10，标准变体）。
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = hex::encode(bytes);
    // 按 8-4-4-4-12 分段拼出标准 UUID 文本形式。
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// 功能：校验持久化版本身份是否满足项目使用的规范小写 UUID v4 文本格式。
/// 入参：value: &str 为待检查文本，不自动去除首尾空白或转换大小写。
/// 返回值：恰为 36 字节、连字符位置正确、版本为 4、变体为 8/9/a/b 且其余为小写十六进制时返回 true，否则 false。
/// 异常：短路检查长度后再遍历字节，不返回错误，也不因短文本索引越界。
/// 注意事项：只验证文本形状，无法证明随机性、唯一性、所属文件或历史真实性。
pub fn is_uuid_v4(value: &str) -> bool {
    let bytes = value.as_bytes();
    // 标准形式固定 36 字节：32 个十六进制字符 + 4 个连字符。
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            // 位置 8、13、18、23 必须是连字符。
            8 | 13 | 18 | 23 => *byte == b'-',
            // 位置 14 是版本位：必须为 '4'。
            14 => *byte == b'4',
            // 位置 19 是变体位：必须为 8/9/a/b。
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            // 其余位置是十六进制数字或小写 a-f。
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(byte),
        })
}

/// 功能：为新逻辑文件分配稳定身份，后续改名、移动和内容更新仍沿用该身份。
/// 入参：无。
/// 返回值：String，file- 加 16 字节随机值的小写十六进制文本，共 37 个 ASCII 字符。
/// 异常：系统随机源不可用时 panic。
/// 注意事项：不是磁盘密文名称或版本 UUID；不创建文件，不查询数据库去重，持久化唯一性由上层约束。
pub fn generate_file_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("无法生成文件 ID");
    format!("file-{}", hex::encode(bytes))
}

/// 目录展示条目：从容器业务表父子关系查询生成的展示快照。
///
/// 业务定位：把数据库里的目录节点组装成客户端可直接展示的对象；`path` 是
/// 沿父链组合出来的展示路径，不作为持久化权威字段。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryEntry {
    /// 目录节点的稳定 ID，改名/移动不变。
    pub id: String,
    /// 从父子关系组合生成的目录展示路径（相对容器根）。
    pub path: String,
}
