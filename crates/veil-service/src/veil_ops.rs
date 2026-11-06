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
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
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

pub(crate) struct ImportBatchContext {
    pub sender: mpsc::SyncSender<QueuedImport>,
    pub directory_guard: Option<Arc<LeaseGuard>>,
    pub owner: crate::execution::ExecutionOwner,
}

pub(crate) struct QueuedImport {
    pending: Vec<Pending>,
    cancelled_files: CancelledFiles,
    guard: Arc<LeaseGuard>,
    directory_id: Option<String>,
    veil_id: String,
    release_owned: bool,
}

impl QueuedImport {
    pub(crate) fn target(&self) -> &str {
        &self.pending[0].spec.target
    }
}

#[cfg(test)]
impl QueuedImport {
    pub(crate) fn invalidate_key(&mut self) -> String {
        self.pending[0].key.key_id = "invalid".into();
        self.pending[0].spec.target.clone()
    }
}

/// 路径和节点 ID 在取得租约后共同校验，供任务内工作线程复用。
pub(crate) struct LockedDirectory {
    path: String,
    id: String,
}
impl LockedDirectory {
    pub(crate) fn covers(&self, target: &str) -> bool {
        self.path.is_empty()
            || target
                .strip_prefix(&self.path)
                .is_some_and(|suffix| suffix.starts_with('/'))
    }
    pub(crate) fn path_len(&self) -> usize {
        self.path.len()
    }
}

/// 仅在提交前中止时清除本次操作创建的密文；提交结果不确定时交给常规恢复流程。
struct CancelledFiles {
    files: Vec<PathBuf>,
    staging: Vec<PathBuf>,
    committed: std::cell::Cell<bool>,
}

