//! 第 2 版便携包：加密数据库快照与独立密文，拒绝旧元数据格式。
use crate::{
    database,
    error::{Result, VeilError},
};
use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
pub const VEIL_PACKAGE_MAGIC: &[u8; 8] = b"VEILPKG\0";
pub const VEIL_PACKAGE_VERSION: u16 = 2;
const MAX_DB: u32 = 512 * 1024 * 1024;
fn invalid(s: &str) -> VeilError {
    VeilError::InvalidFormat(s.into())
}
#[derive(Debug, Clone)]
pub struct VeilPackageHeader {
    pub version: u16,
    pub header_size: u32,
    pub metadata_size: u32,
    pub file_count: u32,
}
impl VeilPackageHeader {
    pub fn new(metadata_size: u32, file_count: u32) -> Self {
        Self {
            version: 2,
            header_size: 22,
            metadata_size,
            file_count,
        }
    }
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(VEIL_PACKAGE_MAGIC);
        b.extend_from_slice(&self.version.to_le_bytes());
        for n in [self.header_size, self.metadata_size, self.file_count] {
            b.extend_from_slice(&n.to_le_bytes());
        }
        b
    }
    pub fn from_bytes(b: &[u8]) -> Result<Self> {
        if b.len() < 22 || &b[..8] != VEIL_PACKAGE_MAGIC {
            return Err(invalid("仅支持 veil pack 生成的新格式打包文件"));
        }
        let h = Self {
            version: u16::from_le_bytes(b[8..10].try_into().unwrap()),
            header_size: u32::from_le_bytes(b[10..14].try_into().unwrap()),
            metadata_size: u32::from_le_bytes(b[14..18].try_into().unwrap()),
            file_count: u32::from_le_bytes(b[18..22].try_into().unwrap()),
        };
        if h.version != 2
            || h.header_size != 22
            || h.metadata_size < 4096
            || !h.metadata_size.is_multiple_of(4096)
            || h.metadata_size > MAX_DB
        {
            return Err(invalid(
                "仅支持 veil pack 生成的新格式打包文件；格式或数据库大小无效",
            ));
        }
        Ok(h)
    }
}
pub struct VeilPackageWriter {
    output: PathBuf,
    temp: tempfile::NamedTempFile,
    count: u32,
    names: HashSet<String>,
}
impl VeilPackageWriter {
    pub fn new(output: impl AsRef<Path>, database: &[u8]) -> Result<Self> {
        let output = output.as_ref();
        let parent = output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let size = u32::try_from(database.len()).map_err(|_| invalid("数据库快照过大"))?;
        if size == 0 || size > MAX_DB {
            return Err(invalid("数据库快照大小无效"));
        }
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(&VeilPackageHeader::new(size, 0).to_bytes())?;
        temp.write_all(database)?;
        Ok(Self {
            output: output.into(),
            temp,
            count: 0,
            names: HashSet::new(),
        })
    }
    pub fn add_file(&mut self, name: &str, path: &Path) -> Result<()> {
        validate_name(name)?;
        if !self.names.insert(name.into()) {
            return Err(invalid("打包密文名称重复"));
        }
        let bytes = name.as_bytes();
        let len = u16::try_from(bytes.len()).map_err(|_| invalid("名称过长"))?;
        let mut source = File::open(path)?;
        let size = source.metadata()?.len();
        self.temp.write_all(&len.to_le_bytes())?;
        self.temp.write_all(bytes)?;
        self.temp.write_all(&size.to_le_bytes())?;
        if std::io::copy(&mut Read::by_ref(&mut source).take(size), &mut self.temp)? != size {
            return Err(invalid("源密文长度变化"));
        }
        self.count = self
            .count
            .checked_add(1)
            .ok_or_else(|| invalid("文件过多"))?;
        Ok(())
    }
    pub fn finish(self) -> Result<()> {
        self.publish(true)
    }
    pub fn finish_without_overwrite(self) -> Result<()> {
        self.publish(false)
    }
    fn publish(mut self, overwrite: bool) -> Result<()> {
        self.temp.seek(SeekFrom::Start(18))?;
        self.temp.write_all(&self.count.to_le_bytes())?;
        self.temp.as_file().sync_all()?;
        if overwrite {
            self.temp
                .persist(&self.output)
                .map_err(|e| VeilError::Io(e.error))?;
        } else {
            self.temp
                .persist_noclobber(&self.output)
                .map_err(|e| VeilError::Io(e.error))?;
        }
        crate::fsutil::sync_directory(
            self.output
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?;
        Ok(())
    }
}
pub struct VeilPasswordVerifier {
    database: tempfile::NamedTempFile,
}
impl VeilPasswordVerifier {
    pub fn matches_password(&self, password: &str) -> bool {
        database::open(self.database.path(), Some(password), false).is_ok()
    }
}
pub struct VeilUnpacker {
    path: PathBuf,
}
impl VeilUnpacker {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().into(),
        }
    }
    pub fn read_encrypted_metadata(&self) -> Result<Vec<u8>> {
        let mut file = File::open(&self.path)?;
        let header = read_header(&mut file)?;
        let remaining = file.metadata()?.len().saturating_sub(22);
        if header.metadata_size as u64 > remaining {
            return Err(invalid("包内数据库被截断"));
        }
        let mut bytes = vec![0; header.metadata_size as usize];
        file.read_exact(&mut bytes)?;
        Ok(bytes)
    }
    pub fn read_password_verifier(&self) -> Result<VeilPasswordVerifier> {
        let mut database = tempfile::NamedTempFile::new()?;
        database.write_all(&self.read_encrypted_metadata()?)?;
        database.as_file().sync_all()?;
        Ok(VeilPasswordVerifier { database })
    }
    /// 输出只包含加密数据；名称、长度、重复条目及尾部均严格校验。
    pub fn unpack(&self, directory: impl AsRef<Path>) -> Result<Vec<u8>> {
        let directory = directory.as_ref();
        let bytes = self.read_encrypted_metadata()?;
        let mut file = File::open(&self.path)?;
        let header = read_header(&mut file)?;
        file.seek(SeekFrom::Current(header.metadata_size as i64))?;
        std::fs::create_dir_all(directory)?;
        let mut names = HashSet::new();
        for _ in 0..header.file_count {
            let mut len = [0; 2];
            file.read_exact(&mut len)?;
            let len = u16::from_le_bytes(len) as usize;
            if len == 0 || len > 128 {
                return Err(invalid("包内文件名称无效"));
            }
            let mut name = vec![0; len];
            file.read_exact(&mut name)?;
            let name = String::from_utf8(name).map_err(|_| invalid("包内名称不是 UTF-8"))?;
            validate_name(&name)?;
            if !names.insert(name.clone()) {
                return Err(invalid("包内密文名称重复"));
            }
            let mut size = [0; 8];
            file.read_exact(&mut size)?;
            let size = u64::from_le_bytes(size);
            if size
                > file
                    .metadata()?
                    .len()
                    .saturating_sub(file.stream_position()?)
            {
                return Err(invalid("包内密文被截断"));
            }
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(directory.join(&name))?;
            if std::io::copy(&mut Read::by_ref(&mut file).take(size), &mut output)? != size {
                return Err(invalid("包内密文被截断"));
            }
            output.sync_all()?;
        }
        let mut tail = [0; 1];
        if file.read(&mut tail)? != 0 {
            return Err(invalid("打包文件存在多余数据"));
        }
        let mut database = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(database::CONTAINER_DB))?;
        database.write_all(&bytes)?;
        database.sync_all()?;
        crate::fsutil::sync_directory(directory)?;
        Ok(bytes)
    }
}
fn read_header(file: &mut File) -> Result<VeilPackageHeader> {
    let mut bytes = [0; 22];
    file.read_exact(&mut bytes)?;
    VeilPackageHeader::from_bytes(&bytes)
}
fn validate_name(name: &str) -> Result<()> {
    let base = name
        .strip_suffix(".enc")
        .ok_or_else(|| invalid("仅允许密文条目"))?;
    let mut parts = base.split('_');
    if !parts.next().is_some_and(crate::metadata::is_uuid_v4)
        || !parts.next().is_some_and(crate::metadata::is_uuid_v4)
        || parts.next().is_some()
    {
        return Err(invalid("密文名称不符合新格式"));
    }
    Ok(())
}
