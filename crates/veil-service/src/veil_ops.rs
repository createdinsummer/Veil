//! Veil 的高层读写操作。
//!
//! [`VeilManager`] 把 `.veil-info` 与同目录下的 `.enc` 文件组合成一个 Veil。
//! 密码和 salt 经 Argon2id 派生出密码保护密钥，只用于加密元数据 JSONC；文件内容
//! 使用元数据中的随机数据主密钥按固定大小分块流式加密。两者均使用
//! ChaCha20-Poly1305，文件各自保存独立 base nonce。`.veil-info` 中记录对应的
//! 算法 ID，具体值由 [`MetaHeader`] 持久化。

use crate::error::VeilError;
use crate::file_ops::{self, DATA_KEY_LEN};
use crate::kdf;
use crate::locking::{self, LockOptions};
use crate::metadata::{AlgorithmId, FileEntry, MetaData, MetaHeader, VEIL_INFO_FILE};
use crate::version::{VersionRelation, VersionSnapshot, compare_version_history};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use veil_core::crypto::{decrypt_chacha20poly1305, encrypt_chacha20poly1305};
use zeroize::Zeroizing;

/// 一次批量添加中的单个源文件和 Veil 内目标路径。
#[derive(Debug, Clone)]
pub struct AddFileSpec {
    /// 本地源文件路径。
    pub source: PathBuf,
    /// Veil 内的相对目标路径，使用 `/` 分隔。
    pub target: String,
    /// 可选的编辑基础版本；指定后必须与当前文件版本完全一致。
    /// 不指定时，同路径添加会直接覆盖已有文件。
    pub base_version: Option<VersionSnapshot>,
    /// 禁止覆盖容器内已存在的同名文件。
    pub reject_existing: bool,
}

impl AddFileSpec {
    /// 创建文件添加请求。
    pub fn new(source: impl Into<PathBuf>, target: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            base_version: None,
            reject_existing: false,
        }
    }

    /// 指定导出时取得的基础版本，用于阻止过期修改覆盖新内容。
    pub fn with_base_version(mut self, base_version: VersionSnapshot) -> Self {
        self.base_version = Some(base_version);
        self
    }

    /// 在持锁提交期间拒绝同名文件。
    pub fn without_overwrite(mut self) -> Self {
        self.reject_existing = true;
        self
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

/// 对单个 Veil 执行初始化和文件操作。
pub struct VeilManager {
    /// 包含 `.veil-info` 和 `.enc` 文件的实际 Veil 目录。
    pub veil_dir: PathBuf,
    /// 调用方解析出的预期容器 ID；设置后所有元数据访问都会校验该身份。
    expected_veil_id: Option<String>,
    lock_options: LockOptions,
}

/// 容器内路径的类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerPathKind {
    File(String),
    Directory(String),
    Missing(String),
}

impl VeilManager {
    /// 为指定 `veil_dir` 创建 Veil 管理器。
    ///
    /// 构造过程只保存路径，不访问文件系统。
    pub fn new(veil_dir: PathBuf) -> Self {
        Self {
            veil_dir,
            expected_veil_id: None,
            lock_options: LockOptions::default(),
        }
    }

    /// 创建绑定到指定稳定 ID 的 Veil 管理器。
    ///
    /// 后续每次读取或写入元数据都会确认实际 `veil_id` 与预期值一致，避免链接或
    /// 目录位置变化后误操作其他 Veil。
    pub fn for_veil(veil_dir: PathBuf, veil_id: impl Into<String>) -> Self {
        Self {
            veil_dir,
            expected_veil_id: Some(veil_id.into()),
            lock_options: LockOptions::default(),
        }
    }

    /// 设置后续操作的锁等待超时和取消标志。
    pub fn with_lock_options(mut self, options: LockOptions) -> Self {
        self.lock_options = options;
        self
    }

    /// 校验元数据身份是否与解析阶段得到的稳定 ID 一致。
    fn verify_veil_id(&self, actual: &str) -> Result<(), VeilError> {
        let Some(expected) = self.expected_veil_id.as_deref() else {
            return Ok(());
        };

        if expected != actual {
            return Err(VeilError::VeilIdConflict(format!(
                "预期容器 ID '{}'，实际读取到 '{}'",
                expected, actual
            )));
        }

        Ok(())
    }

    /// 为新 Veil 生成稳定 ID 并初始化目录。
    ///
    /// # 错误
    /// 目录创建、密钥派生或元数据写入失败时返回错误。
    pub fn init_veil(&self, veil_name: &str, password: &str) -> Result<MetaData, VeilError> {
        self.init_veil_with_id(&crate::metadata::generate_veil_id(), veil_name, password)
    }

    /// 使用调用方指定的稳定 ID 初始化新 Veil。
    ///
    /// 方法创建目录和随机盐/nonce，生成初始元数据，并写入加密的 `.veil-info`。
    ///
    /// # 错误
    /// 目录创建、随机数生成、密钥派生或元数据写入失败时返回错误。
    pub fn init_veil_with_id(
        &self,
        veil_id: &str,
        veil_name: &str,
        password: &str,
    ) -> Result<MetaData, VeilError> {
        self.verify_veil_id(veil_id)?;

        crate::fsutil::create_dir_all_durable(&self.veil_dir)
            .map_err(|e| VeilError::WorkError(format!("创建 Veil 目录失败: {}", e)))?;
        let _maintenance = locking::maintenance(&self.veil_dir, false, &self.lock_options)?;
        let _config = locking::config(&self.veil_dir, false, &self.lock_options)?;

        // salt 决定密码保护密钥；每次提交的 nonce 都写入当次明文头部。
        let salt = generate_random_bytes::<32>();
        let nonce = generate_random_bytes::<12>();

        let meta_data = MetaData::with_veil_id(veil_id.to_string(), veil_name.to_string());

        let header = MetaHeader::new(
            salt,
            nonce,
            AlgorithmId::ChaCha20Poly1305,
            meta_data.veil_id.clone(),
            meta_data.veil_name.clone(),
            meta_data.created_at.clone(),
        );

        let master_key = derive_master_key(password, &salt)?;

        self.write_meta(&header, &meta_data, &master_key)?;
        // 初始化没有可回退的旧快照，目录同步失败时由调用方整体回滚。
        crate::fsutil::sync_directory(&self.veil_dir)?;

        Ok(meta_data)
    }

