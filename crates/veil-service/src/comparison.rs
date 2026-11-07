//! 对不同来源的文件内容、明文哈希及版本进行组合比较。
use crate::{
    error::{Result, VeilError},
    storage::{
        container::{ContainerDb, Node},
        invalid,
        locks::{self, LeaseGuard, LockMode},
    },
    veil_ops::{LockedDirectory, VeilManager},
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::{
    fs::{self, File, Metadata},
    io::{Cursor, Read},
    path::Path,
};
use veil_core::{
    file_ops::DecryptedFileReader,
    version::{VersionRelation, VersionSnapshot, compare_version_history},
};
use zeroize::Zeroizing;

/// 路径冲突由调用方判断；此处的容器路径仅用于定位已有密文。
pub enum FileInput<'a> {
    /// 调用方在事务内读取的容器文件元数据；缺失哈希时需通过容器输入补算。
    ContainerMetadata {
        version: &'a VersionSnapshot,
        hash: Option<&'a str>,
    },
    /// 调用方已解密的容器文件明文，版本快照可缺省。
    ContainerPlaintext {
        bytes: &'a [u8],
        version: Option<&'a VersionSnapshot>,
    },
    /// 容器中登记的加密文件；读取时流式解密并认证。
    ContainerCiphertext {
        manager: &'a VeilManager,
        password: &'a str,
        path: &'a str,
    },
    /// 复用调用方持有的目录租约，避免申请重叠的文件租约。
    ContainerCiphertextInDirectory {
        manager: &'a VeilManager,
        password: &'a str,
        path: &'a str,
        guard: &'a LeaseGuard,
        directory: &'a LockedDirectory,
    },
    /// 容器外的明文文件，版本快照可由调用方提供。
    External {
        path: &'a Path,
        version: Option<&'a VersionSnapshot>,
    },
}

/// 可单选或多选；未选择任何条件会返回参数错误。
#[derive(Debug, Clone, Copy, Default)]
pub struct ComparisonChecks {
    pub hash: bool,
    pub content: bool,
    pub version: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonState {
    Same,
    Different,
    /// 例如输入没有携带版本信息。
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictOutcome {
    /// 任一选中条件不同。
    Conflict,
    /// 全部选中条件相同。
    NoConflict,
    /// 没有发现差异，但至少一个选中条件不可比较。
    Indeterminate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComparisonReport {
    pub hash: Option<ComparisonState>,
    pub content: Option<ComparisonState>,
    pub version: Option<ComparisonState>,
    pub outcome: ConflictOutcome,
}

/// 判断容器内目标路径是否已被文件或目录占用；不比较文件内容。
/// 结果是当前快照，写入时仍须在事务内检查路径约束。
pub fn check_path_conflict(
    manager: &VeilManager,
    path: &str,
    password: &str,
    directory_guard: Option<(&LeaseGuard, &LockedDirectory)>,
) -> Result<bool> {
    let db = manager.open_db(password)?;
    if let Some((guard, directory)) = directory_guard {
        if !directory.covers(&crate::veil_ops::normalize_veil_dir(path)?) {
            return Err(invalid("操作超出目录锁范围"));
        }
        let credential = guard
            .credentials()?
            .into_iter()
            .find(|credential| credential.directory_id() == Some(directory.id()))
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
    }
    Ok(db.resolve(path)?.is_some())
}

/// 目标路径不存在则无冲突；目标为目录则冲突；目标为文件则按指定条件比较。
/// 所有选中条件相同才无冲突，条件不可比较时保留 Indeterminate。
/// 不决定跳过、覆盖或替换等业务策略。
pub fn check_file_conflict(
    manager: &VeilManager,
    path: &str,
    password: &str,
    source: FileInput<'_>,
    checks: ComparisonChecks,
    directory_guard: Option<(&LeaseGuard, &LockedDirectory)>,
) -> Result<ConflictOutcome> {
    validate_checks(checks)?;
    if !check_path_conflict(manager, path, password, directory_guard)? {
        return Ok(ConflictOutcome::NoConflict);
    }
    let db = manager.open_db(password)?;
    match db.resolve(path)? {
        None => return Ok(ConflictOutcome::NoConflict),
        Some(node) if node.kind != "file" => return Ok(ConflictOutcome::Conflict),
        Some(_) => {}
    }
    drop(db);
    let target = match directory_guard {
        Some((guard, directory)) => FileInput::ContainerCiphertextInDirectory {
            manager,
            password,
            path,
            guard,
            directory,
        },
        None => FileInput::ContainerCiphertext {
            manager,
            password,
            path,
        },
    };
    Ok(compare_files(target, source, checks)?.outcome)
}

fn validate_checks(checks: ComparisonChecks) -> Result<()> {
    if !checks.hash && !checks.content && !checks.version {
        return Err(invalid("请选择至少一个比较条件"));
    }
    Ok(())
}

struct LockedContainer<'a> {
    manager: &'a VeilManager,
    db: ContainerDb,
    node: Node,
    guard: ComparisonGuard<'a>,
    mode: LockMode,
    key: Zeroizing<[u8; 32]>,
}

enum ComparisonGuard<'a> {
    Owned(LeaseGuard),
    Directory(&'a LeaseGuard, &'a str),
}
impl ComparisonGuard<'_> {
    fn credentials(&self) -> Result<Vec<locks::Credential>> {
        match self {
            Self::Owned(guard) => guard.credentials(),
            Self::Directory(guard, id) => {
                let credential = guard
                    .credentials()?
                    .into_iter()
                    .find(|credential| credential.directory_id() == Some(*id))
                    .ok_or_else(|| invalid("目录锁凭证缺失"))?;
                Ok(vec![credential])
            }
        }
    }
}

