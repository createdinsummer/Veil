//! 工作区容器的高层读写操作。
//!
//! [`WorkspaceManager`] 把 `.veil-meta` 与同目录下的独立加密文件组合成完整容器。
//! 元数据和文件内容均由 Argon2id 派生出的主密钥保护；当前实现使用
//! ChaCha20-Poly1305，并为每个文件生成独立 nonce。`.veil-meta` 中记录对应的
//! 算法 ID，具体值由 [`MetaHeader`] 持久化。

use crate::error::VeilError;
use crate::kdf;
use crate::metadata::{AlgorithmId, FileEntry, MetaData, MetaHeader};
use chacha20poly1305::{
    ChaCha20Poly1305,
    aead::{Aead, KeyInit},
};
use std::fs;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// 对单个工作区容器执行初始化和文件操作。
pub struct WorkspaceManager {
    /// 包含 `.veil-meta` 和加密文件的工作区根路径。
    pub workspace_path: PathBuf,
}

impl WorkspaceManager {
    /// 为指定工作区路径创建工作区管理器。
    ///
    /// 构造过程只保存路径，不访问文件系统。
    pub fn new(workspace_path: PathBuf) -> Self {
        Self { workspace_path }
    }

    /// 为新容器生成稳定 ID 并初始化工作区。
    ///
    /// # 错误
    /// 目录创建、密钥派生或元数据写入失败时返回错误。
    pub fn init_container(
        &self,
        container_name: &str,
        workspace_type: &str,
        password: &str,
    ) -> Result<MetaData, VeilError> {
        self.init_container_with_id(
            &crate::metadata::generate_veil_id(),
            container_name,
            workspace_type,
            password,
        )
    }

    /// 使用调用方指定的稳定 ID 初始化新容器。
    ///
    /// 方法创建目录和随机盐/nonce，生成初始元数据，并写入加密的 `.veil-meta`。
    ///
    /// # 错误
    /// 目录创建、随机数生成、密钥派生或元数据写入失败时返回错误。
    pub fn init_container_with_id(
        &self,
        veil_id: &str,
        container_name: &str,
        workspace_type: &str,
        password: &str,
    ) -> Result<MetaData, VeilError> {
        fs::create_dir_all(&self.workspace_path)
            .map_err(|e| VeilError::WorkspaceError(format!("创建容器目录失败: {}", e)))?;

        // salt 决定主密钥，nonce 只保护首份元数据；二者都写入明文头部。
        let salt = generate_random_bytes::<32>();
        let nonce = generate_random_bytes::<12>();

        let meta_data = MetaData::with_veil_id(
            veil_id.to_string(),
            container_name.to_string(),
            workspace_type.to_string(),
        );

        let header = MetaHeader::new(
            salt,
            nonce,
            AlgorithmId::Aes256Gcm,
            meta_data.veil_id.clone(),
            meta_data.container_name.clone(),
            meta_data.workspace_type.clone(),
        );

        let master_key = derive_master_key(password, &salt)?;

        self.write_meta(&header, &meta_data, &master_key)?;

        Ok(meta_data)
    }

    /// 使用密码读取并解密容器元数据。
    ///
    /// # 错误
    /// `.veil-meta` 不存在、头部无效、密钥派生失败、密码错误或密文损坏时返回错误。
    pub fn read_meta(&self, password: &str) -> Result<MetaData, VeilError> {
        let meta_path = self.workspace_path.join(".veil-meta");

        if !meta_path.exists() {
            return Err(VeilError::InvalidFormat("元数据文件不存在".to_string()));
        }

        let bytes = fs::read(&meta_path).map_err(|e| VeilError::Io(e))?;

        let header = MetaHeader::from_bytes(&bytes)?;

        let master_key = derive_master_key(password, &header.salt)?;

        // 明文头部之后全部是 ChaCha20-Poly1305 密文。
        let header_len = MetaHeader::header_len(&bytes)?;
        let encrypted_data = &bytes[header_len..];

        let decrypted = decrypt_aes256gcm(&master_key, encrypted_data, &header.nonce)?;

        let meta_data = MetaData::from_json(&decrypted)?;

        Ok(meta_data)
    }

    /// 不输入密码，只读取恢复所需的明文身份信息。
    ///
    /// # 错误
    /// `.veil-meta` 无法读取或头部格式无效时返回错误。
    pub fn read_meta_header(&self) -> Result<MetaHeader, VeilError> {
        let meta_path = self.workspace_path.join(".veil-meta");
        let bytes = fs::read(&meta_path).map_err(VeilError::Io)?;
        MetaHeader::from_bytes(&bytes)
    }

