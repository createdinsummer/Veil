//! 工作区操作模块
//!
//! 提供工作区的创建、元数据读写、文件管理等功能

use crate::error::VeilError;
use crate::kdf;
use crate::metadata::{AlgorithmId, FileEntry, MetaData, MetaHeader};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305,
};
use std::fs;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// 工作区管理器
pub struct WorkspaceManager {
    /// 工作区根路径
    pub workspace_path: PathBuf,
}

impl WorkspaceManager {
    /// 创建工作区管理器
    pub fn new(workspace_path: PathBuf) -> Self {
        Self { workspace_path }
    }

    /// 初始化新容器
    pub fn init_container(
        &self,
        container_name: &str,
        workspace_type: &str,
        password: &str,
    ) -> Result<(), VeilError> {
        // 创建容器目录
        fs::create_dir_all(&self.workspace_path).map_err(|e| {
            VeilError::WorkspaceError(format!("创建容器目录失败: {}", e))
        })?;

        // 生成随机 salt 和 nonce
        let salt = generate_random_bytes::<32>();
        let nonce = generate_random_bytes::<12>();

        // 创建初始元数据
        let meta_data = MetaData::new(container_name.to_string(), workspace_type.to_string());

        // 创建头部
        let header = MetaHeader::new(salt, nonce, AlgorithmId::Aes256Gcm);

        // 派生密钥
        let master_key = derive_master_key(password, &salt)?;

        // 加密并写入元数据
        self.write_meta(&header, &meta_data, &master_key)?;

        Ok(())
    }

    /// 读取元数据
    pub fn read_meta(&self, password: &str) -> Result<MetaData, VeilError> {
        let meta_path = self.workspace_path.join(".veil-meta");

        if !meta_path.exists() {
            return Err(VeilError::InvalidFormat("元数据文件不存在".to_string()));
        }

        let bytes = fs::read(&meta_path).map_err(|e| {
            VeilError::Io(e)
        })?;

        // 解析头部
        let header = MetaHeader::from_bytes(&bytes)?;

        // 派生密钥
        let master_key = derive_master_key(password, &header.salt)?;

        // 解密数据
        let header_len = MetaHeader::header_len(&bytes)?;
        let encrypted_data = &bytes[header_len..];

        let decrypted = decrypt_aes256gcm(&master_key, encrypted_data, &header.nonce)?;

        // 解析 JSON
        let meta_data = MetaData::from_json(&decrypted)?;

        Ok(meta_data)
    }

    /// 写入元数据
    fn write_meta(
        &self,
        header: &MetaHeader,
        meta_data: &MetaData,
        master_key: &[u8; 32],
    ) -> Result<(), VeilError> {
        let meta_path = self.workspace_path.join(".veil-meta");

        // 序列化头部
        let header_bytes = header.to_bytes()?;

        // 序列化并加密数据
        let json_bytes = meta_data.to_json()?;
        let encrypted = encrypt_aes256gcm(master_key, &json_bytes, &header.nonce)?;

        // 组合数据
        let mut file_data = header_bytes;
        file_data.extend_from_slice(&encrypted);

        // 原子写入
        atomic_write(&meta_path, &file_data)?;

        Ok(())
    }

    /// 更新元数据
    pub fn update_meta(
        &self,
        password: &str,
        update_fn: impl FnOnce(&mut MetaData),
    ) -> Result<(), VeilError> {
        // 读取现有元数据
        let mut meta_data = self.read_meta(password)?;

        // 执行更新
        update_fn(&mut meta_data);

        // 读取头部（获取 salt 和 nonce）
        let meta_path = self.workspace_path.join(".veil-meta");
        let bytes = fs::read(&meta_path)?;
        let header = MetaHeader::from_bytes(&bytes)?;

        // 派生密钥
        let master_key = derive_master_key(password, &header.salt)?;

        // 写回
        self.write_meta(&header, &meta_data, &master_key)?;

        Ok(())
    }

    /// 添加文件到容器
    pub fn add_file(
        &self,
        file_path: &Path,
        password: &str,
    ) -> Result<String, VeilError> {
        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| VeilError::WorkspaceError("无效的文件名".to_string()))?;

        // 读取文件
        let file_data = fs::read(file_path)?;
        let file_size = file_data.len() as u64;

        // 生成随机加密文件名和 nonce
        let encrypted_name = format!("{}.enc", generate_random_id());
        let file_nonce = generate_random_bytes::<12>();

