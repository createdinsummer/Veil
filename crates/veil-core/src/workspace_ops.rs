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
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use zeroize::Zeroizing;

/// 一次批量添加中的单个源文件和容器内目标路径。
#[derive(Debug, Clone)]
pub struct AddFileSpec {
    /// 本地源文件路径。
    pub source: PathBuf,
    /// 容器内的相对目标路径，使用 `/` 分隔。
    pub target: String,
}

impl AddFileSpec {
    /// 创建文件添加请求。
    pub fn new(source: impl Into<PathBuf>, target: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
        }
    }
}

/// 删除操作命中的路径类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovedPathKind {
    /// 删除的是单个文件。
    File,
    /// 删除的是目录及其全部文件。
    Directory,
}

/// 移动操作命中的路径类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovedPathKind {
    /// 移动的是单个文件。
    File,
    /// 移动的是目录及其全部子文件。
    Directory,
}

/// 密码修改阶段中暂存的新密文和原密文备份。
struct StagedPasswordFile {
    original: PathBuf,
    staged: PathBuf,
    backup: PathBuf,
}

/// 对单个工作区容器执行初始化和文件操作。
pub struct WorkspaceManager {
    /// 包含 `.veil-meta` 和加密文件的工作区根路径。
    pub workspace_path: PathBuf,
    /// 调用方解析出的预期容器 ID；设置后所有元数据访问都会校验该身份。
    expected_veil_id: Option<String>,
}

impl WorkspaceManager {
    /// 为指定工作区路径创建工作区管理器。
    ///
    /// 构造过程只保存路径，不访问文件系统。
    pub fn new(workspace_path: PathBuf) -> Self {
        Self {
            workspace_path,
            expected_veil_id: None,
        }
    }

    /// 创建绑定到指定稳定 ID 的工作区管理器。
    ///
    /// 后续每次读取或写入元数据都会确认实际 `veil_id` 与预期值一致，避免链接或
    /// 工作区路径变化后误操作其他容器。
    pub fn for_container(workspace_path: PathBuf, veil_id: impl Into<String>) -> Self {
        Self {
            workspace_path,
            expected_veil_id: Some(veil_id.into()),
        }
    }

    /// 校验元数据身份是否与解析阶段得到的稳定 ID 一致。
    fn verify_veil_id(&self, actual: &str) -> Result<(), VeilError> {
        let Some(expected) = self.expected_veil_id.as_deref() else {
            return Ok(());
        };

        if expected != actual {
            return Err(VeilError::ContainerIdConflict(format!(
                "预期容器 ID '{}'，实际读取到 '{}'",
                expected, actual
            )));
        }

        Ok(())
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
        self.verify_veil_id(veil_id)?;

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
            AlgorithmId::ChaCha20Poly1305,
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
        Ok(self.read_meta_context(password)?.0)
    }

    /// 读取密码上下文，同时返回解密后的元数据、明文头部和零化主密钥。
    fn read_meta_context(
        &self,
        password: &str,
    ) -> Result<(MetaData, MetaHeader, Zeroizing<[u8; 32]>), VeilError> {
        let meta_path = self.workspace_path.join(".veil-meta");
        if !meta_path.exists() {
            return Err(VeilError::InvalidFormat("元数据文件不存在".to_string()));
        }

        let bytes = fs::read(&meta_path)?;
        let header = MetaHeader::from_bytes(&bytes)?;
        self.verify_veil_id(&header.veil_id)?;
        let master_key = derive_master_key(password, &header.salt)?;
        let header_len = MetaHeader::header_len(&bytes)?;
        let encrypted_data = &bytes[header_len..];
        let decrypted = decrypt_chacha20poly1305(&master_key, encrypted_data, &header.nonce)?;
        let meta_data = MetaData::from_json(&decrypted)?;

        Ok((meta_data, header, master_key))
    }

    /// 不输入密码，只读取恢复所需的明文身份信息。
    ///
    /// # 错误
    /// `.veil-meta` 无法读取或头部格式无效时返回错误。
    pub fn read_meta_header(&self) -> Result<MetaHeader, VeilError> {
        let meta_path = self.workspace_path.join(".veil-meta");
        let bytes = fs::read(&meta_path).map_err(VeilError::Io)?;
        let header = MetaHeader::from_bytes(&bytes)?;
        self.verify_veil_id(&header.veil_id)?;
        Ok(header)
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
        self.verify_veil_id(&header.veil_id)?;
        let meta_path = self.workspace_path.join(".veil-meta");

        let header_bytes = header.to_bytes()?;

        let json_bytes = meta_data.to_json()?;
        let encrypted = encrypt_chacha20poly1305(master_key, &json_bytes, &header.nonce)?;

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

    /// 加密并添加一个本地文件，目标名称使用源文件名。
    pub fn add_file(&self, file_path: &Path, password: &str) -> Result<String, VeilError> {
        let file_name = file_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| VeilError::WorkspaceError("无效的文件名".to_string()))?;

        let mut encrypted_names =
            self.add_files(&[AddFileSpec::new(file_path, file_name)], password)?;

        Ok(encrypted_names.remove(0))
    }