    /// 查询容器内文件或目录，读取前会验证密码。
    pub fn path_kind(&self, path: &str, password: &str) -> Result<ContainerPathKind, VeilError> {
        let metadata = self.read_meta(password)?;
        if path == "." || path == "/" {
            return Ok(ContainerPathKind::Directory(path.to_string()));
        }
        let normalized = normalize_veil_dir(path)?;
        if metadata.find_file(&normalized).is_some() {
            return Ok(ContainerPathKind::File(normalized));
        }
        let prefix = format!("{normalized}/");
        if metadata
            .files
            .iter()
            .any(|entry| entry.original_name.starts_with(&prefix))
        {
            return Ok(ContainerPathKind::Directory(normalized));
        }
        Ok(ContainerPathKind::Missing(normalized))
    }

    /// 使用密码读取并解密 Veil 元数据。
    ///
    /// `.veil-info` 不存在、头部无效、密钥派生失败、密码错误或密文损坏时返回错误。
    pub fn read_meta(&self, password: &str) -> Result<MetaData, VeilError> {
        let maintenance = locking::maintenance(&self.veil_dir, true, &self.lock_options)?;
        let config = locking::config(&self.veil_dir, true, &self.lock_options)?;
        let metadata = self.read_meta_context(password)?.0;
        drop(config);
        drop(maintenance);
        self.try_cleanup(password);
        Ok(metadata)
    }

    /// 在同一个配置锁内取得已解密元数据及其对应的密文快照。
    pub fn read_meta_snapshot(&self, password: &str) -> Result<(MetaData, Vec<u8>), VeilError> {
        let _maintenance = locking::maintenance(&self.veil_dir, true, &self.lock_options)?;
        let _config = locking::config(&self.veil_dir, true, &self.lock_options)?;
        let metadata = self.read_meta_context(password)?.0;
        let bytes = fs::read(self.veil_dir.join(VEIL_INFO_FILE))?;
        Ok((metadata, bytes))
    }

    /// 读取密码上下文，同时返回解密后的元数据、明文头部和零化密码保护密钥。
    fn read_meta_context(
        &self,
        password: &str,
    ) -> Result<(MetaData, MetaHeader, Zeroizing<[u8; 32]>), VeilError> {
        let meta_path = self.veil_dir.join(VEIL_INFO_FILE);
        if !meta_path.exists() {
            return Err(VeilError::InvalidFormat("元数据文件不存在".to_string()));
        }

        let bytes = fs::read(&meta_path)?;
        let header = MetaHeader::from_bytes(&bytes)?;
        self.verify_veil_id(&header.veil_id)?;
        let master_key = derive_master_key(password, &header.salt)?;
        let header_len = MetaHeader::header_len(&bytes)?;
        let encrypted_data = &bytes[header_len..];
        let decrypted = Zeroizing::new(decrypt_chacha20poly1305(
            &master_key,
            encrypted_data,
            &header.nonce,
        )?);
        let meta_data = MetaData::from_jsonc(decrypted.as_slice())?;
        if meta_data.veil_id != header.veil_id {
            return Err(VeilError::VeilIdConflict(
                "元数据明文头与加密正文中的容器 ID 不一致".into(),
            ));
        }

        Ok((meta_data, header, master_key))
    }

    /// 不输入密码，只读取恢复所需的明文身份信息。
    ///
    /// # 错误
    /// `.veil-info` 无法读取或头部格式无效时返回错误。
    pub fn read_meta_header(&self) -> Result<MetaHeader, VeilError> {
        let meta_path = self.veil_dir.join(VEIL_INFO_FILE);
        let bytes = fs::read(&meta_path).map_err(VeilError::Io)?;
        let header = MetaHeader::from_bytes(&bytes)?;
        self.verify_veil_id(&header.veil_id)?;
        Ok(header)
    }

    /// 编码、加密并原子替换 `.veil-info`。
    ///
    /// # 错误
    /// 头部或 JSONC 序列化、内容加密、临时文件写入或重命名失败时返回错误。
    fn write_meta(
        &self,
        header: &MetaHeader,
        meta_data: &MetaData,
        master_key: &[u8; 32],
    ) -> Result<(), VeilError> {
        self.verify_veil_id(&header.veil_id)?;
        meta_data.primary_data_key()?;
        let meta_path = self.veil_dir.join(VEIL_INFO_FILE);

        // 每次元数据提交都使用新的 nonce；同一密码保护密钥下不能重复使用
        // ChaCha20-Poly1305
        // nonce，否则会破坏元数据的机密性保证。
        let mut commit_header = header.clone();
        commit_header.nonce = generate_random_bytes::<12>();
        // 名称查找和无密码重建链接读取明文头，名称、创建时间与正文同步。
        commit_header.veil_name = meta_data.veil_name.clone();
        commit_header.created_at = meta_data.created_at.clone();
        let header_bytes = commit_header.to_bytes()?;

        let jsonc_bytes = Zeroizing::new(meta_data.to_jsonc()?);
        let encrypted =
            encrypt_chacha20poly1305(master_key, jsonc_bytes.as_slice(), &commit_header.nonce)?;

        // .veil-info 使用“明文 TLV 头部 + 密文 JSONC”的连续布局。
        let mut file_data = header_bytes;
        file_data.extend_from_slice(&encrypted);

        crate::fsutil::atomic_write_metadata(&meta_path, &file_data).map_err(|error| {
            if error.published {
                VeilError::CommitUncertain(error.error)
            } else {
                VeilError::Io(error.error)
            }
        })?;

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
        let _maintenance = locking::maintenance(&self.veil_dir, false, &self.lock_options)?;
        let _config = locking::config(&self.veil_dir, false, &self.lock_options)?;
        // 先在内存中完成调用方修改；读取上下文时只执行一次密钥派生。
        let (mut meta_data, header, master_key) = self.read_meta_context(password)?;

        update_fn(&mut meta_data);

        self.write_meta(&header, &meta_data, &master_key)?;

        Ok(())
    }