        // 读取元数据获取 salt
        let meta_path = self.workspace_path.join(".veil-meta");
        let meta_bytes = fs::read(&meta_path)?;
        let header = MetaHeader::from_bytes(&meta_bytes)?;

        // 派生密钥
        let master_key = derive_master_key(password, &header.salt)?;

        // 加密文件
        let encrypted_data = encrypt_aes256gcm(&master_key, &file_data, &file_nonce)?;

        // 写入加密文件
        let encrypted_path = self.workspace_path.join(&encrypted_name);
        fs::write(&encrypted_path, encrypted_data)?;

        // 更新元数据
        self.update_meta(password, |meta| {
            meta.add_file(FileEntry::new(
                encrypted_name.clone(),
                file_name.to_string(),
                file_size,
                file_nonce,
            ));
        })?;

        Ok(encrypted_name)
    }

    /// 从容器提取文件
    pub fn extract_file(
        &self,
        original_name: &str,
        output_path: &Path,
        password: &str,
    ) -> Result<(), VeilError> {
        // 读取元数据
        let meta_data = self.read_meta(password)?;

        // 查找文件
        let entry = meta_data.find_file(original_name).ok_or_else(|| {
            VeilError::ContainerNotFound(format!("文件 '{}' 不存在", original_name))
        })?;

        // 读取加密文件
        let encrypted_path = self.workspace_path.join(&entry.encrypted_name);
        let encrypted_data = fs::read(&encrypted_path)?;

        // 获取 salt
        let meta_path = self.workspace_path.join(".veil-meta");
        let meta_bytes = fs::read(&meta_path)?;
        let header = MetaHeader::from_bytes(&meta_bytes)?;

        // 派生密钥
        let master_key = derive_master_key(password, &header.salt)?;

        // 解密文件
        let decrypted = decrypt_aes256gcm(&master_key, &encrypted_data, &entry.nonce)?;

        // 写入输出文件
        fs::write(output_path, decrypted)?;

        Ok(())
    }

    /// 删除文件
    pub fn remove_file(
        &self,
        original_name: &str,
        password: &str,
    ) -> Result<(), VeilError> {
        // 读取元数据
        let meta_data = self.read_meta(password)?;

        // 查找文件
        let entry = meta_data.find_file(original_name).ok_or_else(|| {
            VeilError::ContainerNotFound(format!("文件 '{}' 不存在", original_name))
        })?;

        let encrypted_name = entry.encrypted_name.clone();

        // 删除加密文件
        let encrypted_path = self.workspace_path.join(&encrypted_name);
        fs::remove_file(&encrypted_path)?;

        // 更新元数据
        self.update_meta(password, |meta| {
            meta.remove_file(&encrypted_name);
        })?;

        Ok(())
    }

    /// 列出所有文件
    pub fn list_files(&self, password: &str) -> Result<Vec<FileEntry>, VeilError> {
        let meta_data = self.read_meta(password)?;
        Ok(meta_data.files.clone())
    }

    /// 修改容器密码
    pub fn change_password(&self, old_password: &str, new_password: &str) -> Result<(), VeilError> {
        // 读取当前元数据（验证旧密码）
        let meta_path = self.workspace_path.join(".veil-meta");
        let meta_bytes = fs::read(&meta_path)?;

        let old_header = MetaHeader::from_bytes(&meta_bytes)?;
        let old_master_key = derive_master_key(old_password, &old_header.salt)?;

        // 解密元数据验证密码
        let header_len = MetaHeader::header_len(&meta_bytes)?;
        let encrypted_data = &meta_bytes[header_len..];
        let decrypted = decrypt_aes256gcm(&old_master_key, encrypted_data, &old_header.nonce)?;
        let meta_data = MetaData::from_json(&decrypted)?;

        // 生成新的 salt 和 nonce
        let new_salt = generate_random_bytes::<32>();
        let new_nonce = generate_random_bytes::<12>();

        // 使用新密码派生密钥
        let new_master_key = derive_master_key(new_password, &new_salt)?;

        // 重新加密元数据
        let json_bytes = meta_data.to_json()?;
        let new_encrypted_data = encrypt_aes256gcm(&new_master_key, &json_bytes, &new_nonce)?;

        // 创建新头部
        let new_header = MetaHeader::new(new_salt, new_nonce, AlgorithmId::Aes256Gcm);

        // 写入新元数据
        let mut new_meta_bytes = new_header.to_bytes()?;
        new_meta_bytes.extend_from_slice(&new_encrypted_data);
        atomic_write(&meta_path, &new_meta_bytes)?;

        // 重新加密所有文件（每个文件有自己的 nonce，但使用容器 salt）
        for file_entry in &meta_data.files {
            let encrypted_path = self.workspace_path.join(&file_entry.encrypted_name);

            // 用旧密码解密
            let encrypted_file_data = fs::read(&encrypted_path)?;
            let plaintext = decrypt_aes256gcm(&old_master_key, &encrypted_file_data, &file_entry.nonce)?;

            // 用新密码重新加密（保持相同的 nonce）
            let new_encrypted = encrypt_aes256gcm(&new_master_key, &plaintext, &file_entry.nonce)?;

            // 原子写入
            atomic_write(&encrypted_path, &new_encrypted)?;
        }

        Ok(())
    }

    /// 重命名容器内的文件
    pub fn rename_file(&self, from: &str, to: &str, password: &str) -> Result<(), VeilError> {
        // 读取元数据头部和数据
        let meta_path = self.workspace_path.join(".veil-meta");
        let meta_bytes = fs::read(&meta_path)?;

        let header = MetaHeader::from_bytes(&meta_bytes)?;
        let master_key = derive_master_key(password, &header.salt)?;

        // 解密元数据
        let header_len = MetaHeader::header_len(&meta_bytes)?;
        let encrypted_data = &meta_bytes[header_len..];
        let decrypted = decrypt_aes256gcm(&master_key, encrypted_data, &header.nonce)?;
        let mut meta_data = MetaData::from_json(&decrypted)?;

        // 查找源文件
        let file_entry = meta_data.find_file(from)
            .ok_or_else(|| VeilError::FileNotFound(from.to_string()))?
            .clone();

        // 检查目标文件是否已存在
        if meta_data.find_file(to).is_some() {
            return Err(VeilError::FileAlreadyExists(to.to_string()));
        }

        // 移除旧条目
        meta_data.remove_file(&file_entry.encrypted_name);

        // 创建新条目（保持相同的加密文件名、nonce 等）
        let new_entry = FileEntry {
            encrypted_name: file_entry.encrypted_name,
            original_name: to.to_string(),
            size: file_entry.size,
            nonce: file_entry.nonce,
            encrypted_at: file_entry.encrypted_at,
            hash: file_entry.hash,
        };

        // 添加新条目
        meta_data.add_file(new_entry);

        // 写回元数据
        self.write_meta(&header, &meta_data, &master_key)?;

        Ok(())
    }
}