    /// 批量加密并添加本地文件。
    ///
    /// 方法先验证密码，再把所有新密文写入工作区，最后只提交一次元数据。同一批次
    /// 或已有元数据中的同名目标会被替换。提交失败时删除本次新增密文；提交成功后
    /// 删除被替换的旧密文。
    ///
    /// # 返回
    /// 成功时按去重后的输入顺序返回新密文文件名。
    ///
    /// # 错误
    /// 密码错误、目标路径无效、源文件读取、加密、密文写入或元数据提交失败时返回错误。
    pub fn add_files(
        &self,
        files: &[AddFileSpec],
        password: &str,
    ) -> Result<Vec<String>, VeilError> {
        if files.is_empty() {
            return Ok(Vec::new());
        }

        // 先验证密码并解密元数据；这一步发生在任何文件写入之前。
        let (mut meta_data, header, master_key) = self.read_meta_context(password)?;

        // 同一批次内相同目标路径采用“后者覆盖前者”，避免重复元数据条目。
        let mut deduplicated = Vec::with_capacity(files.len());
        let mut positions = HashMap::new();
        for file in files {
            let normalized_target = normalize_container_path(&file.target)?;
            let spec = AddFileSpec::new(&file.source, normalized_target.clone());
            if let Some(index) = positions.get(&normalized_target).copied() {
                deduplicated[index] = spec;
            } else {
                positions.insert(normalized_target, deduplicated.len());
                deduplicated.push(spec);
            }
        }

        let mut written = Vec::new();
        let mut new_entries = Vec::new();
        let transaction = (|| -> Result<(), VeilError> {
            for file in &deduplicated {
                let file_data = fs::read(&file.source)?;
                let file_size = file_data.len() as u64;
                let encrypted_name = format!("{}.enc", generate_random_id());
                let file_nonce = generate_random_bytes::<12>();
                let encrypted_data =
                    encrypt_chacha20poly1305(&master_key, &file_data, &file_nonce)?;
                let encrypted_path = self.workspace_path.join(&encrypted_name);

                crate::temp::write_private_file(&encrypted_path, &encrypted_data)?;
                written.push(encrypted_name.clone());
                new_entries.push(FileEntry::new(
                    encrypted_name,
                    file.target.clone(),
                    file_size,
                    file_nonce,
                ));
            }

            let mut replaced = Vec::new();
            for entry in &new_entries {
                replaced.extend(
                    meta_data
                        .files
                        .iter()
                        .filter(|existing| existing.original_name == entry.original_name)
                        .cloned(),
                );
                meta_data
                    .files
                    .retain(|existing| existing.original_name != entry.original_name);
                meta_data.add_file(entry.clone());
            }

            self.write_meta(&header, &meta_data, &master_key)?;

            // 元数据提交成功后旧密文才失去引用，删除失败不会影响新内容可用性。
            for entry in replaced {
                let _ = fs::remove_file(self.workspace_path.join(entry.encrypted_name));
            }

            Ok(())
        })();

        if let Err(error) = transaction {
            for encrypted_name in written {
                let _ = fs::remove_file(self.workspace_path.join(encrypted_name));
            }
            return Err(error);
        }

        Ok(new_entries
            .into_iter()
            .map(|entry| entry.encrypted_name)
            .collect())
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

        let entry = meta_data
            .find_file(original_name)
            .ok_or_else(|| VeilError::FileNotFound(format!("文件 '{}' 不存在", original_name)))?;

        // 元数据只暴露原始名称，实际读取必须通过条目保存的加密文件名。
        let encrypted_path = self.workspace_path.join(&entry.encrypted_name);
        let encrypted_data = fs::read(&encrypted_path)?;

        let meta_path = self.workspace_path.join(".veil-meta");
        let meta_bytes = fs::read(&meta_path)?;
        let header = MetaHeader::from_bytes(&meta_bytes)?;
        self.verify_veil_id(&header.veil_id)?;

        let master_key = derive_master_key(password, &header.salt)?;

        let decrypted = decrypt_chacha20poly1305(&master_key, &encrypted_data, &entry.nonce)?;

        fs::write(output_path, decrypted)?;

        Ok(())
    }