    /// 编码、加密并原子替换 `.veil-meta`。
    ///
    /// # 错误
    /// 头部或 JSON 序列化、内容加密、临时文件写入或重命名失败时返回错误。
    fn write_meta(
        &self,
        header: &MetaHeader,
        meta_data: &MetaData,
        master_key: &[u8; 32],
    ) -> Result<(), VeilError> {
        let meta_path = self.workspace_path.join(".veil-meta");

        let header_bytes = header.to_bytes()?;

        let json_bytes = meta_data.to_json()?;
        let encrypted = encrypt_aes256gcm(master_key, &json_bytes, &header.nonce)?;

        // .veil-meta 使用“明文 TLV 头部 + 密文 JSON”的连续布局。
        let mut file_data = header_bytes;
        file_data.extend_from_slice(&encrypted);

        atomic_write(&meta_path, &file_data)?;

        Ok(())
    }

    /// 读取元数据、应用调用方修改，再使用原头部参数写回。
    ///
    /// 更新闭包仅在内存中的 [`MetaData`] 上执行，持久化由本方法统一完成。
    ///
    /// # 错误
    /// 读取、密钥派生、序列化或写回失败时返回错误。
    pub fn update_meta(
        &self,
        password: &str,
        update_fn: impl FnOnce(&mut MetaData),
    ) -> Result<(), VeilError> {
        // 先在内存中完成调用方修改，避免闭包直接接触磁盘或密钥。
        let mut meta_data = self.read_meta(password)?;

        update_fn(&mut meta_data);

        // 复用原头部的 salt/nonce，保持当前密码下的密钥参数不变。
        let meta_path = self.workspace_path.join(".veil-meta");
        let bytes = fs::read(&meta_path)?;
        let header = MetaHeader::from_bytes(&bytes)?;

        let master_key = derive_master_key(password, &header.salt)?;

        self.write_meta(&header, &meta_data, &master_key)?;

        Ok(())
    }

    /// 加密并添加一个本地文件。
    ///
    /// 文件原名仅写入加密元数据；磁盘上使用随机加密名保存内容。方法先写入密文，
    /// 再通过 [`WorkspaceManager::update_meta`] 登记条目。
    ///
    /// # 返回
    /// 成功时返回新密文文件在工作区中的名称。
    ///
    /// # 错误
    /// 文件名为空、源文件读取、密钥派生、加密、密文写入或元数据更新失败时返回错误。
    pub fn add_file(&self, file_path: &Path, password: &str) -> Result<String, VeilError> {
        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| VeilError::WorkspaceError("无效的文件名".to_string()))?;

        let file_data = fs::read(file_path)?;
        let file_size = file_data.len() as u64;

        // 原始文件名只进入加密元数据；磁盘使用随机加密名隔离目录项。
        let encrypted_name = format!("{}.enc", generate_random_id());
        let file_nonce = generate_random_bytes::<12>();

        let meta_path = self.workspace_path.join(".veil-meta");
        let meta_bytes = fs::read(&meta_path)?;
        let header = MetaHeader::from_bytes(&meta_bytes)?;

        let master_key = derive_master_key(password, &header.salt)?;

        let encrypted_data = encrypt_aes256gcm(&master_key, &file_data, &file_nonce)?;

        // 先落盘密文，再发布指向它的元数据条目，避免元数据提前引用缺失文件。
        let encrypted_path = self.workspace_path.join(&encrypted_name);
        fs::write(&encrypted_path, encrypted_data)?;

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

    /// 解密指定原始路径的文件并写入输出位置。
    ///
    /// # 错误
    /// 元数据读取、文件查找、密文读取、密钥派生、解密或输出写入失败时返回错误。
    pub fn extract_file(
        &self,
        original_name: &str,
        output_path: &Path,
        password: &str,
    ) -> Result<(), VeilError> {
        let meta_data = self.read_meta(password)?;

        let entry = meta_data.find_file(original_name).ok_or_else(|| {
            VeilError::ContainerNotFound(format!("文件 '{}' 不存在", original_name))
        })?;

        // 元数据只暴露原始名称，实际读取必须通过条目保存的加密文件名。
        let encrypted_path = self.workspace_path.join(&entry.encrypted_name);
        let encrypted_data = fs::read(&encrypted_path)?;

        let meta_path = self.workspace_path.join(".veil-meta");
        let meta_bytes = fs::read(&meta_path)?;
        let header = MetaHeader::from_bytes(&meta_bytes)?;

        let master_key = derive_master_key(password, &header.salt)?;

        let decrypted = decrypt_aes256gcm(&master_key, &encrypted_data, &entry.nonce)?;

        fs::write(output_path, decrypted)?;

        Ok(())
    }