enum Prepared<'a> {
    Metadata(&'a VersionSnapshot, Option<&'a str>),
    Plaintext {
        bytes: &'a [u8],
        version: Option<&'a VersionSnapshot>,
    },
    Ciphertext(LockedContainer<'a>),
    External {
        path: &'a Path,
        before: Metadata,
        version: Option<&'a VersionSnapshot>,
    },
}

impl<'a> Prepared<'a> {
    fn new(input: FileInput<'a>) -> Result<Self> {
        Ok(match input {
            FileInput::ContainerMetadata { version, hash } => Self::Metadata(version, hash),
            FileInput::ContainerPlaintext { bytes, version } => Self::Plaintext { bytes, version },
            FileInput::External { path, version } => {
                let before = fs::metadata(path)?;
                if !before.is_file() {
                    return Err(invalid("容器外输入必须是文件"));
                }
                Self::External {
                    path,
                    before,
                    version,
                }
            }
            FileInput::ContainerCiphertextInDirectory {
                manager,
                password,
                path,
                guard,
                directory,
            } => {
                if !directory.covers(&crate::veil_ops::normalize_veil_dir(path)?) {
                    return Err(invalid("操作超出目录锁范围"));
                }
                let db = manager.open_db(password)?;
                let node = db
                    .resolve(path)?
                    .filter(|node| node.kind == "file")
                    .ok_or_else(|| VeilError::FileNotFound(path.into()))?;
                let credential = guard
                    .credentials()?
                    .into_iter()
                    .find(|credential| credential.directory_id() == Some(directory.id()))
                    .ok_or_else(|| invalid("目录锁凭证缺失"))?;
                let mode = if credential.is_shared_write() {
                    LockMode::SharedWrite
                } else {
                    let mode: String = db.conn.query_row(
                        "SELECT mode FROM operation_locks WHERE lock_id=?1",
                        [&credential.lock_id],
                        |r| r.get(0),
                    )?;
                    if mode == "read" {
                        LockMode::Read
                    } else {
                        LockMode::Write
                    }
                };
                locks::validate(&db.conn, &credential, Some(mode))?;
                let key = db
                    .keys()?
                    .into_iter()
                    .find(|key| Some(&key.key_id) == node.key_id.as_ref())
                    .ok_or_else(|| invalid("文件引用未知密钥"))?
                    .material()?;
                Self::Ciphertext(LockedContainer {
                    manager,
                    db,
                    node,
                    guard: ComparisonGuard::Directory(guard, directory.id()),
                    mode,
                    key,
                })
            }
            FileInput::ContainerCiphertext {
                manager,
                password,
                path,
            } => {
                let (db, node, guard, key) = manager.read_locked(path, password, LockMode::Read)?;
                Self::Ciphertext(LockedContainer {
                    manager,
                    db,
                    node,
                    guard: ComparisonGuard::Owned(guard),
                    mode: LockMode::Read,
                    key,
                })
            }
        })
    }

