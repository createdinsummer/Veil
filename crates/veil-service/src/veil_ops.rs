//! SQLCipher 容器操作：文件内容留在磁盘，数据库是唯一权威清单。
use crate::error::{Result, VeilError};
use crate::file_ops;
use crate::metadata::{FileEntry, MetaData};
use crate::storage::{
    container::{self, ContainerDb, DataKey, Node},
    id, invalid, json,
    locks::{self, LeaseGuard, LockManager, LockMode, LockOptions},
    now,
};
use crate::version::{VersionRelation, VersionSnapshot, compare_version_history};
use rusqlite::{TransactionBehavior, params};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};
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

/// 容器内路径的类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerPathKind {
    File(String),
    Directory(String),
    Missing(String),
}

#[derive(Clone)]
pub struct VeilManager {
    pub veil_dir: PathBuf,
    expected_veil_id: Option<String>,
    lock_options: LockOptions,
}
struct Pending {
    spec: AddFileSpec,
    parent: String,
    name: String,
    base: Option<Node>,
    identity: String,
    key: DataKey,
    nonce: [u8; 12],
    cipher: String,
    size: u64,
    base_key: Option<DataKey>,
    unchanged: bool,
}
impl VeilManager {
    pub fn new(veil_dir: PathBuf) -> Self {
        Self {
            veil_dir,
            expected_veil_id: None,
            lock_options: locks::default_options(),
        }
    }
    pub fn for_veil(veil_dir: PathBuf, veil_id: impl Into<String>) -> Self {
        Self {
            veil_dir,
            expected_veil_id: Some(veil_id.into()),
            lock_options: locks::default_options(),
        }
    }
    pub fn with_lock_options(mut self, options: LockOptions) -> Self {
        self.lock_options = options;
        self
    }
    pub(crate) fn open_db(&self, password: &str) -> Result<ContainerDb> {
        let db = ContainerDb::open(&self.veil_dir, password, self.expected_veil_id.as_deref())?;
        // 展示缓存更新失败不影响独立容器解锁，也不能替代容器业务状态。
        let _ = self.refresh_local_cache(&db);
        Ok(db)
    }
    fn refresh_local_cache(&self, db: &ContainerDb) -> Result<()> {
        use crate::storage::app::{AppStore, RecentContainer, app_data_dir};
        let directory = app_data_dir()?;
        if !directory.join(veil_core::database::APP_DB).try_exists()? {
            return Ok(());
        }
        let mut store = AppStore::open(&directory)?;
        let (identity, name, created, _) = db.info()?;
        let Some(registration) = store
            .registrations()?
            .into_iter()
            .find(|r| r.div_id == identity)
        else {
            return Ok(());
        };
        let registered = store
            .spaces()?
            .into_iter()
            .find(|s| s.id == registration.space_id)
            .and_then(|s| s.local_path)
            .ok_or_else(|| invalid("空间未绑定本机目录"))?
            .join(&identity);
        let actual = fs::canonicalize(&self.veil_dir)?;
        if fs::canonicalize(registered)? != actual {
            return Err(VeilError::VeilIdConflict(
                "本机登记位置与解锁位置不一致".into(),
            ));
        }
        store.register(&registration.space_id, &identity, &name)?;
        store.cache_container_identity(&identity, &container::timestamp(created)?)?;
        store.set_record(
            "recent_div",
            &identity,
            &RecentContainer {
                id: identity.clone(),
                name,
                path: actual.to_string_lossy().into_owned(),
            },
        )?;
        Ok(())
    }
    fn lock_manager(&self, password: &str) -> LockManager {
        LockManager::new(&self.veil_dir, password).with_options(self.lock_options.clone())
    }
    pub fn init_veil(&self, name: &str, password: &str) -> Result<MetaData> {
        self.init_veil_with_id(&crate::metadata::generate_veil_id(), name, password)
    }
    pub fn init_veil_with_id(
        &self,
        identity: &str,
        name: &str,
        password: &str,
    ) -> Result<MetaData> {
        if self
            .expected_veil_id
            .as_deref()
            .is_some_and(|e| e != identity)
        {
            return Err(VeilError::VeilIdConflict("初始化身份不一致".into()));
        }
        ContainerDb::create(&self.veil_dir, identity, name, password)?.metadata()
    }
    pub fn read_meta(&self, password: &str) -> Result<MetaData> {
        let db = self.open_db(password)?;
        // 单个读事务保证目录树、文件引用、密钥集合属于同一个快照。
        db.conn.execute_batch("BEGIN DEFERRED")?;
        let result = db.metadata();
        db.conn.execute_batch("ROLLBACK")?;
        result
    }
    pub fn read_meta_snapshot(&self, password: &str) -> Result<(MetaData, Vec<u8>)> {
        let temporary = tempfile::TempDir::new()?;
        let count = self.snapshot_to(temporary.path(), password)?;
        let metadata =
            ContainerDb::open(temporary.path(), password, self.expected_veil_id.as_deref())?
                .metadata()?;
        if count != metadata.files.len() {
            return Err(invalid("快照文件数量不一致"));
        }
        Ok((
            metadata,
            fs::read(temporary.path().join(veil_core::database::CONTAINER_DB))?,
        ))
    }
    pub fn update_meta<F: FnOnce(&mut MetaData)>(&self, password: &str, update: F) -> Result<()> {
        let mut metadata = self.read_meta(password)?;
        let old = metadata.clone();
        update(&mut metadata);
        if metadata.veil_id != old.veil_id
            || metadata.created_at != old.created_at
            || metadata.files.len() != old.files.len()
            || metadata.data_keys != old.data_keys
            || metadata
                .files
                .iter()
                .zip(&old.files)
                .any(|(a, b)| serde_json::to_string(a).ok() != serde_json::to_string(b).ok())
        {
            return Err(invalid("仅允许通过此入口修改容器名称"));
        }
        self.rename_container(&metadata.veil_name, password)
    }
    pub fn rename_container(&self, name: &str, password: &str) -> Result<()> {
        if name.trim().is_empty() {
            return Err(invalid("名称不能为空"));
        }
        let db = self.open_db(password)?;
        db.conn
            .execute("UPDATE div_info SET name=?1 WHERE id=1", [name])?;
        let _ = self.refresh_local_cache(&db);
        Ok(())
    }
    pub fn list_files(&self, password: &str) -> Result<Vec<FileEntry>> {
        Ok(self.read_meta(password)?.files.clone())
    }
    pub fn list_children(
        &self,
        parent: &str,
        offset: u32,
        limit: u32,
        password: &str,
    ) -> Result<Vec<Node>> {
        self.open_db(password)?.children(parent, offset, limit)
    }
    pub fn create_directory(&self, path: &str, password: &str) -> Result<String> {
        let path = normalize_veil_dir(path)?;
        let mut db = self.open_db(password)?;
        let tx = db
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let node = container::ensure_directory(&tx, &path)?;
        tx.commit()?;
        Ok(node.id)
    }
    pub fn path_kind(&self, path: &str, password: &str) -> Result<ContainerPathKind> {
        let db = self.open_db(password)?;
        let normalized = if path == "." || path == "/" || path.is_empty() {
            String::new()
        } else {
            normalize_veil_dir(path)?
        };
        Ok(match db.resolve(&normalized)? {
            Some(n) if n.kind == "file" => ContainerPathKind::File(normalized),
            Some(_) => ContainerPathKind::Directory(normalized),
            None => ContainerPathKind::Missing(normalized),
        })
    }
    pub fn add_file(&self, path: &Path, password: &str) -> Result<String> {
        let target = path
            .file_name()
            .and_then(|p| p.to_str())
            .ok_or_else(|| invalid("源文件名无效"))?;
        Ok(self
            .add_files(&[AddFileSpec::new(path, target)], password)?
            .remove(0))
    }
    pub fn add_files(&self, files: &[AddFileSpec], password: &str) -> Result<Vec<String>> {
        let execution = crate::ExecutionService::global()?;
        let _scope = execution.enter_task("写入文件");
        let mut db = self.open_db(password)?;
        let veil_id = db.info()?.0;
        let tx = db
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let key_ring = container::keys(&tx)?;
        let key = key_ring
            .clone()
            .into_iter()
            .find(|k| k.status == "active")
            .unwrap();
        let mut targets = HashSet::new();
        let mut pending = Vec::new();
        for spec in files {
            let target = normalize_veil_dir(&spec.target)?;
            if !targets.insert(target.clone()) {
                return Err(invalid("批量目标重复"));
            }
            if !spec.source.is_file() {
                return Err(invalid("源路径必须是文件"));
            }
            let (parent_path, name) = split_parent(&target);
            let parent = container::ensure_directory(&tx, parent_path)?;
            let base = container::resolve(&tx, &target)?;
            if base.as_ref().is_some_and(|n| n.kind != "file") {
                return Err(VeilError::FileAlreadyExists(target));
            }
            if spec.reject_existing && base.is_some() {
                return Err(VeilError::FileAlreadyExists(target));
            }
            if let Some(version) = &spec.base_version {
                verify_base(&veil_id, base.as_ref(), version)?;
            }
            let identity = base
                .as_ref()
                .map(|n| n.id.clone())
                .unwrap_or_else(crate::metadata::generate_file_id);
            pending.push(Pending {
                spec: spec.clone(),
                parent: parent.id,
                name: name.into(),
                base_key: base
                    .as_ref()
                    .map(|n| {
                        key_ring
                            .iter()
                            .find(|k| Some(&k.key_id) == n.key_id.as_ref())
                            .cloned()
                            .ok_or_else(|| invalid("文件引用未知密钥"))
                    })
                    .transpose()?,
                base,
                identity,
                key: key.clone(),
                nonce: file_ops::generate_base_nonce(),
                cipher: String::new(),
                size: 0,
                unchanged: false,
            });
        }
        tx.commit()?;
        drop(db);
        let guard = self.lock_manager(password).acquire(
            &pending
                .iter()
                .map(|p| p.identity.clone())
                .collect::<Vec<_>>(),
            LockMode::Write,
        )?;
        let credentials = guard.credentials()?;
        // 申请锁可能等待其他写入完成；先复核快照，避免访问已经失去引用的旧密文。
        let current = self.open_db(password)?;
        for p in &pending {
            if let Some(base) = &p.base {
                if current.node(&p.identity)?.as_ref() != Some(base) {
                    return Err(VeilError::VersionConflict("文件在申请锁期间已变化".into()));
                }
            }
        }
        drop(current);
        for p in &mut pending {
            let credential = credentials
                .iter()
                .find(|c| c.resource_id == p.identity)
                .unwrap();
            let staging = self.veil_dir.join(".staging").join(&credential.lock_id);
            crate::fsutil::create_dir_all_durable(&staging)?;
            p.cipher = format!("{}_{}.enc", credential.lock_id, id());
            let staged = staging.join(&p.cipher);
            fault("before_cipher_write");
            p.size = file_ops::encrypt_file_streaming(
                &p.spec.source,
                &staged,
                &*p.key.material()?,
                &p.nonce,
            )?;
            fault("after_cipher_write");
            crate::fsutil::rename_no_replace(&staged, &self.veil_dir.join(&p.cipher))?;
            crate::fsutil::sync_directory(&self.veil_dir)?;
            fault("after_cipher_persist");
            if let Some(base) = &p.base {
                if base.size == Some(p.size) {
                    p.unchanged = file_ops::plaintext_digest(
                        &self.veil_dir.join(base.cipher()?),
                        &*p.base_key.as_ref().unwrap().material()?,
                        &base.nonce()?,
                        p.size,
                    )? == file_ops::plaintext_digest(
                        &self.veil_dir.join(&p.cipher),
                        &*p.key.material()?,
                        &p.nonce,
                        p.size,
                    )?;
                }
            }
        }
        // 重新打开可识别其他进程改密；旧密码连接不能提交较早启动的写入。
        let mut db = self.open_db(password)?;
        if db.info()?.0 != veil_id {
            return Err(VeilError::VeilIdConflict("写入期间容器身份已变化".into()));
        }
        let tx = db
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active = container::keys(&tx)?
            .into_iter()
            .find(|k| k.status == "active")
            .unwrap();
        let credentials = guard.credentials()?;
        let mut obsolete = Vec::new();
        for p in &pending {
            let c = credentials
                .iter()
                .find(|c| c.resource_id == p.identity)
                .unwrap();
            locks::validate(&tx, c, Some(LockMode::Write))?;
            if p.key.key_id != active.key_id {
                return Err(invalid("活动文件密钥已变化，请重试"));
            }
            let parent = container::node(&tx, &p.parent)?.ok_or_else(|| invalid("父目录已删除"))?;
            if parent.kind != "directory" {
                return Err(invalid("父节点不是目录"));
            }
            let current = container::node(&tx, &p.identity)?;
            let mut versions = if let Some(base) = &p.base {
                let current = current
                    .as_ref()
                    .ok_or_else(|| VeilError::VersionConflict("文件已删除".into()))?;
                if current.parent_id != base.parent_id || current.name != base.name {
                    return Err(VeilError::VersionConflict("文件位置已变化".into()));
                }
                verify_base(
                    &veil_id,
                    Some(current),
                    &VersionSnapshot {
                        veil_id: veil_id.clone(),
                        file_id: base.id.clone(),
                        version_history: base.versions()?.to_vec(),
                    },
                )?;
                obsolete.push(current.cipher()?.to_owned());
                current.versions()?.to_vec()
            } else {
                if current.is_some() {
                    return Err(VeilError::VersionConflict("文件身份已被占用".into()));
                }
                Vec::new()
            };
            if !p.unchanged {
                versions.push(crate::metadata::generate_version_id());
            }
            let size = i64::try_from(p.size).map_err(|_| invalid("文件过大"))?;
            if let Some(base) = &p.base {
                let updated = if p.unchanged { base.updated_at } else { now() };
                tx.execute("UPDATE file_info SET cipher_name=?1,size=?2,key_id=?3,nonce=?4,encrypted_at=?5,updated_at=?8,version_history=?6 WHERE id=?7",params![p.cipher,size,active.key_id,p.nonce.as_slice(),now(),json(&versions)?,p.identity,updated])?;
            } else {
                tx.execute(
                    "INSERT INTO file_info VALUES(?1,?2,?3,'file',?4,?4,?5,?6,?7,?8,?4,?9)",
                    params![
                        p.identity,
                        p.parent,
                        p.name,
                        now(),
                        p.cipher,
                        size,
                        active.key_id,
                        p.nonce.as_slice(),
                        json(&versions)?
                    ],
                )
                .map_err(|error| match error.sqlite_error() {
                    Some(code) if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE => {
                        VeilError::FileAlreadyExists(p.spec.target.clone())
                    }
                    _ => error.into(),
                })?;
            }
            locks::release_in(&tx, c)?;
        }
        fault("before_metadata_commit");
        let commit = tx.commit();
        drop(db);
        #[cfg(feature = "fault-injection")]
        let injected_uncertainty = std::env::var_os("VEIL_TEST_COMMIT_UNCERTAIN").is_some();
        #[cfg(not(feature = "fault-injection"))]
        let injected_uncertainty = false;
        if commit.is_err() || injected_uncertainty {
            // 提交错误不能等同于回滚；只在重新确认全部引用后返回成功。
            let confirmed = self.open_db(password).and_then(|db| {
                for file in &pending {
                    if db
                        .node(&file.identity)?
                        .and_then(|node| node.cipher_name)
                        .as_deref()
                        != Some(file.cipher.as_str())
                    {
                        return Ok(false);
                    }
                }
                Ok(true)
            });
            if !matches!(confirmed, Ok(true)) {
                return Err(VeilError::CommitUncertain(std::io::Error::other(
                    "无法确认文件写入事务结果，请重新读取容器状态",
                )));
            }
        }
        fault("after_metadata_commit");
        drop(guard);
        let _ = self.cleanup(password);
        Ok(pending.into_iter().map(|p| p.cipher).collect())
    }
    fn read_locked(
        &self,
        name: &str,
        password: &str,
    ) -> Result<(ContainerDb, Node, LeaseGuard, Zeroizing<[u8; 32]>)> {
        let db = self.open_db(password)?;
        let identity = db.info()?.0;
        let node = db
            .resolve(name)?
            .filter(|n| n.kind == "file")
            .ok_or_else(|| VeilError::FileNotFound(name.into()))?;
        let guard = self
            .lock_manager(password)
            .acquire(std::slice::from_ref(&node.id), LockMode::Read)?;
        drop(db);
        let db = self.open_db(password)?;
        if db.info()?.0 != identity {
            return Err(VeilError::VeilIdConflict("读取期间容器身份已变化".into()));
        }
        for credential in guard.credentials()? {
            locks::validate(&db.conn, &credential, Some(LockMode::Read))?;
        }
        let node = db
            .node(&node.id)?
            .ok_or_else(|| VeilError::FileNotFound(name.into()))?;
        let key = db
            .keys()?
            .into_iter()
            .find(|k| Some(&k.key_id) == node.key_id.as_ref())
            .ok_or_else(|| invalid("文件引用未知密钥"))?
            .material()?;
        Ok((db, node, guard, key))
    }
    pub fn extract_file(&self, name: &str, output: &Path, password: &str) -> Result<()> {
        self.extract_file_with_version(name, output, password)
            .map(|_| ())
    }
    pub fn extract_file_with_version(
        &self,
        name: &str,
        output: &Path,
        password: &str,
    ) -> Result<VersionSnapshot> {
        self.extract_file_inner(name, output, password, true)
    }
    fn extract_file_inner(
        &self,
        name: &str,
        output: &Path,
        password: &str,
        overwrite: bool,
    ) -> Result<VersionSnapshot> {
        let execution = crate::ExecutionService::global()?;
        let _scope = execution.enter_task("读取文件");
        let (db, node, guard, key) = self.read_locked(name, password)?;
        let version = VersionSnapshot {
            veil_id: db.info()?.0,
            file_id: node.id.clone(),
            version_history: node.versions()?.to_vec(),
        };
        drop(db);
        let parent = output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        crate::fsutil::create_dir_all_durable(parent)?;
        let temporary = tempfile::NamedTempFile::new_in(parent)?.into_temp_path();
        file_ops::decrypt_file_streaming(
            &self.veil_dir.join(node.cipher()?),
            &temporary,
            &key,
            &node.nonce()?,
            node.size.unwrap(),
        )?;
        fault("before_plaintext_publish");
        let mut fresh = self.open_db(password)?;
        let tx = fresh.conn.transaction()?;
        for credential in guard.credentials()? {
            locks::validate(&tx, &credential, Some(LockMode::Read))?;
        }
        if overwrite {
            temporary
                .persist(output)
                .map_err(|e| VeilError::Io(e.error))?;
        } else {
            temporary
                .persist_noclobber(output)
                .map_err(|e| VeilError::Io(e.error))?;
        }
        crate::fsutil::sync_directory(parent)?;
        tx.commit()?;
        Ok(version)
    }
    pub fn extract_file_without_overwrite(
        &self,
        name: &str,
        output: &Path,
        password: &str,
    ) -> Result<()> {
        self.extract_file_inner(name, output, password, false)
            .map(|_| ())
    }
    pub fn read_file_bytes(&self, name: &str, password: &str, limit: u64) -> Result<Vec<u8>> {
        let execution = crate::ExecutionService::global()?;
        let _scope = execution.enter_task("预览文件");
        let (db, node, guard, key) = self.read_locked(name, password)?;
        if node.size.unwrap() > limit {
            return Err(invalid("文件超过预览大小限制"));
        }
        let mut bytes = Vec::with_capacity(node.size.unwrap() as usize);
        file_ops::decrypt_file_to_writer(
            &self.veil_dir.join(node.cipher()?),
            &mut bytes,
            &key,
            &node.nonce()?,
            node.size.unwrap(),
        )?;
        for c in guard.credentials()? {
            locks::validate(&db.conn, &c, Some(LockMode::Read))?;
        }
        Ok(bytes)
    }
    pub fn remove_file(&self, name: &str, password: &str) -> Result<()> {
        self.remove_path(name, password).map(|_| ())
    }
    pub fn remove_path(&self, path: &str, password: &str) -> Result<RemovedPathKind> {
        let execution = crate::ExecutionService::global()?;
        let _scope = execution.enter_task("删除路径");
        let path = normalize_veil_dir(path)?;
        let db = self.open_db(password)?;
        let node = db
            .resolve(&path)?
            .ok_or_else(|| VeilError::FileNotFound(path.clone()))?;
        let before = db.subtree(&node.id)?;
        let files = before
            .iter()
            .filter(|n| n.kind == "file")
            .map(|n| n.id.clone())
            .collect::<Vec<_>>();
        drop(db);
        let guard = self
            .lock_manager(password)
            .acquire(&files, LockMode::Write)?;
        let mut db = self.open_db(password)?;
        let tx = db
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if container::subtree(&tx, &node.id)? != before {
            return Err(VeilError::VersionConflict(
                "删除前子树已变化，请重试".into(),
            ));
        }
        for c in guard.credentials()? {
            locks::release_in(&tx, &c)?;
        }
        tx.execute("WITH RECURSIVE tree(id) AS (SELECT id FROM file_info WHERE id=?1 UNION ALL SELECT f.id FROM file_info f JOIN tree t ON f.parent_id=t.id) DELETE FROM file_info WHERE id IN (SELECT id FROM tree)",[&node.id])?;
        tx.commit()?;
        drop(db);
        drop(guard);
        let _ = self.cleanup(password);
        Ok(if node.kind == "file" {
            RemovedPathKind::File
        } else {
            RemovedPathKind::Directory
        })
    }
    pub fn rename_file(&self, from: &str, to: &str, password: &str) -> Result<()> {
        self.move_path(from, to, password).map(|_| ())
    }
    pub fn move_path(
        &self,
        from: &str,
        to: &str,
        password: &str,
    ) -> Result<(MovedPathKind, String)> {
        let from = normalize_veil_dir(from)?;
        let mut to = normalize_veil_dir(to)?;
        let mut db = self.open_db(password)?;
        let tx = db
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let n =
            container::resolve(&tx, &from)?.ok_or_else(|| VeilError::FileNotFound(from.clone()))?;
        if let Some(destination) = container::resolve(&tx, &to)? {
            if destination.id == n.id {
                tx.commit()?;
                return Ok((
                    if n.kind == "file" {
                        MovedPathKind::File
                    } else {
                        MovedPathKind::Directory
                    },
                    from,
                ));
            }
            if destination.kind != "directory" {
                return Err(VeilError::FileAlreadyExists(to));
            }
            to = format!("{to}/{}", n.name);
        }
        let (parent, name) = split_parent(&to);
        let parent = container::resolve(&tx, parent)?
            .filter(|n| n.kind == "directory")
            .ok_or_else(|| invalid("目标父目录不存在"))?;
        let cycle:bool=tx.query_row("WITH RECURSIVE ancestors(id,parent_id) AS (SELECT id,parent_id FROM file_info WHERE id=?1 UNION ALL SELECT f.id,f.parent_id FROM file_info f JOIN ancestors a ON f.id=a.parent_id) SELECT EXISTS(SELECT 1 FROM ancestors WHERE id=?2)",params![parent.id,n.id],|r|r.get(0))?;
        if cycle {
            return Err(VeilError::WorkError("不能把目录移动到自身或子目录".into()));
        }
        tx.execute(
            "UPDATE file_info SET parent_id=?1,name=?2,updated_at=?3 WHERE id=?4",
            params![parent.id, name, now(), n.id],
        )?;
        tx.commit()?;
        Ok((
            if n.kind == "file" {
                MovedPathKind::File
            } else {
                MovedPathKind::Directory
            },
            to,
        ))
    }
    /// 仅清理由本协议命名的无引用对象；删除前持写事务重新核对运行锁及引用。
    pub fn cleanup(&self, password: &str) -> Result<usize> {
        let mut db = self.open_db(password)?;
        let dead = locks::dead_locks(&db.conn)?;
        let mut removed = 0;
        for entry in fs::read_dir(&self.veil_dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !entry.file_type()?.is_file() || container::validate_cipher(&name).is_err() {
                continue;
            }
            let lock_id = name.split('_').next().unwrap();
            let tx = db
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            locks::reap(&tx, &dead)?;
            let referenced:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM file_info WHERE cipher_name=?1) OR EXISTS(SELECT 1 FROM operation_locks WHERE lock_id=?2)",params![name,lock_id],|r|r.get(0))?;
            if !referenced {
                fs::remove_file(entry.path())?;
                removed += 1;
            }
            tx.commit()?;
        }
        let staging = self.veil_dir.join(".staging");
        if staging.is_dir() {
            for entry in fs::read_dir(&staging)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if !entry.file_type()?.is_dir() || !crate::metadata::is_uuid_v4(&name) {
                    continue;
                }
                let tx = db
                    .conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                locks::reap(&tx, &dead)?;
                let active: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM operation_locks WHERE lock_id=?1)",
                    [&name],
                    |r| r.get(0),
                )?;
                if !active {
                    fs::remove_dir_all(entry.path())?;
                    removed += 1;
                }
                tx.commit()?;
            }
        }
        Ok(removed)
    }
    pub fn change_password(&self, old: &str, new: &str) -> Result<()> {
        let db = self.open_db(old)?;
        let result = veil_core::database::rekey(&db.conn, new);
        drop(db);
        if self.open_db(new).is_err() {
            if self.open_db(old).is_ok() {
                return Err(result.err().unwrap_or_else(|| {
                    VeilError::EncryptionError("密码切换未生效，原密码仍有效".into())
                }));
            }
            return Err(VeilError::CommitUncertain(std::io::Error::other(
                "无法确认数据库改密结果，请重新验证数据库",
            )));
        }
        fault("after_database_rekey");
        Ok(())
    }
    pub fn change_password_full(&self, old: &str, new: &str) -> Result<()> {
        let execution = crate::ExecutionService::global()?;
        let _scope = execution.enter_task("完整改密");
        if new.is_empty() {
            return Err(invalid("密码不能为空"));
        }
        // 密码切换后、旧密钥移除前中断时，也允许以新密码继续。
        let (password, already_rekeyed) = match self.open_db(old) {
            Ok(_) => (old, false),
            Err(VeilError::DecryptionError(_)) => {
                self.open_db(new)?;
                (new, true)
            }
            Err(e) => return Err(e),
        };
        if already_rekeyed && self.open_db(new)?.keys()?.len() == 1 {
            let _ = self.cleanup(new);
            return Ok(());
        }
        let mut db = self.open_db(password)?;
        let tx = db
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut keys = container::keys(&tx)?;
        if keys.len() == 1 {
            keys[0].status = "retiring".into();
            keys.push(DataKey::generate());
            tx.execute(
                "UPDATE div_info SET data_keys=?1 WHERE id=1",
                [json(&keys)?],
            )?;
        }
        tx.commit()?;
        drop(db);
        fault("after_rotation_keys");
        loop {
            let db = self.open_db(password)?;
            let keys = db.keys()?;
            let active = keys.iter().find(|k| k.status == "active").unwrap();
            let candidate: Option<String> = {
                use rusqlite::OptionalExtension;
                db.conn
                    .query_row(
                        "SELECT id FROM file_info WHERE kind='file' AND key_id<>?1 LIMIT 1",
                        [&active.key_id],
                        |r| r.get(0),
                    )
                    .optional()?
            };
            let Some(identity) = candidate else {
                break;
            };
            drop(db);
            let guard = self
                .lock_manager(password)
                .acquire(std::slice::from_ref(&identity), LockMode::Write)?;
            let db = self.open_db(password)?;
            let n = db
                .node(&identity)?
                .ok_or_else(|| VeilError::FileNotFound(identity.clone()))?;
            let keys = db.keys()?;
            let active = keys.iter().find(|k| k.status == "active").unwrap();
            if n.key_id.as_deref() == Some(&active.key_id) {
                continue;
            }
            let previous = keys
                .iter()
                .find(|k| Some(&k.key_id) == n.key_id.as_ref())
                .ok_or_else(|| invalid("文件引用未知密钥"))?;
            let credential = guard.credentials()?.remove(0);
            let cipher = format!("{}_{}.enc", credential.lock_id, id());
            let nonce = file_ops::generate_base_nonce();
            let staging = self.veil_dir.join(".staging").join(&credential.lock_id);
            crate::fsutil::create_dir_all_durable(&staging)?;
            file_ops::rewrite_file_streaming(
                &*previous.material()?,
                &*active.material()?,
                &self.veil_dir.join(n.cipher()?),
                &staging.join(&cipher),
                &n.nonce()?,
                &nonce,
                n.size.unwrap(),
            )?;
            crate::fsutil::rename_no_replace(&staging.join(&cipher), &self.veil_dir.join(&cipher))?;
            crate::fsutil::sync_directory(&self.veil_dir)?;
            let active_id = active.key_id.clone();
            drop(db);
            let mut db = self.open_db(password)?;
            let tx = db
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let c = guard.credentials()?.remove(0);
            locks::validate(&tx, &c, Some(LockMode::Write))?;
            if container::node(&tx, &identity)?.as_ref() != Some(&n)
                || !container::keys(&tx)?
                    .iter()
                    .any(|k| k.status == "active" && k.key_id == active_id)
            {
                return Err(VeilError::VersionConflict("轮换提交条件已变化".into()));
            }
            tx.execute("UPDATE file_info SET cipher_name=?1,key_id=?2,nonce=?3,encrypted_at=?4 WHERE id=?5",params![cipher,active_id,nonce.as_slice(),now(),identity])?;
            locks::release_in(&tx, &c)?;
            tx.commit()?;
            drop(guard);
            fault("after_rotation_file");
        }
        fault("before_database_rekey");
        self.change_password(password, new)?;
        let mut db = self.open_db(new)?;
        let tx = db
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut keys = container::keys(&tx)?;
        let active = keys
            .iter()
            .find(|k| k.status == "active")
            .unwrap()
            .key_id
            .clone();
        let remaining: i64 = tx.query_row(
            "SELECT count(*) FROM file_info WHERE kind='file' AND key_id<>?1",
            [&active],
            |r| r.get(0),
        )?;
        if remaining != 0 {
            return Err(invalid("旧文件密钥仍被引用"));
        }
        keys.retain(|k| k.status == "active");
        fault("before_retiring_key_removal");
        tx.execute(
            "UPDATE div_info SET data_keys=?1 WHERE id=1",
            [json(&keys)?],
        )?;
        tx.commit()?;
        drop(db);
        fault("after_retiring_key_removal");
        let _ = self.cleanup(new);
        Ok(())
    }
    /// 所有文件读锁与数据库快照保持到密文复制结束；源锁记录保持原样。
    pub fn snapshot_to(&self, destination: &Path, password: &str) -> Result<usize> {
        let execution = crate::ExecutionService::global()?;
        let _scope = execution.enter_task("导出一致快照");
        let db = self.open_db(password)?;
        let before = db.subtree(&db.root()?.id)?;
        let files = before
            .iter()
            .filter(|n| n.kind == "file")
            .map(|n| n.id.clone())
            .collect::<Vec<_>>();
        drop(db);
        let guard = self
            .lock_manager(password)
            .acquire(&files, LockMode::Read)?;
        let mut source = self.open_db(password)?;
        let reader = self.open_db(password)?;
        let tx = source
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if container::subtree(
            &tx,
            &before.iter().find(|n| n.parent_id.is_none()).unwrap().id,
        )? != before
        {
            return Err(VeilError::VersionConflict(
                "快照准备期间目录树已变化".into(),
            ));
        }
        // backup 从另一连接读取已确认快照；当前短事务仅保护初始化快照与数据库复制。
        crate::fsutil::create_dir_all_durable(destination)?;
        if fs::read_dir(destination)?.next().transpose()?.is_some() {
            return Err(invalid("快照目标必须为空目录"));
        }
        let path = destination.join(veil_core::database::CONTAINER_DB);
        if path.exists() {
            return Err(invalid("快照目标已存在"));
        }
        let mut target = veil_core::database::open(&path, Some(password), true)?;
        let backup = rusqlite::backup::Backup::new(&reader.conn, &mut target)?;
        backup.run_to_completion(128, Duration::from_millis(5), None)?;
        drop(backup);
        tx.commit()?;
        target.execute("DELETE FROM operation_locks", [])?;
        drop(target);
        // 数据库引用的文件受读租约保护；新增文件属于之后的数据库版本。
        for n in before.iter().filter(|n| n.kind == "file") {
            crate::fsutil::copy_file_new(
                &self.veil_dir.join(n.cipher()?),
                &destination.join(n.cipher()?),
            )?;
        }
        let db = self.open_db(password)?;
        for c in guard.credentials()? {
            locks::validate(&db.conn, &c, Some(LockMode::Read))?;
        }
        crate::fsutil::sync_directory(destination)?;
        Ok(files.len())
    }
}
use std::time::Duration;
fn verify_base(identity: &str, node: Option<&Node>, base: &VersionSnapshot) -> Result<()> {
    let Some(node) = node else {
        return Err(VeilError::VersionConflict("基础文件已删除".into()));
    };
    if base.veil_id != identity
        || base.file_id != node.id
        || compare_version_history(node.versions()?, &base.version_history)?
            != VersionRelation::Same
    {
        return Err(VeilError::VersionConflict(
            "文件身份或基础版本不匹配".into(),
        ));
    }
    Ok(())
}
fn split_parent(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}
pub fn normalize_veil_dir(path: &str) -> Result<String> {
    if path.is_empty() || path.starts_with(['/', '\\']) || path.contains(['\\', '\0']) {
        return Err(invalid("容器路径无效"));
    }
    let path = path.trim_end_matches('/');
    let mut parts = Vec::new();
    for part in path.split('/') {
        if part == "." {
            continue;
        }
        if part.is_empty() || part == ".." {
            return Err(invalid("容器路径包含无效节点名称"));
        }
        parts.push(part);
    }
    if parts.is_empty() {
        return Err(invalid("路径为空"));
    }
    Ok(parts.join("/"))
}
fn fault(stage: &str) {
    #[cfg(any(test, feature = "fault-injection"))]
    {
        veil_core::fsutil::fault_point(stage);
    }
    #[cfg(not(any(test, feature = "fault-injection")))]
    let _ = stage;
}