/// 派生主密钥
fn derive_master_key(password: &str, salt: &[u8; 32]) -> Result<Zeroizing<[u8; 32]>, VeilError> {
    // 使用 Argon2id 派生密钥
    let mut key = Zeroizing::new([0u8; 32]);
    kdf::derive_key(password.as_bytes(), salt, &mut *key)
        .map_err(|e| VeilError::KeyDerivationError(format!("密钥派生失败: {:?}", e)))?;
    Ok(key)
}

/// AES-256-GCM 加密
fn encrypt_aes256gcm(key: &[u8; 32], plaintext: &[u8], nonce: &[u8; 12]) -> Result<Vec<u8>, VeilError> {
    use chacha20poly1305::aead::generic_array::GenericArray;

    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(key));
    let nonce_ga = GenericArray::from_slice(nonce);

    cipher
        .encrypt(nonce_ga, plaintext)
        .map_err(|e| VeilError::EncryptionError(format!("加密失败: {}", e)))
}

/// AES-256-GCM 解密
fn decrypt_aes256gcm(key: &[u8; 32], ciphertext: &[u8], nonce: &[u8; 12]) -> Result<Vec<u8>, VeilError> {
    use chacha20poly1305::aead::generic_array::GenericArray;

    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(key));
    let nonce_ga = GenericArray::from_slice(nonce);

    cipher
        .decrypt(nonce_ga, ciphertext)
        .map_err(|e| VeilError::DecryptionError(format!("解密失败（密码错误或数据损坏）: {}", e)))
}

/// 生成随机字节
fn generate_random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::getrandom(&mut bytes).expect("生成随机数失败");
    bytes
}

/// 生成随机 ID（16 字符十六进制）
fn generate_random_id() -> String {
    let bytes = generate_random_bytes::<8>();
    hex::encode(bytes)
}

/// 原子写入文件
fn atomic_write(path: &Path, data: &[u8]) -> Result<(), VeilError> {
    let temp_path = path.with_extension("tmp");

    fs::write(&temp_path, data)?;
    fs::rename(&temp_path, path)?;

    Ok(())
}