    fn reader(&self) -> Result<Box<dyn Read + '_>> {
        Ok(match self {
            Self::Metadata(..) => return Err(invalid("所选比较需要文件明文，元数据输入无法提供")),
            Self::Plaintext { bytes, .. } => Box::new(Cursor::new(*bytes)),
            Self::External { path, .. } => Box::new(File::open(path)?),
            Self::Ciphertext(item) => Box::new(DecryptedFileReader::open(
                &item.manager.veil_dir.join(item.node.cipher()?),
                &item.key,
                &item.node.nonce()?,
                item.node.size.ok_or_else(|| invalid("文件大小缺失"))?,
            )?),
        })
    }

    fn verify(&self) -> Result<()> {
        match self {
            Self::External { path, before, .. } => {
                let after = fs::metadata(path)?;
                if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
                    return Err(invalid("比较期间容器外文件发生变化"));
                }
            }
            Self::Ciphertext(item) => {
                for credential in item.guard.credentials()? {
                    locks::validate(&item.db.conn, &credential, Some(item.mode))?;
                }
                if item.db.node(&item.node.id)?.as_ref() != Some(&item.node) {
                    return Err(VeilError::VersionConflict(
                        "比较期间容器文件发生变化".into(),
                    ));
                }
            }
            Self::Plaintext { .. } | Self::Metadata(..) => {}
        }
        Ok(())
    }

    fn version(&self) -> Result<Option<VersionSnapshot>> {
        Ok(match self {
            Self::Metadata(version, _) => Some((*version).clone()),
            Self::Plaintext { version, .. } | Self::External { version, .. } => {
                version.map(Clone::clone)
            }
            Self::Ciphertext(item) => Some(VersionSnapshot {
                veil_id: item.db.info()?.0,
                file_id: item.node.id.clone(),
                version_history: item.node.versions()?.to_vec(),
            }),
        })
    }

    fn hash(&mut self) -> Result<String> {
        if let Self::Metadata(_, Some(hash)) = self {
            return Ok((*hash).to_owned());
        }
        if let Self::Ciphertext(item) = self {
            if let Some(value) = item
                .db
                .conn
                .query_row(
                    "SELECT blake3 FROM file_hash WHERE file_id=?1",
                    [&item.node.id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            {
                return Ok(value);
            }
        }
        let digest = {
            let mut reader = self.reader()?;
            let mut hasher = blake3::Hasher::new();
            let mut buffer = Zeroizing::new(vec![0u8; 64 * 1024]);
            loop {
                veil_core::progress::check_cancelled()?;
                let read = checked_read(&mut *reader, &mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            hasher.finalize().to_hex().to_string()
        };
        self.verify()?;
        if let Self::Ciphertext(item) = self {
            // 在保护读取的租约仍有效时补登记；版本变化则拒绝写入旧哈希。
            let credentials = item.guard.credentials()?;
            let tx = item
                .db
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            for credential in credentials {
                locks::validate(&tx, &credential, Some(item.mode))?;
            }
            if crate::storage::container::node(&tx, &item.node.id)?.as_ref() != Some(&item.node) {
                return Err(VeilError::VersionConflict("登记哈希时文件发生变化".into()));
            }
            tx.execute("INSERT INTO file_hash(file_id,blake3) VALUES(?1,?2) ON CONFLICT(file_id) DO NOTHING", params![item.node.id, digest])?;
            tx.commit()?;
        }
        Ok(digest)
    }
}

fn read_block(reader: &mut dyn Read, buffer: &mut [u8]) -> Result<usize> {
    let mut used = 0;
    while used < buffer.len() {
        veil_core::progress::check_cancelled()?;
        let count = checked_read(reader, &mut buffer[used..])?;
        if count == 0 {
            break;
        }
        used += count;
    }
    Ok(used)
}

fn checked_read(reader: &mut dyn Read, buffer: &mut [u8]) -> Result<usize> {
    match reader.read(buffer) {
        Err(error)
            if error
                .get_ref()
                .and_then(|source| source.downcast_ref::<VeilError>())
                .is_some_and(|error| matches!(error, VeilError::Cancelled)) =>
        {
            Err(VeilError::Cancelled)
        }
        result => Ok(result?),
    }
}

fn content_equal(left: &Prepared<'_>, right: &Prepared<'_>) -> Result<bool> {
    let mut left_reader = left.reader()?;
    let mut right_reader = right.reader()?;
    let mut left_buffer = Zeroizing::new(vec![0u8; 64 * 1024]);
    let mut right_buffer = Zeroizing::new(vec![0u8; 64 * 1024]);
    let mut same = true;
    loop {
        let a = read_block(&mut *left_reader, &mut left_buffer)?;
        let b = read_block(&mut *right_reader, &mut right_buffer)?;
        if a != b || left_buffer[..a] != right_buffer[..b] {
            same = false;
        }
        if a == 0 && b == 0 {
            break;
        }
    }
    Ok(same)
}

/// 比较任意两类输入。路径冲突需由调用方处理；任一所选条件不同即为冲突。
pub fn compare_files(
    left: FileInput<'_>,
    right: FileInput<'_>,
    checks: ComparisonChecks,
) -> Result<ComparisonReport> {
    validate_checks(checks)?;
    let mut left = Prepared::new(left)?;
    let mut right = Prepared::new(right)?;
    compare_prepared(&mut left, &mut right, checks)
}

fn compare_prepared(
    left: &mut Prepared<'_>,
    right: &mut Prepared<'_>,
    checks: ComparisonChecks,
) -> Result<ComparisonReport> {
    let state = |same| {
        if same {
            ComparisonState::Same
        } else {
            ComparisonState::Different
        }
    };
    let hash = if checks.hash {
        Some(state(left.hash()? == right.hash()?))
    } else {
        None
    };
    let content = if checks.content {
        Some(state(content_equal(left, right)?))
    } else {
        None
    };
    let version = if checks.version {
        Some(match (left.version()?, right.version()?) {
            (Some(a), Some(b)) => {
                let relation = compare_version_history(&a.version_history, &b.version_history)?;
                state(
                    a.veil_id == b.veil_id
                        && a.file_id == b.file_id
                        && relation == VersionRelation::Same,
                )
            }
            _ => ComparisonState::Unavailable,
        })
    } else {
        None
    };
    left.verify()?;
    right.verify()?;
    let values = [hash, content, version];
    let outcome = if values.contains(&Some(ComparisonState::Different)) {
        ConflictOutcome::Conflict
    } else if values.contains(&Some(ComparisonState::Unavailable)) {
        ConflictOutcome::Indeterminate
    } else {
        ConflictOutcome::NoConflict
    };
    Ok(ComparisonReport {
        hash,
        content,
        version,
        outcome,
    })
}