    /// 加密并添加一个本地文件，目标名称使用源文件名。
    pub fn add_file(&self, file_path: &Path, password: &str) -> Result<String, VeilError> {
        let file_name = file_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| VeilError::WorkError("无效的文件名".to_string()))?;

        let mut encrypted_names =
            self.add_files(&[AddFileSpec::new(file_path, file_name)], password)?;

        Ok(encrypted_names.remove(0))
    }

    /// 批量加密并添加本地文件。
    ///
    /// 方法先验证密码，再把所有新密文写入工作区，最后只提交一次元数据。同一批次
    /// 或已有元数据中的同名目标会被替换。提交失败时删除本次新增密文；提交成功后
    /// 删除被替换的旧密文。
    /// 指定基础版本的条目会在持有文件独占锁期间校验版本链；未指定基础版本的条目
    /// 直接采用覆盖语义。
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

        let maintenance = locking::maintenance(&self.veil_dir, true, &self.lock_options)?;
        let namespace = locking::namespace(&self.veil_dir, true, &self.lock_options)?;

        // 同一批次内相同目标路径采用“后者覆盖前者”，避免重复元数据条目。
        let mut deduplicated = Vec::with_capacity(files.len());
        let mut positions = HashMap::new();
        for file in files {
            let normalized_target = normalize_veil_dir(&file.target)?;
            let mut spec = AddFileSpec::new(&file.source, normalized_target.clone());
            spec.base_version = file.base_version.clone();
            spec.reject_existing = file.reject_existing;
            if let Some(index) = positions.get(&normalized_target).copied() {
                deduplicated[index] = spec;
            } else {
                positions.insert(normalized_target, deduplicated.len());
                deduplicated.push(spec);
            }
        }

        // 同名创建需要路径锁；使用容器数据密钥保护锁文件名中的路径信息。
        let config = locking::config(&self.veil_dir, true, &self.lock_options)?;
        let (snapshot, _, _) = self.read_meta_context(password)?;
        let path_key = Zeroizing::new(blake3::derive_key(
            "Veil path lock key v1",
            snapshot.primary_data_key()?,
        ));
        drop(snapshot);
        drop(config);
        // 文件锁基于稳定 ID，多个路径和文件锁均按稳定顺序获取。
        let mut targets: Vec<_> = deduplicated
            .iter()
            .map(|file| file.target.as_str())
            .collect();
        targets.sort_unstable();
        let mut path_guards = Vec::new();
        for target in targets {
            path_guards.push(locking::path(
                &self.veil_dir,
                target,
                &path_key,
                &self.lock_options,
            )?);
        }
        let config = locking::config(&self.veil_dir, true, &self.lock_options)?;
        let (meta_data, _, _) = self.read_meta_context(password)?;
        let target_ids: HashMap<_, _> = deduplicated
            .iter()
            .map(|file| {
                let id = meta_data
                    .find_file(&file.target)
                    .map(|entry| entry.file_id.clone())
                    .unwrap_or_else(crate::metadata::generate_file_id);
                (file.target.clone(), id)
            })
            .collect();
        let mut ids: Vec<_> = target_ids.values().cloned().collect();
        drop(config);
        ids.sort_unstable();
        ids.dedup();
        let mut file_guards = Vec::new();
        for id in &ids {
            file_guards.push(locking::file(
                &self.veil_dir,
                id,
                false,
                &self.lock_options,
            )?);
        }
        #[cfg(test)]
        crate::fsutil::fault_point("file_locked");
        let config = locking::config(&self.veil_dir, true, &self.lock_options)?;
        let (meta_data, _, _) = self.read_meta_context(password)?;
        drop(config);
        for file in &deduplicated {
            validate_base_version(&meta_data, file)?;
        }

        let mut written = Vec::new();
        let mut new_entries = Vec::new();
        let data_key = *meta_data.primary_data_key()?;
        let transaction = (|| -> Result<(), VeilError> {
            for file in &deduplicated {
                let existing = meta_data.find_file(&file.target);
                let file_id = target_ids[&file.target].clone();
                let encrypted_name = format!("{}.enc", generate_random_id());
                let file_nonce = file_ops::generate_base_nonce();
                let encrypted_path = self.veil_dir.join(&encrypted_name);
                let file_size = file_ops::encrypt_file_streaming(
                    &file.source,
                    &encrypted_path,
                    &data_key,
                    &file_nonce,
                )?;
                written.push(encrypted_name.clone());
                let mut entry = FileEntry::with_file_id(
                    file_id,
                    encrypted_name,
                    file.target.clone(),
                    file_size,
                    file_nonce,
                );
                if let Some(existing) = existing {
                    entry.version_history = existing.version_history.clone();
                    entry.advance_version();
                }
                new_entries.push(entry);
            }

            // 新密文的目录项必须先持久化，之后写出的 .veil-info 才能安全引用它们。
            crate::fsutil::sync_directory(&self.veil_dir)?;
            #[cfg(test)]
            crate::fsutil::fault_point("cipher_written");

            let _config = locking::config(&self.veil_dir, false, &self.lock_options)?;
            let (mut latest, header, master_key) = self.read_meta_context(password)?;
            for file in &deduplicated {
                validate_base_version(&latest, file)?;
            }
            for entry in &new_entries {
                latest
                    .files
                    .retain(|existing| existing.original_name != entry.original_name);
                latest.add_file(entry.clone());
            }

            self.write_meta(&header, &latest, &master_key)?;

            Ok(())
        })();

        if let Err(error) = transaction {
            if !matches!(error, VeilError::CommitUncertain(_)) {
                for encrypted_name in written {
                    let _ = fs::remove_file(self.veil_dir.join(encrypted_name));
                }
            }
            return Err(error);
        }

        // 只有当前元数据目录项再次同步成功后，才删除失去引用的旧密文。
        drop(file_guards);
        drop(path_guards);
        drop(namespace);
        drop(maintenance);
        self.try_cleanup(password);

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
        self.extract_file_with_version(original_name, output_path, password)
            .map(|_| ())
    }

    /// 导出文件，并返回与导出内容一致的基础版本信息。
    pub fn extract_file_with_version(
        &self,
        original_name: &str,
        output_path: &Path,
        password: &str,
    ) -> Result<VersionSnapshot, VeilError> {
        self.extract_file_inner(original_name, output_path, password, true)
    }

    /// 导出文件并拒绝覆盖已存在的目标。
    pub fn extract_file_without_overwrite(
        &self,
        original_name: &str,
        output_path: &Path,
        password: &str,
    ) -> Result<VersionSnapshot, VeilError> {
        self.extract_file_inner(original_name, output_path, password, false)
    }

    fn extract_file_inner(
        &self,
        original_name: &str,
        output_path: &Path,
        password: &str,
        overwrite: bool,
    ) -> Result<VersionSnapshot, VeilError> {
        let _maintenance = locking::maintenance(&self.veil_dir, true, &self.lock_options)?;
        let config = locking::config(&self.veil_dir, true, &self.lock_options)?;
        let (snapshot, _, _) = self.read_meta_context(password)?;
        let file_id = snapshot
            .find_file(original_name)
            .ok_or_else(|| VeilError::FileNotFound(format!("文件 '{}' 不存在", original_name)))?
            .file_id
            .clone();
        drop(config);
        let _file = locking::file(&self.veil_dir, &file_id, true, &self.lock_options)?;
        let config = locking::config(&self.veil_dir, true, &self.lock_options)?;
        let (meta_data, _header, _master_key) = self.read_meta_context(password)?;
        drop(config);

        let entry = meta_data
            .find_file(original_name)
            .ok_or_else(|| VeilError::FileNotFound(format!("文件 '{}' 不存在", original_name)))?;
        if entry.file_id != file_id {
            return Err(VeilError::WorkError("文件在读取期间被替换，请重试".into()));
        }

        // 元数据只暴露原始名称，实际读取必须通过条目保存的加密文件名。
        let encrypted_path = self.veil_dir.join(&entry.encrypted_name);
        let key_index = file_ops::detect_key_index(
            &encrypted_path,
            &meta_data.data_keys,
            &entry.nonce,
            entry.size,
        )?;
        if overwrite {
            file_ops::decrypt_file_streaming(
                &encrypted_path,
                output_path,
                &meta_data.data_keys[key_index],
                &entry.nonce,
                entry.size,
            )?;
        } else {
            file_ops::decrypt_file_streaming_without_overwrite(
                &encrypted_path,
                output_path,
                &meta_data.data_keys[key_index],
                &entry.nonce,
                entry.size,
            )?;
        }

        Ok(VersionSnapshot {
            veil_id: meta_data.veil_id.clone(),
            file_id: entry.file_id.clone(),
            version_history: entry.version_history.clone(),
        })
    }

    /// 持共享锁读取指定文件的明文到内存，并限制最大大小。
    pub fn read_file_bytes(
        &self,
        original_name: &str,
        password: &str,
        max_size: u64,
    ) -> Result<Vec<u8>, VeilError> {
        let _maintenance = locking::maintenance(&self.veil_dir, true, &self.lock_options)?;
        let config = locking::config(&self.veil_dir, true, &self.lock_options)?;
        let (snapshot, _, _) = self.read_meta_context(password)?;
        let file_id = snapshot
            .find_file(original_name)
            .ok_or_else(|| VeilError::FileNotFound(format!("文件 '{}' 不存在", original_name)))?
            .file_id
            .clone();
        drop(config);
        let _file = locking::file(&self.veil_dir, &file_id, true, &self.lock_options)?;
        let config = locking::config(&self.veil_dir, true, &self.lock_options)?;
        let (metadata, _, _) = self.read_meta_context(password)?;
        drop(config);
        let entry = metadata
            .find_file(original_name)
            .ok_or_else(|| VeilError::FileNotFound(format!("文件 '{}' 不存在", original_name)))?;
        if entry.file_id != file_id {
            return Err(VeilError::WorkError("文件在读取期间被替换，请重试".into()));
        }
        if entry.size > max_size {
            return Err(VeilError::WorkError("文件过大，暂不支持预览".into()));
        }
        let encrypted_path = self.veil_dir.join(&entry.encrypted_name);
        let key_index = file_ops::detect_key_index(
            &encrypted_path,
            &metadata.data_keys,
            &entry.nonce,
            entry.size,
        )?;
        let mut bytes = Zeroizing::new(Vec::new());
        file_ops::decrypt_file_to_writer(
            &encrypted_path,
            &mut *bytes,
            &metadata.data_keys[key_index],
            &entry.nonce,
            entry.size,
        )?;
        Ok(std::mem::take(&mut *bytes))
    }

    /// 删除指定文件。
    ///
    /// 这是 [`VeilManager::remove_path`] 的兼容入口，调用方可通过额外参数
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
            return Err(VeilError::WorkError("不能删除容器根目录".to_string()));
        }

        let normalized = normalize_veil_dir(original_name)?;
        let maintenance = locking::maintenance(&self.veil_dir, true, &self.lock_options)?;
        let namespace = locking::namespace(&self.veil_dir, false, &self.lock_options)?;
        let config = locking::config(&self.veil_dir, true, &self.lock_options)?;
        let (snapshot, _, _) = self.read_meta_context(password)?;
        let prefix = format!("{normalized}/");
        let mut ids: Vec<_> = snapshot
            .files
            .iter()
            .filter(|entry| {
                entry.original_name == normalized || entry.original_name.starts_with(&prefix)
            })
            .map(|entry| entry.file_id.as_str())
            .collect();
        ids.sort_unstable();
        drop(config);
        let mut file_guards = Vec::new();
        for id in ids {
            file_guards.push(locking::file(
                &self.veil_dir,
                id,
                false,
                &self.lock_options,
            )?);
        }
        let config = locking::config(&self.veil_dir, false, &self.lock_options)?;
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

        meta_data.files.retain(|entry| {
            entry.original_name != normalized && !entry.original_name.starts_with(&directory_prefix)
        });
        self.write_meta(&header, &meta_data, &master_key)?;
        drop(config);
        drop(file_guards);
        drop(namespace);
        drop(maintenance);
        self.try_cleanup(password);

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

    /// 只修改密码保护层，不重新加密文件内容。
    ///
    /// 数据主密钥保持不变，因此所有 `.enc` 文件无需改动；本方法只生成新的 salt、
    /// 新密码派生密钥和新的元数据 nonce，并原子替换 `.veil-info`。
    pub fn change_password(&self, old_password: &str, new_password: &str) -> Result<(), VeilError> {
        if new_password.is_empty() {
            return Err(VeilError::WorkError("新密码不能为空".to_string()));
        }

        let _maintenance = locking::maintenance(&self.veil_dir, false, &self.lock_options)?;
        let config = locking::config(&self.veil_dir, true, &self.lock_options)?;
        let (meta_data, old_header, _) = self.read_meta_context(old_password)?;
        drop(config);
        let (new_header, new_master_key) = self.build_password_header(&old_header, new_password)?;
        let _config = locking::config(&self.veil_dir, false, &self.lock_options)?;
        self.write_meta(&new_header, &meta_data, &new_master_key)?;
        Ok(())
    }

    /// 修改密码并轮换数据主密钥，逐个重新加密文件。
    ///
    /// 操作开始前先把“新数据密钥、旧数据密钥”同时写入元数据。每个文件完成迁移后
    /// 各自提交一次元数据，因此进程被终止时只会出现明确的旧文件或新文件，而不会
    /// 存在无法判断使用哪把数据密钥的文件。全部文件迁移完成后丢弃旧数据密钥。
    pub fn change_password_full(
        &self,
        old_password: &str,
        new_password: &str,
    ) -> Result<(), VeilError> {
        if new_password.is_empty() {
            return Err(VeilError::WorkError("新密码不能为空".to_string()));
        }

        let _maintenance = locking::maintenance(&self.veil_dir, false, &self.lock_options)?;
        let config = locking::config(&self.veil_dir, true, &self.lock_options)?;
        let (mut meta_data, old_header, old_master_key) = self.read_meta_context(old_password)?;
        drop(config);

        // 已经处于双密钥状态时视为继续未完成的迁移，不生成第三把密钥。
        if meta_data.data_keys.len() == 1 {
            let old_data_key = meta_data.data_keys[0];
            let new_data_key = generate_random_bytes::<DATA_KEY_LEN>();
            meta_data.data_keys = vec![new_data_key, old_data_key];
        } else if meta_data.data_keys.len() != 2 {
            return Err(VeilError::InvalidFormat("数据主密钥槽位无效".to_string()));
        }

        // 先让双密钥状态成为可恢复的提交点。文件内容读写统一交给 file_ops，
        // veil_ops 只负责元数据事务和迁移顺序。
        {
            let _config = locking::config(&self.veil_dir, false, &self.lock_options)?;
            self.write_meta(&old_header, &meta_data, &old_master_key)?;
        }

        let active_data_key = meta_data.data_keys[0];
        let file_count = meta_data.files.len();

        for index in 0..file_count {
            let file_entry = meta_data.files[index].clone();
            let _file = locking::file(
                &self.veil_dir,
                &file_entry.file_id,
                false,
                &self.lock_options,
            )?;
            let encrypted_path = self.veil_dir.join(&file_entry.encrypted_name);
            let key_index = file_ops::detect_key_index(
                &encrypted_path,
                &meta_data.data_keys,
                &file_entry.nonce,
                file_entry.size,
            )?;

            // 已经使用当前数据密钥的文件无需再次迁移。
            if key_index == 0 {
                continue;
            }

            let new_nonce = file_ops::generate_base_nonce();
            let new_encrypted_name = format!("{}.enc", generate_random_id());
            let new_encrypted_path = self.veil_dir.join(&new_encrypted_name);
            file_ops::rewrite_file_streaming(
                &meta_data.data_keys[1],
                &active_data_key,
                &encrypted_path,
                &new_encrypted_path,
                &file_entry.nonce,
                &new_nonce,
                file_entry.size,
            )?;
            crate::fsutil::sync_directory(&self.veil_dir)?;

            let mut rewritten_entry = file_entry;
            rewritten_entry.encrypted_name = new_encrypted_name;
            rewritten_entry.nonce = new_nonce;
            rewritten_entry.encrypted_at = chrono::Utc::now().to_rfc3339();
            meta_data.files[index] = rewritten_entry;

            let config = locking::config(&self.veil_dir, false, &self.lock_options)?;
            let result = self.write_meta(&old_header, &meta_data, &old_master_key);
            drop(config);
            if let Err(error) = result {
                if !matches!(error, VeilError::CommitUncertain(_)) {
                    let _ = fs::remove_file(&new_encrypted_path);
                }
                return Err(error);
            }

            self.cleanup_work_state(&meta_data);
        }

        // 所有文件都已使用 active_data_key，可以丢弃旧密钥并把密码切换到新值。
        meta_data.data_keys.truncate(1);
        let (new_header, new_master_key) = self.build_password_header(&old_header, new_password)?;
        {
            let _config = locking::config(&self.veil_dir, false, &self.lock_options)?;
            self.write_meta(&new_header, &meta_data, &new_master_key)?;
        }
        self.cleanup_work_state(&meta_data);

        Ok(())
    }

    /// 为改密生成新的 salt、元数据 nonce 和密码派生密钥。
    fn build_password_header(
        &self,
        old_header: &MetaHeader,
        new_password: &str,
    ) -> Result<(MetaHeader, Zeroizing<[u8; 32]>), VeilError> {
        let new_salt = generate_random_bytes::<32>();
        let new_master_key = derive_master_key(new_password, &new_salt)?;
        let new_header = MetaHeader::new(
            new_salt,
            generate_random_bytes::<12>(),
            AlgorithmId::ChaCha20Poly1305,
            old_header.veil_id.clone(),
            old_header.veil_name.clone(),
            old_header.created_at.clone(),
        );
        Ok((new_header, new_master_key))
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
            return Err(VeilError::WorkError(
                "源路径和目标路径都不能是容器根目录".to_string(),
            ));
        }

        let source = normalize_veil_dir(from)?;
        let destination = normalize_veil_dir(to)?;
        let destination_ends_with_separator = to.ends_with('/') || to.ends_with('\\');

        let _maintenance = locking::maintenance(&self.veil_dir, true, &self.lock_options)?;
        let _namespace = locking::namespace(&self.veil_dir, false, &self.lock_options)?;
        let config = locking::config(&self.veil_dir, true, &self.lock_options)?;
        let (snapshot, _, _) = self.read_meta_context(password)?;
        let source_prefix = format!("{source}/");
        let mut ids: Vec<_> = snapshot
            .files
            .iter()
            .filter(|entry| {
                entry.original_name == source || entry.original_name.starts_with(&source_prefix)
            })
            .map(|entry| entry.file_id.as_str())
            .collect();
        ids.sort_unstable();
        drop(config);
        let mut _files = Vec::new();
        for id in ids {
            _files.push(locking::file(
                &self.veil_dir,
                id,
                false,
                &self.lock_options,
            )?);
        }
        let _config = locking::config(&self.veil_dir, false, &self.lock_options)?;
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
            return Err(VeilError::WorkError(format!(
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
                .ok_or_else(|| VeilError::WorkError("源路径缺少名称".to_string()))?;
            if destination.is_empty() {
                source_name.to_string()
            } else {
                format!("{destination}/{source_name}")
            }
        } else {
            destination.clone()
        };

        if final_target == source {
            return Err(VeilError::WorkError("源路径和目标路径相同".to_string()));
        }

        let final_prefix = format!("{final_target}/");
        if source_kind == MovedPathKind::Directory
            && (final_target.starts_with(&source_prefix) || final_target == source)
        {
            return Err(VeilError::WorkError(
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

    /// 清理崩溃遗留的临时文件，以及不再被当前元数据引用的随机密文文件。
    ///
    /// 只有成功解密元数据后才调用，因此清理依据始终是已提交快照。未知文件名不会
    /// 被删除，避免误伤工作区中的非 Veil 文件。
    fn cleanup_work_state(&self, meta_data: &MetaData) {
        let referenced: HashSet<&str> = meta_data
            .files
            .iter()
            .map(|entry| entry.encrypted_name.as_str())
            .collect();

        let Ok(entries) = fs::read_dir(&self.veil_dir) else {
            return;
        };

        let mut candidates = Vec::new();
        for entry in entries.flatten() {
            if !entry
                .file_type()
                .map(|kind| kind.is_file())
                .unwrap_or(false)
            {
                continue;
            }

            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            let orphaned_ciphertext =
                is_work_ciphertext_name(file_name) && !referenced.contains(file_name);
            if !crate::fsutil::is_internal_temp_name(file_name) && !orphaned_ciphertext {
                continue;
            }

            candidates.push(entry.path());
        }

        if candidates.is_empty() {
            return;
        }

        // 先稳定当前元数据快照，再删除任何旧密文；否则断电后旧元数据可能重新出现，
        // 而它引用的文件已经被删除。
        if crate::fsutil::sync_directory(&self.veil_dir).is_err() {
            return;
        }

        for path in candidates {
            #[cfg(test)]
            crate::fsutil::fault_point("cleanup");
            let _ = fs::remove_file(path);
        }
        let _ = crate::fsutil::sync_directory(&self.veil_dir);
    }

    /// 只有所有普通操作都已结束时才清理，避免误删尚未提交的密文。
    fn try_cleanup(&self, password: &str) {
        let options = LockOptions {
            timeout: Some(std::time::Duration::ZERO),
            cancellation: None,
        };
        let Ok(_maintenance) = locking::maintenance(&self.veil_dir, false, &options) else {
            return;
        };
        let Ok(_config) = locking::config(&self.veil_dir, true, &options) else {
            return;
        };
        if let Ok((metadata, _, _)) = self.read_meta_context(password) {
            self.cleanup_work_state(&metadata);
        }
    }
}

/// 持有目标文件独占锁时校验调用方的编辑基础版本。
fn validate_base_version(metadata: &MetaData, file: &AddFileSpec) -> Result<(), VeilError> {
    if file.reject_existing
        && metadata.files.iter().any(|entry| {
            entry.original_name == file.target
                || entry
                    .original_name
                    .starts_with(&format!("{}/", file.target))
                || file
                    .target
                    .starts_with(&format!("{}/", entry.original_name))
        })
    {
        return Err(VeilError::FileAlreadyExists(format!(
            "文件或目录 '{}' 已存在",
            file.target
        )));
    }
    let Some(base) = &file.base_version else {
        return Ok(());
    };
    if base.veil_id != metadata.veil_id {
        return Err(VeilError::VersionConflict("基础版本属于其他容器".into()));
    }
    let current = metadata.find_file(&file.target).ok_or_else(|| {
        VeilError::VersionConflict(format!("文件 '{}' 已删除或移动", file.target))
    })?;
    if base.file_id != current.file_id {
        return Err(VeilError::VersionConflict(format!(
            "文件 '{}' 的身份已变化",
            file.target
        )));
    }
    let relation = compare_version_history(&current.version_history, &base.version_history)?;
    let reason = match relation {
        VersionRelation::Same => return Ok(()),
        VersionRelation::IncomingAhead => "基础版本领先当前版本",
        VersionRelation::LocalAhead => "当前版本已更新",
        VersionRelation::Diverged => "版本链已分叉",
        VersionRelation::Unrelated => "版本链没有共同起点",
    };
    Err(VeilError::VersionConflict(format!(
        "文件 '{}'：{reason}",
        file.target
    )))
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

/// 使用系统随机源生成指定长度的字节数组。
///
/// # Panics
/// 系统随机源不可用时 panic，以避免使用可预测的密钥材料。
fn generate_random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::getrandom(&mut bytes).expect("生成随机数失败");
    bytes
}

/// 生成 16 字节随机 ID，并编码为 32 个十六进制字符。
fn generate_random_id() -> String {
    let bytes = generate_random_bytes::<16>();
    hex::encode(bytes)
}

/// 判断文件名是否是工作区当前格式生成的随机密文名。
fn is_work_ciphertext_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".enc") else {
        return false;
    };
    stem.len() == 32 && stem.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// 校验并规范化容器内相对路径，统一使用 `/` 分隔。
pub fn normalize_veil_dir(path: &str) -> Result<String, VeilError> {
    if path.is_empty() {
        return Err(VeilError::WorkError("容器内目标路径不能为空".to_string()));
    }

    let mut parts = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => {
                let part = part
                    .to_str()
                    .ok_or_else(|| VeilError::WorkError("目标路径不是有效 UTF-8".to_string()))?;
                parts.push(part.to_string());
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(VeilError::WorkError(format!(
                    "目标路径必须是容器内相对路径: {}",
                    path
                )));
            }
        }
    }

    if parts.is_empty() {
        return Err(VeilError::WorkError("容器内目标路径不能为空".to_string()));
    }

    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kdf;
    use tempfile::TempDir;

    #[test]
    fn fault_writer_worker() {
        let Some(root) = std::env::var_os("VEIL_FAULT_TEST_ROOT") else {
            return;
        };
        kdf::enable_fast_test_kdf();
        let root = PathBuf::from(root);
        let manager = VeilManager::new(root.join("veil"));
        let result = manager.add_files(
            &[AddFileSpec::new(root.join("new"), "file.txt")],
            "password",
        );
        if std::env::var_os("VEIL_TEST_PAUSEPOINT").is_some() {
            assert!(result.is_ok(), "{result:?}");
            return;
        }
        if std::env::var("VEIL_TEST_FAILPOINT").as_deref() == Ok("after_replace_error") {
            assert!(matches!(result, Err(VeilError::CommitUncertain(_))));
        } else {
            panic!("故障注入点未触发: {result:?}");
        }
    }

    /// 每个发布阶段由独立进程退出，确认重开后只能看到完整的新版本或旧版本。
    #[test]
    fn commit_fault_points_preserve_readable_versions() {
        use std::process::{Command, Stdio};
        kdf::enable_fast_test_kdf();
        for (stage, published) in [
            ("file_locked", false),
            ("cipher_written", false),
            ("config_temp_written", false),
            ("before_replace", false),
            ("after_replace", true),
            ("after_commit", true),
            ("cleanup", true),
            ("after_replace_error", true),
        ] {
            let root = TempDir::new().unwrap();
            let manager = VeilManager::new(root.path().join("veil"));
            manager.init_veil("test", "password").unwrap();
            let old = root.path().join("old");
            fs::write(&old, b"old contents").unwrap();
            fs::write(root.path().join("new"), b"new contents").unwrap();
            manager
                .add_files(&[AddFileSpec::new(&old, "file.txt")], "password")
                .unwrap();
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "veil_ops::tests::fault_writer_worker",
                    "--nocapture",
                ])
                .env("VEIL_FAULT_TEST_ROOT", root.path())
                .env("VEIL_TEST_FAILPOINT", stage)
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .status()
                .unwrap();
            if stage == "after_replace_error" {
                assert!(status.success(), "{stage}");
            } else {
                assert_eq!(status.code(), Some(86), "{stage}");
            }
            let output = root.path().join("output");
            manager
                .extract_file("file.txt", &output, "password")
                .unwrap();
            assert_eq!(
                fs::read(&output).unwrap(),
                if published {
                    &b"new contents"[..]
                } else {
                    &b"old contents"[..]
                },
                "{stage}"
            );
            let entries = manager.list_files("password").unwrap();
            assert_eq!(
                entries[0].version_history.len(),
                if published { 2 } else { 1 },
                "{stage}"
            );
            manager
                .add_files(&[AddFileSpec::new(&old, "file.txt")], "password")
                .unwrap();
        }
    }

    #[test]
    fn active_writer_excludes_same_file_but_allows_other_file() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        kdf::enable_fast_test_kdf();
        let root = TempDir::new().unwrap();
        let veil_dir = root.path().join("veil");
        let manager = VeilManager::new(veil_dir.clone());
        manager.init_veil("test", "password").unwrap();
        let old = root.path().join("old");
        fs::write(&old, b"old").unwrap();
        fs::write(root.path().join("new"), b"new").unwrap();
        manager
            .add_files(
                &[
                    AddFileSpec::new(&old, "file.txt"),
                    AddFileSpec::new(&old, "other.txt"),
                ],
                "password",
            )
            .unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "veil_ops::tests::fault_writer_worker",
                "--nocapture",
            ])
            .env("VEIL_FAULT_TEST_ROOT", root.path())
            .env("VEIL_TEST_PAUSEPOINT", "cipher_written")
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let start = Instant::now();
        while !root.path().join("paused").exists() {
            assert!(child.try_wait().unwrap().is_none(), "写入进程提前退出");
            assert!(start.elapsed() < Duration::from_secs(20), "写入进程未暂停");
            std::thread::sleep(Duration::from_millis(10));
        }
        let limited = VeilManager::new(veil_dir).with_lock_options(LockOptions {
            timeout: Some(Duration::from_millis(100)),
            cancellation: None,
        });
        assert!(
            limited
                .extract_file("file.txt", &root.path().join("blocked"), "password")
                .is_err()
        );
        assert!(
            limited
                .add_files(&[AddFileSpec::new(&old, "file.txt")], "password")
                .is_err()
        );
        manager
            .add_files(&[AddFileSpec::new(&old, "other.txt")], "password")
            .unwrap();
        fs::write(root.path().join("resume"), b"resume").unwrap();
        assert!(child.wait().unwrap().success());
        let files = manager.list_files("password").unwrap();
        assert!(files.iter().all(|file| file.version_history.len() == 2));
        manager
            .extract_file("file.txt", &root.path().join("result"), "password")
            .unwrap();
        assert_eq!(fs::read(root.path().join("result")).unwrap(), b"new");
    }

    /// 验证双数据密钥状态可以同时读取迁移前和迁移后的文件。
    #[test]
    fn mixed_data_keys_fall_back_to_old_key() {
        kdf::enable_fast_test_kdf();
        let temp_dir = TempDir::new().unwrap();
        let veil_dir = temp_dir.path().join("mixed-data-keys");
        let manager = VeilManager::new(veil_dir.clone());
        let password = "test-password";

        manager.init_veil("mixed-data-keys", password).unwrap();
        let first = temp_dir.path().join("first.txt");
        let second = temp_dir.path().join("second.txt");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        manager
            .add_files(
                &[
                    AddFileSpec::new(&first, "first.txt"),
                    AddFileSpec::new(&second, "second.txt"),
                ],
                password,
            )
            .unwrap();

        let (mut meta_data, header, master_key) = manager.read_meta_context(password).unwrap();
        let old_key = meta_data.data_keys[0];
        let new_key = generate_random_bytes::<DATA_KEY_LEN>();
        meta_data.data_keys = vec![new_key, old_key];

        let first_index = meta_data
            .files
            .iter()
            .position(|entry| entry.original_name == "first.txt")
            .unwrap();
        let first_entry = meta_data.files[first_index].clone();
        let old_encrypted_path = veil_dir.join(&first_entry.encrypted_name);
        let new_nonce = file_ops::generate_base_nonce();
        let new_encrypted_name = format!("{}.enc", generate_random_id());
        file_ops::rewrite_file_streaming(
            &old_key,
            &new_key,
            &old_encrypted_path,
            &veil_dir.join(&new_encrypted_name),
            &first_entry.nonce,
            &new_nonce,
            first_entry.size,
        )
        .unwrap();
        meta_data.files[first_index] = FileEntry::new(
            new_encrypted_name,
            first_entry.original_name,
            first_entry.size,
            new_nonce,
        );
        manager
            .write_meta(&header, &meta_data, &master_key)
            .unwrap();
        manager.cleanup_work_state(&meta_data);

        let first_output = temp_dir.path().join("first-out.txt");
        let second_output = temp_dir.path().join("second-out.txt");
        manager
            .extract_file("first.txt", &first_output, password)
            .unwrap();
        manager
            .extract_file("second.txt", &second_output, password)
            .unwrap();

        assert_eq!(fs::read(first_output).unwrap(), b"first");
        assert_eq!(fs::read(second_output).unwrap(), b"second");
    }
}