    /// 删除指定原始路径对应的密文文件，并从元数据中移除条目。
    ///
    /// 先删除磁盘密文，再更新元数据；任一步失败都会向上返回错误。
    ///
    /// # 错误
    /// 元数据读取、文件查找、密文删除或元数据更新失败时返回错误。
    pub fn remove_file(&self, original_name: &str, password: &str) -> Result<(), VeilError> {
        let meta_data = self.read_meta(password)?;

        let entry = meta_data.find_file(original_name).ok_or_else(|| {
            VeilError::ContainerNotFound(format!("文件 '{}' 不存在", original_name))
        })?;

        let encrypted_name = entry.encrypted_name.clone();

        // 先移除磁盘密文，再从元数据删除引用。
        let encrypted_path = self.workspace_path.join(&encrypted_name);
        fs::remove_file(&encrypted_path)?;

        self.update_meta(password, |meta| {
            meta.remove_file(&encrypted_name);
        })?;

        Ok(())
    }

    /// 使用密码读取并返回元数据中的所有文件条目。
    ///
    /// # 错误
    /// 元数据无法读取或密码错误时返回错误。
    pub fn list_files(&self, password: &str) -> Result<Vec<FileEntry>, VeilError> {
        let meta_data = self.read_meta(password)?;
        Ok(meta_data.files.clone())
    }

    /// 使用新密码重新保护元数据和所有文件内容。
    ///
    /// 方法先验证旧密码并解密清单，再用新盐和新 nonce 重写 `.veil-meta`，最后逐个
    /// 解密并重加密文件。文件自身已有的 nonce 保持不变。
    ///
    /// # 错误
    /// 旧密码错误、元数据或文件解密失败，以及新元数据或文件写入失败时返回错误。
    pub fn change_password(&self, old_password: &str, new_password: &str) -> Result<(), VeilError> {
        let meta_path = self.workspace_path.join(".veil-meta");
        let meta_bytes = fs::read(&meta_path)?;

        let old_header = MetaHeader::from_bytes(&meta_bytes)?;
        let old_master_key = derive_master_key(old_password, &old_header.salt)?;

        let header_len = MetaHeader::header_len(&meta_bytes)?;
        let encrypted_data = &meta_bytes[header_len..];
        let decrypted = decrypt_aes256gcm(&old_master_key, encrypted_data, &old_header.nonce)?;
        let meta_data = MetaData::from_json(&decrypted)?;

        let new_salt = generate_random_bytes::<32>();
        let new_nonce = generate_random_bytes::<12>();

        let new_master_key = derive_master_key(new_password, &new_salt)?;

        let json_bytes = meta_data.to_json()?;
        let new_encrypted_data = encrypt_aes256gcm(&new_master_key, &json_bytes, &new_nonce)?;

        let new_header = MetaHeader::new(
            new_salt,
            new_nonce,
            AlgorithmId::Aes256Gcm,
            old_header.veil_id.clone(),
            old_header.container_name.clone(),
            old_header.workspace_type.clone(),
        );

        // 新头部携带新 salt/nonce，后接使用新主密钥加密的 JSON。
        // 元数据先切换到新密钥，随后按清单逐个转换文件密文。
        let mut new_meta_bytes = new_header.to_bytes()?;
        new_meta_bytes.extend_from_slice(&new_encrypted_data);
        atomic_write(&meta_path, &new_meta_bytes)?;

        // 每个文件保留自身 nonce，仅替换由新密码派生的主密钥。
        for file_entry in &meta_data.files {
            let encrypted_path = self.workspace_path.join(&file_entry.encrypted_name);

            let encrypted_file_data = fs::read(&encrypted_path)?;
            let plaintext =
                decrypt_aes256gcm(&old_master_key, &encrypted_file_data, &file_entry.nonce)?;

            let new_encrypted = encrypt_aes256gcm(&new_master_key, &plaintext, &file_entry.nonce)?;

            atomic_write(&encrypted_path, &new_encrypted)?;
        }

        Ok(())
    }

