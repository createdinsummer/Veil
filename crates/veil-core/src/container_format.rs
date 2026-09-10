//! .veil 容器文件格式
//!
//! 容器文件用于打包工作区，便于分享、备份和传输
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
use std::io::{Read, Write, Seek, SeekFrom};
use std::path::Path;

/// 容器文件魔数
pub const CONTAINER_MAGIC: &[u8; 8] = b"VEILPKG\0";

/// 容器文件版本
pub const CONTAINER_VERSION: u16 = 1;

/// 容器文件头部
#[derive(Debug, Clone)]
pub struct ContainerHeader {
    /// 版本号
    pub version: u16,
    /// 头部大小
    pub header_size: u32,
    /// 元数据大小
    pub metadata_size: u32,
    /// 文件数量
    pub file_count: u32,
}

impl ContainerHeader {
    /// 创建新的容器头部
    pub fn new(metadata_size: u32, file_count: u32) -> Self {
        Self {
            version: CONTAINER_VERSION,
            header_size: 22, // magic(8) + version(2) + header_size(4) + metadata_size(4) + file_count(4)
            metadata_size,
            file_count,
        }
    }

    /// 序列化为字节
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.header_size as usize);

        // Magic
        buf.extend_from_slice(CONTAINER_MAGIC);

        // Version
        buf.extend_from_slice(&self.version.to_le_bytes());

        // Header size
        buf.extend_from_slice(&self.header_size.to_le_bytes());

        // Metadata size
        buf.extend_from_slice(&self.metadata_size.to_le_bytes());

        // File count
        buf.extend_from_slice(&self.file_count.to_le_bytes());

        buf
    }

    /// 从字节反序列化
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, VeilError> {
        if bytes.len() < 22 {
            return Err(VeilError::InvalidFormat("容器文件头部不完整".to_string()));
        }

        // 验证魔数
        if &bytes[0..8] != CONTAINER_MAGIC {
            return Err(VeilError::InvalidFormat("无效的容器文件魔数".to_string()));
        }

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

/// 文件条目头部
#[derive(Debug, Clone)]
pub struct FileEntryHeader {
    /// 文件名
    pub name: String,
    /// 数据大小
    pub data_size: u64,
}

impl FileEntryHeader {
    /// 序列化为字节
    pub fn to_bytes(&self) -> Vec<u8> {
        let name_bytes = self.name.as_bytes();
        let name_len = name_bytes.len() as u16;

        let mut buf = Vec::with_capacity(2 + name_bytes.len() + 8);

        // Name length
        buf.extend_from_slice(&name_len.to_le_bytes());

        // Name
        buf.extend_from_slice(name_bytes);

        // Data size
        buf.extend_from_slice(&self.data_size.to_le_bytes());

        buf
    }

    /// 从字节反序列化
    pub fn from_bytes(bytes: &[u8]) -> Result<(Self, usize), VeilError> {
        if bytes.len() < 2 {
            return Err(VeilError::InvalidFormat("文件条目头部不完整".to_string()));
        }

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

/// 容器打包器
pub struct ContainerPacker {
    output_path: std::path::PathBuf,
}

impl ContainerPacker {
    /// 创建新的打包器
    pub fn new(output_path: impl AsRef<Path>) -> Self {
        Self {
            output_path: output_path.as_ref().to_path_buf(),
        }
    }

    /// 打包工作区到容器文件
    pub fn pack(
        &self,
        workspace_path: impl AsRef<Path>,
        metadata: &MetaData,
        encrypted_metadata: &[u8],
    ) -> Result<(), VeilError> {
        let workspace_path = workspace_path.as_ref();

        // 创建输出文件
        let mut output = File::create(&self.output_path)?;

        // 写入占位头部（稍后回填）
        let placeholder_header = ContainerHeader::new(0, 0);
        output.write_all(&placeholder_header.to_bytes())?;

        // 写入加密的元数据
        let metadata_size = encrypted_metadata.len() as u32;
        output.write_all(encrypted_metadata)?;

        // 写入每个文件
        let file_count = metadata.files.len() as u32;

        for file_entry in &metadata.files {
            // 读取加密文件
            let encrypted_file_path = workspace_path.join(&file_entry.encrypted_name);
            let encrypted_data = std::fs::read(&encrypted_file_path)?;

            // 写入文件条目头部
            let entry_header = FileEntryHeader {
                name: file_entry.original_name.clone(),
                data_size: encrypted_data.len() as u64,
            };
            output.write_all(&entry_header.to_bytes())?;

            // 写入文件数据
            output.write_all(&encrypted_data)?;
        }

        // 回填正确的头部
        output.seek(SeekFrom::Start(0))?;
        let header = ContainerHeader::new(metadata_size, file_count);
        output.write_all(&header.to_bytes())?;

        Ok(())
    }
}

/// 容器解包器
pub struct ContainerUnpacker {
    container_path: std::path::PathBuf,
}

impl ContainerUnpacker {
    /// 创建新的解包器
    pub fn new(container_path: impl AsRef<Path>) -> Self {
        Self {
            container_path: container_path.as_ref().to_path_buf(),
        }
    }

    /// 解包容器文件到工作区
    pub fn unpack(
        &self,
        workspace_path: impl AsRef<Path>,
    ) -> Result<Vec<u8>, VeilError> {
        let workspace_path = workspace_path.as_ref();

        // 打开容器文件
        let mut file = File::open(&self.container_path)?;

        // 读取头部
        let mut header_buf = vec![0u8; 22];
        file.read_exact(&mut header_buf)?;
        let header = ContainerHeader::from_bytes(&header_buf)?;

        // 读取加密的元数据
        let mut metadata_buf = vec![0u8; header.metadata_size as usize];
        file.read_exact(&mut metadata_buf)?;

        // 创建工作区目录
        std::fs::create_dir_all(workspace_path)?;

        // 读取并解包每个文件
        for _ in 0..header.file_count {
            // 读取文件条目头部
            let mut entry_header_buf = vec![0u8; 1024]; // 临时缓冲区
            file.read_exact(&mut entry_header_buf[..2])?; // 先读取 name_len

            let name_len = u16::from_le_bytes([entry_header_buf[0], entry_header_buf[1]]) as usize;
            file.read_exact(&mut entry_header_buf[2..2 + name_len + 8])?;

            let (entry_header, _) = FileEntryHeader::from_bytes(&entry_header_buf[..2 + name_len + 8])?;

            // 读取文件数据
            let mut file_data = vec![0u8; entry_header.data_size as usize];
            file.read_exact(&mut file_data)?;

            // 写入工作区（保持加密状态）
            let output_path = workspace_path.join(&entry_header.name);
            std::fs::write(&output_path, &file_data)?;
        }

        // 返回加密的元数据，由调用者解密和写入
        Ok(metadata_buf)
    }
}
