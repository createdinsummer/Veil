//! 可分享 `.veil` 打包文件格式。
//!
//! 该格式把工作区的加密元数据和各文件密文依次装入单个文件，用于备份、分享和
//! 跨工作区传输。包内不重新加密内容，只定义头部、元数据段和文件条目的布局，
//! 因而解包后仍需使用原容器密码解密。
//!
//! 文件结构：
//! ```text
//! [Header]
//!   magic: "VEILPKG\0" (8 bytes)
//!   version: u16 (2 bytes)
//!   header_size: u32 (4 bytes)
//!   metadata_size: u32 (4 bytes)
//!   file_count: u32 (4 bytes)
//!
//! [Metadata Section]
//!   加密的 JSON 元数据（包含文件列表）
//!
//! [File Data Section]
//!   [File 1 Header]
//!     name_len: u16
//!     name: [u8; name_len]
//!     data_size: u64
//!   [File 1 Data]
//!     encrypted data
//!   [File 2 Header]
//!     ...
//! ```

use crate::error::VeilError;
use crate::metadata::MetaData;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// 容器文件魔数
pub const CONTAINER_MAGIC: &[u8; 8] = b"VEILPKG\0";

/// 当前打包文件版本。
pub const CONTAINER_VERSION: u16 = 1;

/// `.veil` 打包文件头部。
#[derive(Debug, Clone)]
pub struct ContainerHeader {
    /// 打包文件格式版本。
    pub version: u16,
    /// 以字节计的头部长度。
    pub header_size: u32,
    /// 紧随头部之后的加密元数据长度。
    pub metadata_size: u32,
    /// 元数据声明的文件条目数量。
    pub file_count: u32,
}

impl ContainerHeader {
    /// 使用当前版本构造固定长度 22 字节的头部。
    pub fn new(metadata_size: u32, file_count: u32) -> Self {
        Self {
            version: CONTAINER_VERSION,
            header_size: 22,
            metadata_size,
            file_count,
        }
    }

    /// 将头部按小端序序列化为 22 字节。
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.header_size as usize);

        // 固定字段按写入顺序排列，读取端无需额外查找即可顺序解析。
        buf.extend_from_slice(CONTAINER_MAGIC);
        buf.extend_from_slice(&self.version.to_le_bytes());
        buf.extend_from_slice(&self.header_size.to_le_bytes());
        buf.extend_from_slice(&self.metadata_size.to_le_bytes());
        buf.extend_from_slice(&self.file_count.to_le_bytes());

        buf
    }

    /// 从至少 22 字节的输入解析头部并校验魔数。
    ///
    /// # 错误
    /// 输入不足或魔数不匹配时返回 [`VeilError::InvalidFormat`]。本函数不额外校验
    /// 版本和 `header_size` 的语义。
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, VeilError> {
        if bytes.len() < 22 {
            return Err(VeilError::InvalidFormat("容器文件头部不完整".to_string()));
        }

        if &bytes[0..8] != CONTAINER_MAGIC {
            return Err(VeilError::InvalidFormat("无效的容器文件魔数".to_string()));
        }

        // 字段偏移与 to_bytes 一致，全部采用小端序。
        let version = u16::from_le_bytes([bytes[8], bytes[9]]);
        let header_size = u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]);
        let metadata_size = u32::from_le_bytes([bytes[14], bytes[15], bytes[16], bytes[17]]);
        let file_count = u32::from_le_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]);

        Ok(Self {
            version,
            header_size,
            metadata_size,
            file_count,
        })
    }
}

/// 打包文件中的单个文件条目头部。
#[derive(Debug, Clone)]
pub struct FileEntryHeader {
    /// 工作区元数据中的原始相对路径。
    pub name: String,
    /// 紧随条目头部之后的密文字节数。
    pub data_size: u64,
}