fn deepest_directory(conn: &rusqlite::Connection, path: &str) -> Result<Node> {
    let mut prefix = String::new();
    let mut current = container::resolve(conn, "")?.ok_or_else(|| invalid("根目录缺失"))?;
    for part in path.split('/').filter(|part| !part.is_empty()) {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(part);
        match container::resolve(conn, &prefix)? {
            Some(node) if node.kind == "directory" => current = node,
            Some(_) => return Err(invalid("父路径是文件")),
            None => break,
        }
    }
    Ok(current)
}
impl Drop for CancelledFiles {
    fn drop(&mut self) {
        if self.committed.get() {
            return;
        }
        for path in &self.files {
            let _ = fs::remove_file(path);
        }
        for path in &self.staging {
            let _ = fs::remove_dir(path);
        }
    }
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
        let ancestor = deepest_directory(&tx, &path)?;
        locks::ensure_parent_available(&tx, &ancestor.id)?;
        let node = container::ensure_directory(&tx, &path)?;
        tx.commit()?;
        Ok(node.id)
    }
    pub(crate) fn lock_import_directories(
        &self,
        paths: &[String],
        password: &str,
        cancellation: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(LeaseGuard, Vec<LockedDirectory>)> {
        let mut directories = Vec::new();
        for path in paths {
            veil_core::progress::check_cancelled()?;
            let db = self.open_db(password)?;
            let identity = match db.resolve(path)? {
                Some(node) if node.kind == "directory" => node.id,
                Some(_) => return Err(invalid("导入目标不是目录")),
                None if !path.is_empty() => {
                    drop(db);
                    self.create_directory(path, password)?
                }
                None => return Err(invalid("根目录缺失")),
            };
            directories.push(LockedDirectory {
                path: path.clone(),
                id: identity,
            });
        }
        let mut options = self.lock_options.clone();
        options.cancellation = Some(cancellation);
        let guard = self
            .lock_manager(password)
            .with_options(options)
            .acquire_directories(
                &directories
                    .iter()
                    .map(|directory| directory.id.clone())
                    .collect::<Vec<_>>(),
                LockMode::SharedWrite,
            )?;
        let db = self.open_db(password)?;
        for directory in &directories {
            if db
                .resolve(&directory.path)?
                .is_none_or(|node| node.id != directory.id || node.kind != "directory")
            {
                return Err(VeilError::VersionConflict(
                    "申请目录锁期间目标目录发生变化".into(),
                ));
            }
        }
        Ok((guard, directories))
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
        self.add_files_inner(files, password, true, None, None, None, None)
    }
    /// 使用单文件 SW 租约替换该文件。
    pub fn add_file_with_shared_lock(
        &self,
        file: &AddFileSpec,
        password: &str,
        guard: &LeaseGuard,
    ) -> Result<String> {
        Ok(self
            .add_files_inner(
                std::slice::from_ref(file),
                password,
                true,
                None,
                None,
                Some(guard),
                None,
            )?
            .remove(0))
    }
    /// 使用已取得的目录 SW 租约写入该目录内的文件。
    pub fn add_files_with_shared_directory_lock(
        &self,
        files: &[AddFileSpec],
        password: &str,
        directory: &str,
        guard: &LeaseGuard,
    ) -> Result<Vec<String>> {
        let path = if directory.is_empty() {
            String::new()
        } else {
            normalize_veil_dir(directory)?
        };
        let db = self.open_db(password)?;
        let node = db
            .resolve(&path)?
            .filter(|node| node.kind == "directory")
            .ok_or_else(|| invalid("目录不存在"))?;
        let credential = guard
            .credentials()?
            .into_iter()
            .find(|credential| {
                credential.directory_id() == Some(node.id.as_str()) && credential.is_shared_write()
            })
            .ok_or_else(|| invalid("缺少共享写锁凭证"))?;
        locks::validate(&db.conn, &credential, Some(LockMode::SharedWrite))?;
        drop(db);
        self.add_files_inner(
            files,
            password,
            true,
            Some((guard, &LockedDirectory { path, id: node.id })),
            None,
            None,
            None,
        )
    }
    #[cfg(test)]
    pub(crate) fn add_files_without_cleanup(
        &self,
        files: &[AddFileSpec],
        password: &str,
    ) -> Result<Vec<String>> {
        self.add_files_inner(files, password, false, None, None, None, None)
    }
    #[cfg(test)]
    pub(crate) fn add_files_in_directory(
        &self,
        files: &[AddFileSpec],
        password: &str,
        guard: &LeaseGuard,
        directory: &LockedDirectory,
    ) -> Result<Vec<String>> {
        self.add_files_inner(
            files,
            password,
            false,
            Some((guard, directory)),
            None,
            None,
            None,
        )
    }
    pub(crate) fn add_files_for_import(
        &self,
        files: &[AddFileSpec],
        password: &str,
        directory_guard: Option<(&LeaseGuard, &LockedDirectory)>,
        transaction_gate: &std::sync::Mutex<()>,
        batch: &ImportBatchContext,
    ) -> Result<Vec<String>> {
        self.add_files_inner(
            files,
            password,
            false,
            directory_guard,
            Some(transaction_gate),
            None,
            Some(batch),
        )
    }
    fn add_files_inner(
        &self,
        files: &[AddFileSpec],
        password: &str,
        cleanup: bool,
        directory_guard: Option<(&LeaseGuard, &LockedDirectory)>,
        transaction_gate: Option<&std::sync::Mutex<()>>,
        file_guard: Option<&LeaseGuard>,
        batch: Option<&ImportBatchContext>,
    ) -> Result<Vec<String>> {
        let execution = crate::ExecutionService::global()?;
        let _scope = execution.enter_task("写入文件");
        // 续租会持有凭证互斥锁并访问数据库；必须在开始数据库写事务前取出快照。
        let directory_credential = directory_guard
            .map(|(guard, directory)| {
                guard
                    .credentials()
                    .map(|credentials| (credentials, directory))
            })
            .transpose()?
            .map(|(credentials, directory)| {
                credentials
                    .into_iter()
                    .find(|credential| credential.directory_id() == Some(directory.id.as_str()))
                    .ok_or_else(|| invalid("目录锁凭证缺失"))
            })
            .transpose()?;
        let mut db = self.open_db(password)?;
        let veil_id = db.info()?.0;
        if let Some(guard) = file_guard {
            if files.len() != 1 {
                return Err(invalid("文件共享写锁只支持单个文件"));
            }
            let target = normalize_veil_dir(&files[0].target)?;
            let node = db
                .resolve(&target)?
                .filter(|node| node.kind == "file")
                .ok_or_else(|| invalid("文件不存在"))?;
            let credential = guard
                .credentials()?
                .into_iter()
                .find(|c| c.file_id() == Some(node.id.as_str()) && c.is_shared_write())
                .ok_or_else(|| invalid("缺少目标文件的共享写锁凭证"))?;
            locks::validate(
                &db.conn,
                &credential,
                Some(if credential.is_shared_write() {
                    LockMode::SharedWrite
                } else {
                    LockMode::Write
                }),
            )?;
        }
        let prepare_gate = transaction_gate
            .map(|gate| gate.lock().map_err(|_| invalid("导入事务状态不可用")))
            .transpose()?;
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
            if directory_guard.is_some_and(|(_, directory)| !directory.covers(&target)) {
                return Err(invalid("操作超出目录锁范围"));
            }
            if !targets.insert(target.clone()) {
                return Err(invalid("批量目标重复"));
            }
            if !spec.source.is_file() {
                return Err(invalid("源路径必须是文件"));
            }
            let (parent_path, name) = split_parent(&target);
            if let Some(credential) = &directory_credential {
                locks::validate(
                    &tx,
                    credential,
                    Some(if credential.is_shared_write() {
                        LockMode::SharedWrite
                    } else {
                        LockMode::Write
                    }),
                )?;
            } else {
                let ancestor = deepest_directory(&tx, parent_path)?;
                locks::ensure_parent_available(&tx, &ancestor.id)?;
            }
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
        drop(prepare_gate);
        drop(db);
        let own_guard = if directory_guard.is_none() && file_guard.is_none() {
            let resources = pending
                .iter()
                .map(|p| locks::pending_resource(&p.identity, &p.parent))
                .collect::<Vec<_>>();
            let manager = self.lock_manager(password);
            let guard = if let Some(context) = batch {
                manager.acquire_for(
                    &resources,
                    LockMode::SharedWrite,
                    context.owner.clone(),
                    None,
                )?
            } else {
                manager.acquire(&resources, LockMode::SharedWrite)?
            };
            Some(Arc::new(guard))
        } else {
            None
        };
        let guard = directory_guard
            .map(|(guard, _)| guard)
            .or(file_guard)
            .or(own_guard.as_deref())
            .ok_or_else(|| invalid("锁凭证缺失"))?;
        let credentials = guard.credentials()?;
        if file_guard.is_some()
            && (pending.len() != 1
                || pending[0].base.is_none()
                || !credentials.iter().any(|c| {
                    c.file_id() == Some(pending[0].identity.as_str()) && c.is_shared_write()
                }))
        {
            return Err(invalid("缺少目标文件的共享写锁凭证"));
        }
        let mut cancelled_files = CancelledFiles {
            files: Vec::new(),
            staging: Vec::new(),
            committed: std::cell::Cell::new(false),
        };
        // 申请锁可能等待其他写入完成；先复核快照，避免访问已经失去引用的旧密文。
        if pending.iter().any(|p| p.base.is_some()) {
            let current = self.open_db(password)?;
            for p in &pending {
                if let Some(base) = &p.base {
                    if current.node(&p.identity)?.as_ref() != Some(base) {
                        return Err(VeilError::VersionConflict("文件在申请锁期间已变化".into()));
                    }
                }
            }
        }
        for p in &mut pending {
            let credential = if let Some((_, directory)) = directory_guard {
                credentials
                    .iter()
                    .find(|credential| credential.directory_id() == Some(directory.id.as_str()))
            } else {
                credentials
                    .iter()
                    .find(|c| c.file_id() == Some(p.identity.as_str()))
            }
            .ok_or_else(|| invalid("锁凭证缺失"))?;
            let lease_staging = self.veil_dir.join(".staging").join(&credential.lock_id);
            // 同一目录租约由多个工作线程共享；各文件使用自己的子目录。
            let staging = if directory_guard.is_some() || file_guard.is_some() {
                lease_staging.join(id())
            } else {
                lease_staging
            };
            crate::fsutil::create_dir_all_durable(&staging)?;
            cancelled_files.staging.push(staging.clone());
            p.cipher = format!("{}_{}.enc", credential.lock_id, id());
            let staged = staging.join(&p.cipher);
            cancelled_files.files.push(staged.clone());
            fault("before_cipher_write");
            p.size = file_ops::encrypt_staged_file(
                &p.spec.source,
                &staged,
                &*p.key.material()?,
                &p.nonce,
            )?;
            fault("after_cipher_write");
            let published = self.veil_dir.join(&p.cipher);
            crate::fsutil::publish_staged_file(&staged, &published).map_err(|error| {
                if veil_core::progress::is_cancelled_io(&error) {
                    VeilError::Cancelled
                } else {
                    VeilError::Io(error)
                }
            })?;
            cancelled_files.files.push(published);
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
        veil_core::progress::check_cancelled()?;
        if let Some(batch) = batch {
            let batch_guard = match &own_guard {
                Some(guard) => Arc::clone(guard),
                None => batch
                    .directory_guard
                    .as_ref()
                    .cloned()
                    .ok_or_else(|| invalid("导入锁凭证缺失"))?,
            };
            let ciphers = pending.iter().map(|file| file.cipher.clone()).collect();
            let queued = QueuedImport {
                pending,
                guard: batch_guard,
                directory_id: directory_guard.map(|(_, directory)| directory.id.clone()),
                veil_id,
                release_owned: own_guard.is_some(),
                cancelled_files,
            };
            veil_core::progress::file_queued();
            if batch.sender.send(queued).is_err() {
                veil_core::progress::file_dequeued();
                return Err(invalid("导入提交队列已关闭"));
            }
            return Ok(ciphers);
        }
        // 重新打开可识别其他进程改密；旧密码连接不能提交较早启动的写入。
        let credentials = guard.credentials()?;
        let mut db = self.open_db(password)?;
        if db.info()?.0 != veil_id {
            return Err(VeilError::VeilIdConflict("写入期间容器身份已变化".into()));
        }
        let commit_gate = transaction_gate
            .map(|gate| gate.lock().map_err(|_| invalid("导入事务状态不可用")))
            .transpose()?;
        let commit = veil_core::database::commit_batch(
            &mut db.conn,
            |tx| {
                Self::apply_pending(
                    tx,
                    &pending,
                    &credentials,
                    directory_guard.map(|(_, directory)| directory.id.as_str()),
                    own_guard.is_some(),
                    &veil_id,
                )?;
                veil_core::progress::check_cancelled()?;
                fault("before_metadata_commit");
                Ok(())
            },
            || {
                cancelled_files.committed.set(true);
            },
        );
        drop(commit_gate);
        drop(db);
        #[cfg(feature = "fault-injection")]
        let injected_uncertainty = std::env::var_os("VEIL_TEST_COMMIT_UNCERTAIN").is_some();
        #[cfg(not(feature = "fault-injection"))]
        let injected_uncertainty = false;
        if let Err(error) = &commit {
            if !matches!(error, VeilError::CommitUncertain(_)) {
                return commit.map(|_| Vec::new());
            }
        }
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
        // 本次写入的临时目录通常已空；仅尝试删除空目录，不碰其他任务的文件。
        for directory in &cancelled_files.staging {
            let _ = fs::remove_dir(directory);
        }
        drop(own_guard);
        if cleanup {
            let _ = self.cleanup(password);
        }
        Ok(pending.into_iter().map(|p| p.cipher).collect())
    }
    pub(crate) fn commit_import_batch(
        &self,
        batch: Vec<QueuedImport>,
        password: &str,
        stopped: &AtomicBool,
    ) -> Result<()> {
        let result = (|| -> Result<()> {
            veil_core::progress::check_cancelled()?;
            if stopped.load(Ordering::Acquire) {
                return Err(VeilError::Cancelled);
            }
            let credentials = batch
                .iter()
                .map(|item| {
                    item.guard
                        .credentials()
                        .map_err(|error| VeilError::ImportBatchFailed {
                            file: item.target().to_owned(),
                            message: error.to_string(),
                        })
                })
                .collect::<Result<Vec<_>>>()?;
            let mut db = self.open_db(password)?;
            let identity = db.info()?.0;
            if batch.iter().any(|item| item.veil_id != identity) {
                return Err(VeilError::VeilIdConflict("导入期间容器身份已变化".into()));
            }
            let commit = veil_core::database::commit_batch(
                &mut db.conn,
                |tx| {
                    for (item, credentials) in batch.iter().zip(&credentials) {
                        Self::apply_pending(
                            tx,
                            &item.pending,
                            credentials,
                            item.directory_id.as_deref(),
                            item.release_owned,
                            &identity,
                        )
                        .map_err(|error| VeilError::ImportBatchFailed {
                            file: item.pending[0].spec.target.clone(),
                            message: error.to_string(),
                        })?;
                    }
                    veil_core::progress::check_cancelled()?;
                    if stopped.load(Ordering::Acquire) {
                        return Err(VeilError::Cancelled);
                    }
                    fault("before_metadata_commit");
                    Ok(())
                },
                || {
                    for item in &batch {
                        item.cancelled_files.committed.set(true);
                    }
                },
            );
            drop(db);
            #[cfg(feature = "fault-injection")]
            let commit =
                if commit.is_ok() && std::env::var_os("VEIL_TEST_COMMIT_UNCERTAIN").is_some() {
                    Err(VeilError::CommitUncertain(std::io::Error::other(
                        "模拟提交结果不确定",
                    )))
                } else {
                    commit
                };
            match commit {
                Err(VeilError::CommitUncertain(error)) => {
                    let confirmed = self.open_db(password).and_then(|db| {
                        for item in &batch {
                            for file in &item.pending {
                                if db
                                    .node(&file.identity)?
                                    .and_then(|node| node.cipher_name)
                                    .as_deref()
                                    != Some(file.cipher.as_str())
                                {
                                    return Ok(false);
                                }
                            }
                        }
                        Ok(true)
                    });
                    if !matches!(confirmed, Ok(true)) {
                        return Err(VeilError::CommitUncertain(error));
                    }
                }
                other => other?,
            }
            fault("after_metadata_commit");
            Ok(())
        })();
        for item in &batch {
            veil_core::progress::file_dequeued();
            if result.is_ok() {
                for directory in &item.cancelled_files.staging {
                    let _ = fs::remove_dir(directory);
                }
                veil_core::progress::file_done(false);
            }
        }
        result
    }

    pub(crate) fn cancel_import_batch(batch: Vec<QueuedImport>) {
        for _ in &batch {
            veil_core::progress::file_dequeued();
        }
        drop(batch);
    }

    fn apply_pending(
        tx: &rusqlite::Transaction<'_>,
        pending: &[Pending],
        credentials: &[locks::Credential],
        directory_id: Option<&str>,
        release_owned: bool,
        veil_id: &str,
    ) -> Result<()> {
        let active = container::keys(&tx)?
            .into_iter()
            .find(|k| k.status == "active")
            .unwrap();
        let mut obsolete = Vec::new();
        for p in pending {
            let c = if let Some(directory) = directory_id {
                credentials
                    .iter()
                    .find(|credential| credential.directory_id() == Some(directory))
            } else {
                credentials
                    .iter()
                    .find(|c| c.file_id() == Some(p.identity.as_str()))
            }
            .ok_or_else(|| invalid("锁凭证缺失"))?;
            locks::validate(
                &tx,
                c,
                Some(if c.is_shared_write() {
                    LockMode::SharedWrite
                } else {
                    LockMode::Write
                }),
            )?;
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
                    veil_id,
                    Some(current),
                    &VersionSnapshot {
                        veil_id: veil_id.to_owned(),
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
            if release_owned {
                locks::release_in(&tx, c)?;
            }
        }
        Ok(())
    }
    fn read_locked(
        &self,
        name: &str,
        password: &str,
        mode: LockMode,
    ) -> Result<(ContainerDb, Node, LeaseGuard, Zeroizing<[u8; 32]>)> {
        let db = self.open_db(password)?;
        let identity = db.info()?.0;
        let node = db
            .resolve(name)?
            .filter(|n| n.kind == "file")
            .ok_or_else(|| VeilError::FileNotFound(name.into()))?;
        let guard = self
            .lock_manager(password)
            .acquire(std::slice::from_ref(&node.id), mode)?;
        drop(db);
        let db = self.open_db(password)?;
        if db.info()?.0 != identity {
            return Err(VeilError::VeilIdConflict("读取期间容器身份已变化".into()));
        }
        for credential in guard.credentials()? {
            locks::validate(&db.conn, &credential, Some(mode))?;
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

    /// 对已存在的目标逐块核对明文；只有内容相同时才能作为已完成文件跳过。
    pub fn matches_existing_file(&self, name: &str, source: &Path, password: &str) -> Result<bool> {
        self.matches_existing_file_inner(name, source, password, None)
    }
    pub(crate) fn matches_existing_file_in_directory(
        &self,
        name: &str,
        source: &Path,
        password: &str,
        guard: &LeaseGuard,
        directory: &LockedDirectory,
    ) -> Result<bool> {
        self.matches_existing_file_inner(name, source, password, Some((guard, directory)))
    }
    fn matches_existing_file_inner(
        &self,
        name: &str,
        source: &Path,
        password: &str,
        directory_guard: Option<(&LeaseGuard, &LockedDirectory)>,
    ) -> Result<bool> {
        use std::io::{Read, Write};
        struct Compare<R> {
            source: R,
            matches: bool,
        }
        impl<R: Read> Write for Compare<R> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                let mut input = vec![0; bytes.len()];
                self.source.read_exact(&mut input)?;
                self.matches &= input == bytes;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        if let Some((_, directory)) = directory_guard {
            if !directory.covers(&normalize_veil_dir(name)?) {
                return Err(invalid("操作超出目录锁范围"));
            }
        }
        let input = fs::File::open(source)?;
        let before = input.metadata()?;
        let (db, node, owned_guard, key) = if let Some((guard, directory)) = directory_guard {
            let db = self.open_db(password)?;
            let node = db
                .resolve(name)?
                .filter(|node| node.kind == "file")
                .ok_or_else(|| VeilError::FileNotFound(name.into()))?;
            let credential = guard
                .credentials()?
                .into_iter()
                .find(|credential| credential.directory_id() == Some(directory.id.as_str()))
                .ok_or_else(|| invalid("目录锁凭证缺失"))?;
            locks::validate(
                &db.conn,
                &credential,
                Some(if credential.is_shared_write() {
                    LockMode::SharedWrite
                } else {
                    LockMode::Write
                }),
            )?;
            let key = db
                .keys()?
                .into_iter()
                .find(|key| Some(&key.key_id) == node.key_id.as_ref())
                .ok_or_else(|| invalid("文件引用未知密钥"))?
                .material()?;
            (db, node, None, key)
        } else {
            let (db, node, guard, key) = self.read_locked(name, password, LockMode::Read)?;
            if guard
                .credentials()?
                .iter()
                .any(|credential| credential.is_read_only())
            {
                return Err(invalid("文件正在写入，无法安全核对已导入内容"));
            }
            (db, node, Some(guard), key)
        };
        if Some(before.len()) != node.size {
            return Ok(false);
        }
        let mut comparison = Compare {
            source: input,
            matches: true,
        };
        file_ops::decrypt_file_to_writer(
            &self.veil_dir.join(node.cipher()?),
            &mut comparison,
            &key,
            &node.nonce()?,
            before.len(),
        )?;
        let after = comparison.source.metadata()?;
        if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
            return Err(invalid("核对期间源文件发生变化"));
        }
        if let Some((guard, directory)) = directory_guard {
            let credential = guard
                .credentials()?
                .into_iter()
                .find(|credential| credential.directory_id() == Some(directory.id.as_str()))
                .ok_or_else(|| invalid("目录锁凭证缺失"))?;
            locks::validate(
                &db.conn,
                &credential,
                Some(if credential.is_shared_write() {
                    LockMode::SharedWrite
                } else {
                    LockMode::Write
                }),
            )?;
        } else if let Some(guard) = owned_guard {
            for credential in guard.credentials()? {
                locks::validate(&db.conn, &credential, Some(LockMode::Read))?;
            }
        }
        Ok(comparison.matches)
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
            .map(|(version, _)| version)
    }
    fn extract_file_inner(
        &self,
        name: &str,
        output: &Path,
        password: &str,
        overwrite: bool,
    ) -> Result<(VersionSnapshot, bool)> {
        let execution = crate::ExecutionService::global()?;
        let _scope = execution.enter_task("读取文件");
        let (db, node, guard, key) = self.read_locked(name, password, LockMode::SharedWrite)?;
        let unstable = guard
            .credentials()?
            .iter()
            .any(|credential| credential.is_read_only());
        veil_core::progress::start(node.size);
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
        veil_core::progress::finalizing();
        fault("before_plaintext_publish");
        let credentials = guard.credentials()?;
        let mut fresh = self.open_db(password)?;
        let tx = fresh.conn.transaction()?;
        for credential in credentials {
            locks::validate(&tx, &credential, Some(LockMode::SharedWrite))?;
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
        Ok((version, unstable))
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
    pub fn extract_file_without_overwrite_with_status(
        &self,
        name: &str,
        output: &Path,
        password: &str,
    ) -> Result<bool> {
        self.extract_file_inner(name, output, password, false)
            .map(|(_, unstable)| unstable)
    }
    pub fn read_file_bytes(&self, name: &str, password: &str, limit: u64) -> Result<Vec<u8>> {
        self.read_file_bytes_with_status(name, password, limit)
            .map(|(bytes, _)| bytes)
    }
    pub fn read_file_bytes_with_status(
        &self,
        name: &str,
        password: &str,
        limit: u64,
    ) -> Result<(Vec<u8>, bool)> {
        let execution = crate::ExecutionService::global()?;
        let _scope = execution.enter_task("预览文件");
        let (db, node, guard, key) = self.read_locked(name, password, LockMode::ReadOnly)?;
        let unstable = guard
            .credentials()?
            .iter()
            .any(|credential| credential.is_read_only());
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
            locks::validate(&db.conn, &c, Some(LockMode::ReadOnly))?;
        }
        Ok((bytes, unstable))
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
        drop(db);
        let guard = if node.kind == "directory" {
            self.lock_manager(password)
                .acquire_directory(&node.id, LockMode::Write)?
        } else {
            self.lock_manager(password)
                .acquire(std::slice::from_ref(&node.id), LockMode::Write)?
        };
        let credentials = guard.credentials()?;
        let mut db = self.open_db(password)?;
        let tx = db
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if container::subtree(&tx, &node.id)? != before {
            return Err(VeilError::VersionConflict(
                "删除前子树已变化，请重试".into(),
            ));
        }
        for c in credentials {
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
        let source_resource = if n.kind == "directory" {
            locks::directory_resource(&n.id)
        } else {
            n.id.clone()
        };
        locks::ensure_available(&tx, &source_resource, LockMode::Write)?;
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
        locks::ensure_parent_available(&tx, &parent.id)?;
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
        // 初始快照中仍被引用的密文可以直接保留；未引用候选仍须在写事务内复核。
        let referenced: HashSet<String> = {
            let mut query = db
                .conn
                .prepare("SELECT cipher_name FROM file_info WHERE cipher_name IS NOT NULL")?;
            query
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        {
            let tx = db
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            locks::reap(&tx, &dead)?;
            tx.commit()?;
        }
        let mut removed = 0;
        for entry in fs::read_dir(&self.veil_dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !entry.file_type()?.is_file() || container::validate_cipher(&name).is_err() {
                continue;
            }
            if referenced.contains(&name) {
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
        let guard = self.lock_manager(old).acquire_container(LockMode::Write)?;
        let credential = guard
            .credentials()?
            .into_iter()
            .next()
            .ok_or_else(|| invalid("目录锁凭证缺失"))?;
        let db = self.open_db(old)?;
        locks::validate(&db.conn, &credential, Some(LockMode::Write))?;
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
        // 改密已确认成功；释放失败时租约会过期，不能误报密码仍未修改。
        let _ = guard.release_after_rekey(new);
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
        let root = container::resolve(&tx, "")?.ok_or_else(|| invalid("根目录缺失"))?;
        locks::ensure_available(&tx, &locks::directory_resource(&root.id), LockMode::Write)?;
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
            file_ops::rewrite_staged_file(
                &*previous.material()?,
                &*active.material()?,
                &self.veil_dir.join(n.cipher()?),
                &staging.join(&cipher),
                &n.nonce()?,
                &nonce,
                n.size.unwrap(),
            )?;
            crate::fsutil::publish_staged_file(
                &staging.join(&cipher),
                &self.veil_dir.join(&cipher),
            )?;
            crate::fsutil::sync_directory(&self.veil_dir)?;
            let active_id = active.key_id.clone();
            drop(db);
            let c = guard.credentials()?.remove(0);
            let mut db = self.open_db(password)?;
            let tx = db
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
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
        let root_id = &before
            .iter()
            .find(|node| node.parent_id.is_none())
            .ok_or_else(|| invalid("根目录缺失"))?
            .id;
        let guard = self
            .lock_manager(password)
            .acquire_directory(root_id, LockMode::Read)?;
        if guard
            .credentials()?
            .iter()
            .any(|credential| credential.is_read_only())
        {
            return Err(invalid("容器正在写入，无法创建一致的备份"));
        }
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
        let names: Vec<String> = before
            .iter()
            .filter(|n| n.kind == "file")
            .map(|node| node.cipher().map(str::to_owned))
            .collect::<Result<_>>()?;
        let total = names.iter().try_fold(0u64, |total, name| {
            total.checked_add(fs::metadata(self.veil_dir.join(name)).ok()?.len())
        });
        veil_core::progress::stage("snapshot", total);
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let count = veil_core::progress::worker_count(names.len());
        veil_core::progress::set_parallel_workers(count);
        let next = AtomicUsize::new(0);
        let stopped = AtomicBool::new(false);
        let failure = std::sync::Mutex::new(None);
        let progress = veil_core::progress::current();
        std::thread::scope(|scope| {
            for _ in 0..count {
                let names = &names;
                let next = &next;
                let stopped = &stopped;
                let failure = &failure;
                let progress = &progress;
                scope.spawn(move || {
                    let _attachment = progress.as_ref().map(|progress| progress.attach());
                    loop {
                        if stopped.load(Ordering::Acquire) {
                            break;
                        }
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(name) = names.get(index) else { break };
                        if let Err(error) = crate::fsutil::copy_file_new_with_progress(
                            &self.veil_dir.join(name),
                            &destination.join(name),
                        ) {
                            stopped.store(true, Ordering::Release);
                            let mut failure = failure.lock().unwrap_or_else(|e| e.into_inner());
                            if failure.is_none() {
                                *failure = Some(error);
                            }
                            break;
                        }
                    }
                });
            }
        });
        if let Some(error) = failure.into_inner().unwrap_or_else(|e| e.into_inner()) {
            return Err(error.into());
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

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    #[test]
    fn cancellation_cleans_only_uncommitted_files() {
        let root = tempfile::TempDir::new().unwrap();
        let staged_dir = root.path().join("staging");
        fs::create_dir(&staged_dir).unwrap();
        let uncommitted = root.path().join("uncommitted.enc");
        let committed = root.path().join("committed.enc");
        fs::write(&uncommitted, b"unfinished").unwrap();
        fs::write(&committed, b"finished").unwrap();
        let progress = veil_core::progress::Progress::default();
        let _scope = progress.begin("import");
        veil_core::progress::start(Some(1));
        assert!(progress.request_cancel());
        drop(CancelledFiles {
            files: vec![uncommitted.clone()],
            staging: vec![staged_dir.clone()],
            committed: std::cell::Cell::new(false),
        });
        drop(CancelledFiles {
            files: vec![committed.clone()],
            staging: vec![],
            committed: std::cell::Cell::new(true),
        });
        assert!(!uncommitted.exists());
        assert!(!staged_dir.exists());
        assert_eq!(fs::read(committed).unwrap(), b"finished");
    }
}