    /// 删除指定文件。
    ///
    /// 这是 [`WorkspaceManager::remove_path`] 的兼容入口，调用方可通过额外参数
    /// 获得文件或目录类型。
    pub fn remove_file(
        &self,
        original_name: &str,
        password: &str,
    ) -> Result<RemovedPathKind, VeilError> {
        self.remove_path(original_name, password)
    }

    /// 删除文件，或递归删除目录及其全部文件。
    ///
    /// 方法先验证密码并提交更新后的元数据，再删除已失去引用的密文。这样提交失败
    /// 不会破坏原容器；密文清理失败只会留下不可达的孤立文件。
    ///
    /// # 错误
    /// 密码错误、路径无效、目标不存在或元数据提交失败时返回错误。
    pub fn remove_path(
        &self,
        original_name: &str,
        password: &str,
    ) -> Result<RemovedPathKind, VeilError> {
        if matches!(original_name, "." | "/") {
            return Err(VeilError::WorkspaceError("不能删除容器根目录".to_string()));
        }

        let normalized = normalize_container_path(original_name)?;
        let (mut meta_data, header, master_key) = self.read_meta_context(password)?;
        let exact_file = meta_data.find_file(&normalized).cloned();
        let directory_prefix = format!("{normalized}/");
        let has_children = meta_data
            .files
            .iter()
            .any(|entry| entry.original_name.starts_with(&directory_prefix));

        if exact_file.is_none() && !has_children {
            return Err(VeilError::FileNotFound(format!(
                "文件或目录 '{}' 不存在",
                normalized
            )));
        }

        let kind = if has_children {
            RemovedPathKind::Directory
        } else {
            RemovedPathKind::File
        };

        let removed: Vec<_> = meta_data
            .files
            .iter()
            .filter(|entry| {
                entry.original_name == normalized
                    || entry.original_name.starts_with(&directory_prefix)
            })
            .cloned()
            .collect();

        meta_data.files.retain(|entry| {
            entry.original_name != normalized && !entry.original_name.starts_with(&directory_prefix)
        });
        self.write_meta(&header, &meta_data, &master_key)?;

        for entry in removed {
            let _ = fs::remove_file(self.workspace_path.join(entry.encrypted_name));
        }

        Ok(kind)
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
    /// 方法先验证旧密码、解密全部文件并生成新密文，再替换文件，最后提交新元数据。
    /// 运行期任一步失败都会尝试恢复原文件；新密码不能为空。
    ///
    /// # 错误
    /// 旧密码错误、元数据或文件解密失败，以及新元数据或文件写入失败时返回错误。
    pub fn change_password(&self, old_password: &str, new_password: &str) -> Result<(), VeilError> {
        if new_password.is_empty() {
            return Err(VeilError::WorkspaceError("新密码不能为空".to_string()));
        }

        let (meta_data, old_header, old_master_key) = self.read_meta_context(old_password)?;

        let new_salt = generate_random_bytes::<32>();
        let new_nonce = generate_random_bytes::<12>();

        let new_master_key = derive_master_key(new_password, &new_salt)?;

        let new_header = MetaHeader::new(
            new_salt,
            new_nonce,
            AlgorithmId::ChaCha20Poly1305,
            old_header.veil_id.clone(),
            old_header.container_name.clone(),
            old_header.workspace_type.clone(),
        );

        // 第一阶段只生成新密文，不触碰原文件；任一文件失败时原容器保持完整。
        let mut staged = Vec::with_capacity(meta_data.files.len());
        for file_entry in &meta_data.files {
            let prepared = (|| -> Result<StagedPasswordFile, VeilError> {
                let encrypted_path = self.workspace_path.join(&file_entry.encrypted_name);
                let encrypted_file_data = fs::read(&encrypted_path)?;
                let plaintext = decrypt_chacha20poly1305(
                    &old_master_key,
                    &encrypted_file_data,
                    &file_entry.nonce,
                )?;
                let new_encrypted =
                    encrypt_chacha20poly1305(&new_master_key, &plaintext, &file_entry.nonce)?;

                let staged_path = self.unique_password_temp_path("new")?;
                crate::temp::write_private_file(&staged_path, &new_encrypted)?;
                let backup = match self.unique_password_temp_path("bak") {
                    Ok(path) => path,
                    Err(error) => {
                        let _ = fs::remove_file(&staged_path);
                        return Err(error);
                    }
                };

                Ok(StagedPasswordFile {
                    original: encrypted_path,
                    staged: staged_path,
                    backup,
                })
            })();

            match prepared {
                Ok(file) => staged.push(file),
                Err(error) => {
                    remove_staged_password_files(&staged);
                    return Err(error);
                }
            }
        }

        // 第二阶段用 rename 替换文件，并保留旧密文备份以便回滚。
        let mut replaced = Vec::with_capacity(staged.len());
        for file in &staged {
            if let Err(error) = fs::rename(&file.original, &file.backup) {
                restore_password_backups(&replaced);
                remove_staged_password_files(&staged);
                return Err(VeilError::Io(error));
            }

            if let Err(error) = fs::rename(&file.staged, &file.original) {
                let _ = fs::rename(&file.backup, &file.original);
                restore_password_backups(&replaced);
                remove_staged_password_files(&staged);
                return Err(VeilError::Io(error));
            }

            replaced.push((file.original.clone(), file.backup.clone()));
        }

        // 第三阶段提交元数据；失败时恢复全部旧密文。
        if let Err(error) = self.write_meta(&new_header, &meta_data, &new_master_key) {
            let rollback_errors = restore_password_backups(&replaced);
            if !rollback_errors.is_empty() {
                return Err(VeilError::WorkspaceError(format!(
                    "密码修改失败，且部分文件回滚失败: {} ({})",
                    error,
                    rollback_errors.join("; ")
                )));
            }
            return Err(error);
        }

        // 元数据和新密文均已持久化，旧密文备份可以安全删除。
        for file in &staged {
            let _ = fs::remove_file(&file.backup);
        }

        Ok(())
    }

    /// 为密码修改临时文件生成工作区内唯一路径。
    fn unique_password_temp_path(&self, label: &str) -> Result<PathBuf, VeilError> {
        for _ in 0..16 {
            let candidate = self
                .workspace_path
                .join(format!(".veil-pw-{label}-{}.tmp", generate_random_id()));
            if !candidate.exists() {
                return Ok(candidate);
            }
        }

        Err(VeilError::WorkspaceError(
            "无法分配密码修改临时文件".to_string(),
        ))
    }

    /// 修改文件路径的兼容入口。
    pub fn rename_file(&self, from: &str, to: &str, password: &str) -> Result<(), VeilError> {
        self.move_path(from, to, password).map(|_| ())
    }

    /// 移动或重命名文件与目录，并返回最终目标路径。
    ///
    /// 如果目标已经表示一个目录，源会被移动到目标目录下并保留最后一级名称。
    /// 目录移动会同步更新全部子文件路径，但不会移动或重新加密任何密文。
    ///
    /// # 错误
    /// 源不存在、目标冲突、目标位于源目录内部、路径无效、密码错误或元数据写回失败时返回错误。
    pub fn move_path(
        &self,
        from: &str,
        to: &str,
        password: &str,
    ) -> Result<(MovedPathKind, String), VeilError> {
        if matches!(from, "." | "/") || matches!(to, "." | "/") {
            return Err(VeilError::WorkspaceError(
                "源路径和目标路径都不能是容器根目录".to_string(),
            ));
        }

        let source = normalize_container_path(from)?;
        let destination = normalize_container_path(to)?;
        let destination_ends_with_separator = to.ends_with('/') || to.ends_with('\\');

        let (mut meta_data, header, master_key) = self.read_meta_context(password)?;
        let source_prefix = format!("{source}/");
        let source_file = meta_data.find_file(&source).cloned();
        let source_children: Vec<_> = meta_data
            .files
            .iter()
            .filter(|entry| entry.original_name.starts_with(&source_prefix))
            .cloned()
            .collect();

        if source_file.is_some() && !source_children.is_empty() {
            return Err(VeilError::WorkspaceError(format!(
                "源路径同时匹配文件和目录: {source}"
            )));
        }

        let source_kind = if source_file.is_some() {
            MovedPathKind::File
        } else if source_children.is_empty() {
            return Err(VeilError::FileNotFound(format!(
                "源路径 '{}' 不存在",
                source
            )));
        } else {
            MovedPathKind::Directory
        };

        let destination_prefix = format!("{destination}/");
        let destination_children_exist = meta_data
            .files
            .iter()
            .any(|entry| entry.original_name.starts_with(&destination_prefix));
        let destination_is_directory =
            destination_ends_with_separator || destination_children_exist;

        let final_target = if destination_is_directory {
            let source_name = source
                .rsplit('/')
                .next()
                .ok_or_else(|| VeilError::WorkspaceError("源路径缺少名称".to_string()))?;
            if destination.is_empty() {
                source_name.to_string()
            } else {
                format!("{destination}/{source_name}")
            }
        } else {
            destination.clone()
        };

        if final_target == source {
            return Err(VeilError::WorkspaceError(
                "源路径和目标路径相同".to_string(),
            ));
        }

        let final_prefix = format!("{final_target}/");
        if source_kind == MovedPathKind::Directory
            && (final_target.starts_with(&source_prefix) || final_target == source)
        {
            return Err(VeilError::WorkspaceError(
                "不能把目录移动到自身或子目录".to_string(),
            ));
        }

        if meta_data.find_file(&final_target).is_some()
            || meta_data
                .files
                .iter()
                .any(|entry| entry.original_name.starts_with(&final_prefix))
        {
            return Err(VeilError::FileAlreadyExists(final_target));
        }

        match source_kind {
            MovedPathKind::File => {
                let encrypted_name = source_file
                    .ok_or_else(|| VeilError::FileNotFound(source.clone()))?
                    .encrypted_name;
                if let Some(entry) = meta_data
                    .files
                    .iter_mut()
                    .find(|entry| entry.encrypted_name == encrypted_name)
                {
                    entry.original_name = final_target.clone();
                }
            }
            MovedPathKind::Directory => {
                for entry in &mut meta_data.files {
                    if entry.original_name == source {
                        entry.original_name = final_target.clone();
                    } else if let Some(suffix) = entry.original_name.strip_prefix(&source_prefix) {
                        entry.original_name = format!("{final_target}/{suffix}");
                    }
                }
            }
        }

        self.write_meta(&header, &meta_data, &master_key)?;
        Ok((source_kind, final_target))
    }
}

/// 删除尚未替换的密码修改暂存密文。
fn remove_staged_password_files(files: &[StagedPasswordFile]) {
    for file in files {
        let _ = fs::remove_file(&file.staged);
    }
}

/// 用旧密文备份恢复已经替换的文件，返回无法恢复的路径。
fn restore_password_backups(replaced: &[(PathBuf, PathBuf)]) -> Vec<String> {
    let mut errors = Vec::new();
    for (original, backup) in replaced.iter().rev() {
        if let Err(error) = fs::remove_file(original)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            errors.push(format!("{}: {}", original.display(), error));
            continue;
        }
        if let Err(error) = fs::rename(backup, original) {
            errors.push(format!("{}: {}", original.display(), error));
        }
    }
    errors
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
/// # 错误
/// 加密失败时返回 [`VeilError::EncryptionError`]。
fn encrypt_chacha20poly1305(
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
/// # 错误
/// 密码错误、密文损坏或认证标签校验失败时返回
/// [`VeilError::DecryptionError`]。
fn decrypt_chacha20poly1305(
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

/// 校验并规范化容器内相对路径，统一使用 `/` 分隔。
pub fn normalize_container_path(path: &str) -> Result<String, VeilError> {
    if path.is_empty() {
        return Err(VeilError::WorkspaceError(
            "容器内目标路径不能为空".to_string(),
        ));
    }

    let mut parts = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_str().ok_or_else(|| {
                    VeilError::WorkspaceError("目标路径不是有效 UTF-8".to_string())
                })?;
                parts.push(part.to_string());
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(VeilError::WorkspaceError(format!(
                    "目标路径必须是容器内相对路径: {}",
                    path
                )));
            }
        }
    }

    if parts.is_empty() {
        return Err(VeilError::WorkspaceError(
            "容器内目标路径不能为空".to_string(),
        ));
    }

    Ok(parts.join("/"))
}

/// 先写同目录临时文件，再通过原子替换更新目标文件。
///
/// # 错误
/// 临时文件创建、写入、同步或替换失败时返回 I/O 错误。
fn atomic_write(path: &Path, data: &[u8]) -> Result<(), VeilError> {
    // 临时文件与目标同目录，避免跨卷移动，并兼容 Windows 的覆盖语义。
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temp_file = tempfile::NamedTempFile::new_in(parent)?;
    temp_file.write_all(data)?;
    temp_file.as_file().sync_all()?;
    temp_file
        .persist(path)
        .map_err(|error| VeilError::Io(error.error))?;

    Ok(())
}