impl FileEntryHeader {
    /// 将名称长度、名称和密文长度序列化为一个小端序条目。
    ///
    /// 名称长度使用 `u16`，调用方需保证名称字节长度不超过该字段容量。
    pub fn to_bytes(&self) -> Vec<u8> {
        let name_bytes = self.name.as_bytes();
        // 名称长度只占 2 字节，条目名总长度由该字段限制。
        let name_len = name_bytes.len() as u16;

        let mut buf = Vec::with_capacity(2 + name_bytes.len() + 8);

        buf.extend_from_slice(&name_len.to_le_bytes());
        buf.extend_from_slice(name_bytes);
        buf.extend_from_slice(&self.data_size.to_le_bytes());

        buf
    }

    /// 从字节解析文件条目。
    ///
    /// # 返回
    /// 返回条目本身以及本次解析消耗的字节数。
    ///
    /// # 错误
    /// 长度字段超出输入、名称不是 UTF-8 或数据不足时返回
    /// [`VeilError::InvalidFormat`]。
    pub fn from_bytes(bytes: &[u8]) -> Result<(Self, usize), VeilError> {
        if bytes.len() < 2 {
            return Err(VeilError::InvalidFormat("文件条目头部不完整".to_string()));
        }

        // 先读名称长度，再据此确定名称、数据长度字段和总消耗量。
        let name_len = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;

        if bytes.len() < 2 + name_len + 8 {
            return Err(VeilError::InvalidFormat("文件条目头部不完整".to_string()));
        }

        let name = String::from_utf8(bytes[2..2 + name_len].to_vec())
            .map_err(|e| VeilError::InvalidFormat(format!("文件名不是有效的 UTF-8: {}", e)))?;

        let data_size = u64::from_le_bytes([
            bytes[2 + name_len],
            bytes[2 + name_len + 1],
            bytes[2 + name_len + 2],
            bytes[2 + name_len + 3],
            bytes[2 + name_len + 4],
            bytes[2 + name_len + 5],
            bytes[2 + name_len + 6],
            bytes[2 + name_len + 7],
        ]);

        let consumed = 2 + name_len + 8;

        Ok((Self { name, data_size }, consumed))
    }
}

/// 将工作区目录写入单个 `.veil` 文件。
pub struct ContainerPacker {
    /// 待创建或覆盖的目标打包文件路径。
    output_path: std::path::PathBuf,
}

impl ContainerPacker {
    /// 为指定输出路径创建打包器。
    pub fn new(output_path: impl AsRef<Path>) -> Self {
        Self {
            output_path: output_path.as_ref().to_path_buf(),
        }
    }

    /// 将工作区加密文件和元数据写入打包文件。
    ///
    /// 方法先写入占位头部，再依次写入加密元数据和每个条目；完成后回填包含实际
    /// 元数据长度与文件数量的头部。
    ///
    /// # 参数
    /// - `workspace_path`：包含 `.veil-meta` 和加密文件的工作区目录。
    /// - `metadata`：解密后的清单，用于确定文件顺序和原始名称。
    /// - `encrypted_metadata`：工作区 `.veil-meta` 的完整字节，包含明文头。
    ///
    /// # 错误
    /// 输出文件创建、条目读取、写入或头部回填失败时返回 I/O 错误。
    pub fn pack(
        &self,
        workspace_path: impl AsRef<Path>,
        metadata: &MetaData,
        encrypted_metadata: &[u8],
    ) -> Result<(), VeilError> {
        let workspace_path = workspace_path.as_ref();

        // 打包文件整体覆写，输出路径冲突由命令层提前拒绝。
        let mut output = File::create(&self.output_path)?;

        // 头部包含后续总长度，先写占位值，完成文件遍历后再回填。
        let placeholder_header = ContainerHeader::new(0, 0);
        output.write_all(&placeholder_header.to_bytes())?;

        // 元数据直接复用工作区中的加密字节，不在打包阶段重复加密。
        let metadata_size = encrypted_metadata.len() as u32;
        output.write_all(encrypted_metadata)?;

        let file_count = metadata.files.len() as u32;

        for file_entry in &metadata.files {
            // 每个条目由头部和一段已加密文件字节组成，顺序与元数据清单一致。
            let encrypted_file_path = workspace_path.join(&file_entry.encrypted_name);
            let encrypted_data = std::fs::read(&encrypted_file_path)?;

            let entry_header = FileEntryHeader {
                name: file_entry.original_name.clone(),
                data_size: encrypted_data.len() as u64,
            };
            output.write_all(&entry_header.to_bytes())?;

            output.write_all(&encrypted_data)?;
        }

        // 回到文件开头写入真实 metadata_size 和 file_count。
        output.seek(SeekFrom::Start(0))?;
        let header = ContainerHeader::new(metadata_size, file_count);
        output.write_all(&header.to_bytes())?;

        Ok(())
    }
}