    /// 修改文件在元数据中的原始路径，不移动或重加密其密文。
    ///
    /// 目标路径已存在时拒绝覆盖，并保留原条目的密文名称、nonce、时间和哈希字段。
    ///
    /// # 错误
    /// 源文件不存在、目标已存在、密码错误或元数据写回失败时返回错误。
    pub fn rename_file(&self, from: &str, to: &str, password: &str) -> Result<(), VeilError> {
        let meta_path = self.workspace_path.join(".veil-meta");
        let meta_bytes = fs::read(&meta_path)?;

        let header = MetaHeader::from_bytes(&meta_bytes)?;
        let master_key = derive_master_key(password, &header.salt)?;

        let header_len = MetaHeader::header_len(&meta_bytes)?;
        let encrypted_data = &meta_bytes[header_len..];
        let decrypted = decrypt_aes256gcm(&master_key, encrypted_data, &header.nonce)?;
        let mut meta_data = MetaData::from_json(&decrypted)?;

        let file_entry = meta_data
            .find_file(from)
            .ok_or_else(|| VeilError::FileNotFound(from.to_string()))?
            .clone();

        // 目标已存在时拒绝覆盖，保持 rename 语义可预测。
        if meta_data.find_file(to).is_some() {
            return Err(VeilError::FileAlreadyExists(to.to_string()));
        }

        // 新条目复用原加密文件和 nonce，只替换对外原始路径。
        meta_data.remove_file(&file_entry.encrypted_name);

        let new_entry = FileEntry {
            encrypted_name: file_entry.encrypted_name,
            original_name: to.to_string(),
            size: file_entry.size,
            nonce: file_entry.nonce,
            encrypted_at: file_entry.encrypted_at,
            hash: file_entry.hash,
        };

        meta_data.add_file(new_entry);

        self.write_meta(&header, &meta_data, &master_key)?;

        Ok(())
    }
}

/// 使用 Argon2id 从用户密码和容器盐值派生零化主密钥。
///
/// # 错误
/// Argon2 参数无效或派生失败时返回 [`VeilError::KeyDerivationError`]。
fn derive_master_key(password: &str, salt: &[u8; 32]) -> Result<Zeroizing<[u8; 32]>, VeilError> {
    let mut key = Zeroizing::new([0u8; 32]);
    kdf::derive_key(password.as_bytes(), salt, &mut *key)
        .map_err(|e| VeilError::KeyDerivationError(format!("密钥派生失败: {:?}", e)))?;
    Ok(key)
}

/// 使用 ChaCha20-Poly1305 加密字节。
///
/// 函数名沿用历史命名，实际 cipher 由当前实现决定。
///
/// # 错误
/// 加密失败时返回 [`VeilError::EncryptionError`]。
fn encrypt_aes256gcm(
    key: &[u8; 32],
    plaintext: &[u8],
    nonce: &[u8; 12],
) -> Result<Vec<u8>, VeilError> {
    use chacha20poly1305::aead::generic_array::GenericArray;

    // 将普通切片转换为当前 AEAD 库要求的固定长度参数类型。
    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(key));
    let nonce_ga = GenericArray::from_slice(nonce);

    cipher
        .encrypt(nonce_ga, plaintext)
        .map_err(|e| VeilError::EncryptionError(format!("加密失败: {}", e)))
}

/// 使用 ChaCha20-Poly1305 解密并验证字节。
///
/// 函数名沿用历史命名，实际 cipher 由当前实现决定。
///
/// # 错误
/// 密码错误、密文损坏或认证标签校验失败时返回
/// [`VeilError::DecryptionError`]。
fn decrypt_aes256gcm(
    key: &[u8; 32],
    ciphertext: &[u8],
    nonce: &[u8; 12],
) -> Result<Vec<u8>, VeilError> {
    use chacha20poly1305::aead::generic_array::GenericArray;

    // 解密同时验证认证标签，失败时不会返回任何明文。
    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(key));
    let nonce_ga = GenericArray::from_slice(nonce);

    cipher
        .decrypt(nonce_ga, ciphertext)
        .map_err(|e| VeilError::DecryptionError(format!("解密失败（密码错误或数据损坏）: {}", e)))
}

/// 使用系统随机源生成指定长度的字节数组。
///
/// # Panics
/// 系统随机源不可用时 panic，以避免使用可预测的密钥材料。
fn generate_random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::getrandom(&mut bytes).expect("生成随机数失败");
    bytes
}

/// 生成 8 字节随机 ID，并编码为 16 个十六进制字符。
fn generate_random_id() -> String {
    let bytes = generate_random_bytes::<8>();
    hex::encode(bytes)
}

/// 先写同路径的 `.tmp` 文件，再通过重命名替换目标文件。
///
/// # 错误
/// 临时文件写入或重命名失败时返回 I/O 错误。
fn atomic_write(path: &Path, data: &[u8]) -> Result<(), VeilError> {
    // 临时文件与目标同目录，确保 rename 不跨卷并且替换具有原子性。
    let temp_path = path.with_extension("tmp");

    fs::write(&temp_path, data)?;
    fs::rename(&temp_path, path)?;

    Ok(())
}