/// 从 `.veil` 打包文件读取元数据和文件内容。
pub struct ContainerUnpacker {
    /// 待读取的打包文件路径。
    container_path: std::path::PathBuf,
}

impl ContainerUnpacker {
    /// 为指定打包文件创建解包器。
    pub fn new(container_path: impl AsRef<Path>) -> Self {
        Self {
            container_path: container_path.as_ref().to_path_buf(),
        }
    }

    /// 只读取包内加密元数据，不创建工作区或写入文件。
    ///
    /// # 错误
    /// 打包文件无法打开、头部无效或元数据长度不足时返回错误。
    pub fn read_encrypted_metadata(&self) -> Result<Vec<u8>, VeilError> {
        let mut file = File::open(&self.container_path)?;

        // 头部长度固定 22 字节，元数据长度从头部字段取得。
        let mut header_buf = vec![0u8; 22];
        file.read_exact(&mut header_buf)?;
        let header = ContainerHeader::from_bytes(&header_buf)?;

        let mut metadata_buf = vec![0u8; header.metadata_size as usize];
        file.read_exact(&mut metadata_buf)?;
        Ok(metadata_buf)
    }

    /// 将打包文件中的加密条目写入工作区。
    ///
    /// 条目以元数据中的原始名称落盘，调用方随后需依据解密后的清单将其重命名为
    /// 加密文件名。方法最后返回 `.veil-meta` 的完整字节。
    ///
    /// # 错误
    /// 目录创建、条目解析、内容读取或文件写入失败时返回错误。
    pub fn unpack(&self, workspace_path: impl AsRef<Path>) -> Result<Vec<u8>, VeilError> {
        let workspace_path = workspace_path.as_ref();

        // 所有读取按文件布局顺序前进：头部、元数据、条目头部、条目数据。
        let mut file = File::open(&self.container_path)?;

        let mut header_buf = vec![0u8; 22];
        file.read_exact(&mut header_buf)?;
        let header = ContainerHeader::from_bytes(&header_buf)?;

        let mut metadata_buf = vec![0u8; header.metadata_size as usize];
        file.read_exact(&mut metadata_buf)?;

        std::fs::create_dir_all(workspace_path)?;

        for _ in 0..header.file_count {
            // 当前格式使用固定缓冲区读取变长条目头，名称头必须能放入该缓冲区。
            let mut entry_header_buf = vec![0u8; 1024];
            file.read_exact(&mut entry_header_buf[..2])?;

            // 名称长度决定条目头剩余部分，数据长度紧随名称之后。
            let name_len = u16::from_le_bytes([entry_header_buf[0], entry_header_buf[1]]) as usize;
            file.read_exact(&mut entry_header_buf[2..2 + name_len + 8])?;

            let (entry_header, _) =
                FileEntryHeader::from_bytes(&entry_header_buf[..2 + name_len + 8])?;

            // 文件内容已加密，解包阶段只负责原样落盘。
            let mut file_data = vec![0u8; entry_header.data_size as usize];
            file.read_exact(&mut file_data)?;

            // 先按原始名称落盘，上层再依据解密元数据重命名为加密文件名。
            let output_path = workspace_path.join(&entry_header.name);
            std::fs::write(&output_path, &file_data)?;
        }

        Ok(metadata_buf)
    }
}
