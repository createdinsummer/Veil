//! 对不同来源的文件内容、明文哈希及版本进行组合比较。
//!
//! 所属业务模块：veil-service 的对比（comparison）模块。导入/导出/同步场景需要
//! 判断「目标路径是否被占用、占用者与候选文件是否相同」，本模块把比较工作统一成
//! 四类来源（容器元数据、容器明文、容器密文、容器外文件）与三类条件（哈希、内容、
//! 版本链），输出结构化差异报告；调用方（导入计划、导出流水线）再决定跳过/覆盖。
//!
//! 文件内核心清单：
//! - `FileInput`：四类来源、五个变体（密文另提供「复用目录租约」变体）；
//! - `ComparisonChecks` / `ComparisonState` / `ConflictOutcome` / `ComparisonReport`：
//!   比较条件、单条件结论、总体结论与报告；
//! - `check_path_conflict` / `check_file_conflict` / `compare_files`：对外入口；
//! - `Prepared`：把输入标准化成可读可哈希可验版本的内部表示；
//! - `LockedContainer`：持租约读取容器密文文件的状态集合（比较期间持续验锁）。
//!
//! 依赖能力：storage 的容器库访问与租约校验（locks）、veil-core 的流式解密
//! （DecryptedFileReader）与版本链比较（compare_version_history）。
//!
//! 整体业务目标：用统一的规则回答「这两个东西是不是同一个内容」：
//! 全部选中条件都相同才判 NoConflict；任一条件不同即 Conflict；
//! 有条件无法比较（如缺版本信息）时保留 Indeterminate，交给调用方谨慎处理。
use crate::{
    error::{Result, VeilError},
    storage::{
        container::{self, ContainerDb, Node},
        invalid,
        locks::{self, LeaseGuard, LockMode},
    },
    veil_ops::{LockedDirectory, VeilManager},
};
use rusqlite::{OptionalExtension, TransactionBehavior};
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
///
/// 比较输入的统一抽象：无论比较对象来自哪里，都先包装成 FileInput，再进入
/// Prepared 标准化。容器内密文是「权威来源」（数据库记录 + 磁盘密文），
/// 外部文件边读边复查长度与修改时间，这只是变化检测，不是操作系统快照。
pub enum FileInput<'a> {
    /// 调用方在事务内读取的容器文件元数据；没有已登记哈希时应改用密文输入。
    ContainerMetadata {
        /// 容器文件的版本快照（容器 ID、文件 ID、版本链）。
        version: &'a VersionSnapshot,
        /// 已登记的 BLAKE3 明文哈希（file_hash 表）；没有时改用密文输入现场计算。
        hash: Option<&'a str>,
    },
    /// 调用方已解密的容器文件明文，版本快照可缺省。
    ContainerPlaintext {
        /// 明文内容（借用，比较期间必须保持有效）。
        bytes: &'a [u8],
        /// 可选版本快照；缺省时版本条件会判 Unavailable。
        version: Option<&'a VersionSnapshot>,
    },
    /// 容器中登记的加密文件；读取时流式解密并认证。
    ContainerCiphertext {
        /// 容器管理器，用于开库、定位密文与申请读租约。
        manager: &'a VeilManager,
        /// 解锁密码；请求借用字符串，底层租约或脱敏能力可能保存受保护副本。
        password: &'a str,
        /// 容器内路径，用于定位文件节点。
        path: &'a str,
    },
    /// 复用调用方持有的目录租约，避免申请重叠的文件租约。
    ContainerCiphertextInDirectory {
        /// 容器管理器（与上同）。
        manager: &'a VeilManager,
        /// 容器密码。
        password: &'a str,
        /// 容器内路径。
        path: &'a str,
        /// 调用方已持有的目录租约守卫（导入目录锁等）。
        guard: &'a LeaseGuard,
        /// 与该租约对应的已锁定目录（含其覆盖范围）。
        directory: &'a LockedDirectory,
    },
    /// 容器外的明文文件，版本快照可由调用方提供。
    External {
        /// 容器外文件路径。
        path: &'a Path,
        /// 可选版本快照（导入计划确认覆盖时绑定）。
        version: Option<&'a VersionSnapshot>,
    },
}

/// 可单选或多选；未选择任何条件会返回参数错误。
///
/// 业务含义：决定本次比较考察哪些维度——hash 比较明文哈希（省流量省计算）、
/// content 逐字节比较明文、version 比较版本链关系（同一文件身份与继承链）。
#[derive(Debug, Clone, Copy, Default)]
pub struct ComparisonChecks {
    /// 是否比较 BLAKE3 明文哈希。
    pub hash: bool,
    /// 是否逐字节比较明文内容。
    pub content: bool,
    /// 是否比较版本快照（veil_id + file_id + 版本链）。
    pub version: bool,
}

/// 单个比较条件的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonState {
    /// 该条件判定为相同。
    Same,
    /// 该条件判定为不同。
    Different,
    /// 例如输入没有携带版本信息，该条件无法比较。
    Unavailable,
}

/// 冲突判定的总体结论：由各条件结论汇总得出，供调用方决定业务策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictOutcome {
    /// 任一选中条件不同。
    Conflict,
    /// 全部选中条件相同。
    NoConflict,
    /// 没有发现差异，但至少一个选中条件不可比较。
    Indeterminate,
}

/// 一次比较的完整报告：每个选中条件的结论（未选为 None）与总体 outcome。
/// 调用方（如导入冲突提示）可据此向用户展示「内容相同但版本不同」等细节。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComparisonReport {
    /// 哈希条件结论。
    pub hash: Option<ComparisonState>,
    /// 内容条件结论。
    pub content: Option<ComparisonState>,
    /// 版本条件结论。
    pub version: Option<ComparisonState>,
    /// 汇总结论：Conflict / NoConflict / Indeterminate。
    pub outcome: ConflictOutcome,
}

/// 检查容器内部目标位置当前是否已有节点，供导入计划决定是否需要用户确认。
/// 入参：manager（&VeilManager）定位容器；path（&str）为容器内部逻辑路径；password（&str）用于开库；directory_guard 为可选的（&LeaseGuard, &LockedDirectory）目录租约和覆盖信息。
/// 返回值：当前能解析到节点返回 true，否则 false，不读取文件内容。
/// 异常：开库/路径/SQL 失败、超出目录覆盖范围、目录凭证缺失或模式/有效期不符返回 VeilError。
/// 注意事项：不预留路径；不传目录守卫时不申请资源租约。此入口对非共享写目录凭证按 Write 校验，因此并非接受所有目录读租约。写入仍需事务约束。
pub fn check_path_conflict(
    manager: &VeilManager,
    path: &str,
    password: &str,
    directory_guard: Option<(&LeaseGuard, &LockedDirectory)>,
) -> Result<bool> {
    let db = manager.open_db(password)?;
    if let Some((guard, directory)) = directory_guard {
        // 目录锁只能覆盖其子树：目标路径超出范围时直接拒绝。
        if !directory.covers(&crate::veil_ops::normalize_veil_dir(path)?) {
            return Err(invalid("操作超出目录锁范围"));
        }
        // 从租约守卫里挑出属于该目录的凭证（一个守卫可能聚合了多个目录锁）。
        let credential = guard
            .credentials()?
            .into_iter()
            .find(|credential| credential.directory_id() == Some(directory.id()))
            .ok_or_else(|| invalid("目录锁凭证缺失"))?;
        // 校验本地时间、数据库期限、令牌与所有者匹配；这里不向执行者发起存活探测。
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
    // resolve 返回节点即占用；这里只看「有没有」，不看内容。
    Ok(db.resolve(path)?.is_some())
}

/// 判断已有容器目标与候选来源是否在指定维度产生冲突。
/// 入参：manager/password 定位并解锁容器；path 为目标逻辑路径；source（FileInput）为候选内容；checks（ComparisonChecks）至少选 hash/content/version 一项；directory_guard 为可复用的目录守卫和范围。
/// 返回值：目标空闲为 NoConflict，非文件节点为 Conflict；文件比较中任一不同为 Conflict、全部相同为 NoConflict、仅缺比较信息为 Indeterminate。
/// 异常：条件全未选、目标变化、路径/租约/认证/取消/数据库/I/O 失败返回错误。
/// 注意事项：路径检查和内容比较不是一个不可分割快照；无目录守卫时比较阶段另申请文件读租约。结果供策略选择，不自行跳过或覆盖。
pub fn check_file_conflict(
    manager: &VeilManager,
    path: &str,
    password: &str,
    source: FileInput<'_>,
    checks: ComparisonChecks,
    directory_guard: Option<(&LeaseGuard, &LockedDirectory)>,
) -> Result<ConflictOutcome> {
    validate_checks(checks)?;
    // 路径空闲：直接无冲突，无需打开密文。
    if !check_path_conflict(manager, path, password, directory_guard)? {
        return Ok(ConflictOutcome::NoConflict);
    }
    let db = manager.open_db(password)?;
    // 目标节点类型决定后续：目录直接冲突（文件不能覆盖目录）；
    // 文件则进入内容比较。
    match db.resolve(path)? {
        None => return Ok(ConflictOutcome::NoConflict),
        Some(node) if node.kind != "file" => return Ok(ConflictOutcome::Conflict),
        Some(_) => {}
    }
    // 关闭只读快照连接，比较期间以持锁状态重新读取。
    drop(db);
    // 目标文件作为「容器密文」输入参与比较；有目录锁时复用，没有则走文件读锁。
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

/// 拒绝没有任何比较依据的请求。
/// 入参：checks（ComparisonChecks）为三个可独立选择的布尔条件。
/// 返回值：至少一项为 true 返回 Ok(())。
/// 异常：全部 false 时返回参数类 VeilError。
/// 注意事项：只检查选择，不验证输入是否能提供明文、哈希或版本。
fn validate_checks(checks: ComparisonChecks) -> Result<()> {
    if !checks.hash && !checks.content && !checks.version {
        return Err(invalid("请选择至少一个比较条件"));
    }
    Ok(())
}

/// 持锁读取容器密文文件时的状态集合：打开中的容器库、文件节点、租约守卫、
/// 锁模式与解密用文件密钥。租约约束遵守协议的写入，读取后再复查期限及节点；
/// 租约过期或不遵守协议的外部改动仍可能发生，不能把它等同于不可变磁盘快照。
struct LockedContainer<'a> {
    /// 容器管理器：提供容器目录（密文所在位置）。
    manager: &'a VeilManager,
    /// 已解锁的容器库连接（用于查节点、验锁与写哈希缓存）。
    db: ContainerDb,
    /// 目标文件节点（含密文名、nonce、大小、版本链等）。
    node: Node,
    /// 租约守卫：Owned 为本函数申请的文件锁，Directory 为复用的目录锁。
    guard: ComparisonGuard<'a>,
    /// 持锁模式（Read/Write/SharedWrite），验锁与写哈希时使用。
    mode: LockMode,
    /// 解密用32字节数据密钥副本，包装释放时零化；不代表清除所有其他副本。
    key: Zeroizing<[u8; 32]>,
}

impl LockedContainer<'_> {
    /// 在确认文件仍为同一节点后保存本次计算的明文哈希缓存。
    /// 入参：self 为持锁容器状态；hash（&str）为已计算的 BLAKE3 十六进制文本。
    /// 返回值：即时写事务验证并写入 file_hash 后提交成功为 Ok(())。
    /// 异常：凭证失效、节点变化（VersionConflict）、序列化或 SQL/提交失败返回错误。
    /// 注意事项：调用路径只在非 Read 模式计算后进入；方法本身不验证哈希文本格式。需持有效 Write/SharedWrite 租约；提交错误不应简单等同于已确认回滚。
    fn store_hash(&mut self, hash: &str) -> Result<()> {
        let credentials = self.guard.credentials()?;
        // 即时事务：抢占写权后再检查，避免「先查后写」的竞态窗口。
        let tx = self
            .db
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // 写缓存前逐个验锁：过期租约不能写哈希。
        for credential in &credentials {
            locks::validate(&tx, credential, Some(self.mode))?;
        }
        // 节点已变化（被并发覆盖/删除）则放弃写缓存，防止把哈希挂到错误版本上。
        if container::node(&tx, &self.node.id)?.as_ref() != Some(&self.node) {
            return Err(VeilError::VersionConflict(
                "补充文件哈希期间容器文件发生变化".into(),
            ));
        }
        // UPSERT 语义：同一文件已有哈希则更新为最新值（文件 ID 不变，内容可能已变）。
        tx.execute(
            "INSERT INTO file_hash(file_id,blake3) VALUES(?1,?2) ON CONFLICT(file_id) DO UPDATE SET blake3=excluded.blake3",
            rusqlite::params![self.node.id, hash],
        )?;
        tx.commit()?;
        Ok(())
    }
}

/// 比较期间使用的租约来源：Owned 是本模块自己申请并持有的文件锁；
/// Directory 是借用调用方已有的目录锁（凭目录 ID 找到对应凭证）。
enum ComparisonGuard<'a> {
    /// 自持文件租约（随 LockedContainer 一起生命周期结束）。
    Owned(LeaseGuard),
    /// 借用调用方目录租约 + 目录 ID（凭证从聚合守卫中筛选）。
    Directory(&'a LeaseGuard, &'a str),
}
impl ComparisonGuard<'_> {
    /// 取得比较实际使用的租约凭证快照，供读后验证或写缓存。
    /// 入参：self（&ComparisonGuard）为自持文件守卫或借用目录守卫。
    /// 返回值：Owned 返回全部凭证副本；Directory 返回匹配目录 ID 的单元素 `Vec<Credential>`。
    /// 异常：共享状态锁中毒或对应目录凭证缺失时返回错误。
    /// 注意事项：取得快照可能等待后台续租持有的 Mutex；返回副本仍需在使用时校验，不能当成永久授权。
    fn credentials(&self) -> Result<Vec<locks::Credential>> {
        match self {
            Self::Owned(guard) => guard.credentials(),
            Self::Directory(guard, id) => {
                // 目录租约守卫可能聚合多个目录锁，按目录 ID 挑出当前目录的凭证。
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

/// 标准化后的比较输入：统一提供 reader（明文流）、hash、version 与 verify
/// （确认数据在比较期间未被改动）四个能力。外部文件记录「比较前」元数据，
/// 验证时对比长度与修改时间；密文输入持锁并复查节点。
enum Prepared<'a> {
    /// 只有元数据与（可选）已登记哈希：不能读明文，只能做哈希/版本比较。
    Metadata(&'a VersionSnapshot, Option<&'a str>),
    /// 调用方已解密的明文。
    Plaintext {
        /// 调用方提供的明文借用，哈希和内容比较均从此读取。
        bytes: &'a [u8],
        /// 可选身份与版本历史，仅参与版本维度，不用于验证字节来源。
        version: Option<&'a VersionSnapshot>,
    },
    /// 容器密文文件：持锁读取，可现场计算哈希并回写缓存。
    Ciphertext(LockedContainer<'a>),
    /// 容器外文件：路径 + 比较前的文件元数据（用于变化检测）。
    External {
        /// 比较时重新打开的本地路径，不是已固定 inode 的文件句柄。
        path: &'a Path,
        /// 准备时记录的文件长度和修改时间，供结束时做有限的变化检测。
        before: Metadata,
        /// 调用方附带的可选版本，外部文件系统本身不产生此信息。
        version: Option<&'a VersionSnapshot>,
    },
}

impl<'a> Prepared<'a> {
    /// 将不同来源包装为统一比较状态，并为容器密文准备租约与密钥。
    /// 入参：input（FileInput）为元数据、明文、外部文件或容器密文来源；借用须覆盖返回 Prepared 的使用期。
    /// 返回值：成功返回 Prepared，外部文件保存初始元数据，密文保存数据库、节点、守卫、模式和数据密钥。
    /// 异常：外部输入非文件、路径/目录范围/凭证错误、文件缺失、密码/密钥/数据库/租约失败返回错误。
    /// 注意事项：fs::metadata 跟随外部符号链接，不创建真正文件快照。无目录守卫时申请文件 Read 租约；有守卫时可识别目录 Read/Write/SharedWrite，具体读后稳定性另行复查。
    fn new(input: FileInput<'a>) -> Result<Self> {
        Ok(match input {
            FileInput::ContainerMetadata { version, hash } => Self::Metadata(version, hash),
            FileInput::ContainerPlaintext { bytes, version } => Self::Plaintext { bytes, version },
            FileInput::External { path, version } => {
                // 记录比较前的元数据：verify 时用它检测文件在比较期间是否被改动。
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
                // 复用目录租约时，目标必须位于该目录覆盖范围内。
                if !directory.covers(&crate::veil_ops::normalize_veil_dir(path)?) {
                    return Err(invalid("操作超出目录锁范围"));
                }
                let db = manager.open_db(password)?;
                // 定位文件节点；目标不是文件（目录/不存在）不能按文件比较。
                let node = db
                    .resolve(path)?
                    .filter(|node| node.kind == "file")
                    .ok_or_else(|| VeilError::FileNotFound(path.into()))?;
                // 从聚合守卫中挑出该目录的锁凭证。
                let credential = guard
                    .credentials()?
                    .into_iter()
                    .find(|credential| credential.directory_id() == Some(directory.id()))
                    .ok_or_else(|| invalid("目录锁凭证缺失"))?;
                // 锁模式：共享写（导入目录锁）直接用 SharedWrite；
                // 否则查 operation_locks 表中该凭证的实际模式（read/write）。
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
                // 开始读取前先验锁：过期或已转移的租约不能读密文。
                locks::validate(&db.conn, &credential, Some(mode))?;
                // 取出该文件的数据密钥（32 字节），供流式解密使用。
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
                // 没有目录锁时：按普通读锁打开文件（read_locked 内部申请租约并验锁）。
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

    /// 为内容比较或哈希计算打开从头开始的明文读取流。
    /// 入参：self（&Prepared）为已准备来源。
    /// 返回值：成功返回借用 self 生命周期的 `Box<dyn Read>`；明文用内存游标，外部用文件，密文用分块认证解密器。
    /// 异常：仅元数据输入无法提供明文时返回参数错误；打开、节点密文名/nonce/大小无效等错误传播，读取时仍可继续报认证/I/O 错误。
    /// 注意事项：每次调用重新打开流；密文只有被读取的分块经过认证，打开成功不表示整文件已经认证。
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

    /// 在出具比较结论前复查输入的可观察稳定性。
    /// 入参：self（&Prepared）含准备时的外部元数据或容器节点与租约。
    /// 返回值：外部长度/修改时间未观察到变化，或容器租约和节点仍匹配时返回 Ok(())；内存来源直接成功。
    /// 异常：外部变化返回参数类错误；容器节点变化为 VersionConflict；凭证、数据库或文件元数据错误传播。
    /// 注意事项：不主动探测执行者，也不证明外部文件绝无改动；等长且修改时间相同的替换可能漏检，两次 modified 都不可得也视作相同。
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

    /// 取得文件身份与版本继承链，用于区分同内容但不同历史的文件。
    /// 入参：self（&Prepared）为比较来源。
    /// 返回值：有版本返回 Some(VersionSnapshot)，含 veil_id、file_id、version_history；调用方未提供版本时为 None。
    /// 异常：容器信息或版本链解析错误返回 VeilError。
    /// 注意事项：内存/外部输入的快照由调用方负责可信性；密文从当前库身份及已读节点构造，最终还需 verify。
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

    /// 取得明文 BLAKE3 比较值，优先复用已有哈希以减少解密。
    /// 入参：self（&mut Prepared）为比较来源，必要时借此回写缓存。
    /// 返回值：有调用方/数据库缓存直接返回其 String；否则完整读取明文计算并返回十六进制值。
    /// 异常：缺哈希的纯元数据无法读明文、取消、认证、读后变化、读取或缓存写入失败返回错误。
    /// 注意事项：缓存命中不重新认证密文或校验哈希文本格式；现场计算后才验证稳定性并在写模式补缓存。最终 compare_prepared 仍会复查两侧状态。
    fn hash(&mut self) -> Result<String> {
        // 快捷路径 1：元数据输入带哈希。
        if let Self::Metadata(_, Some(hash)) = self {
            return Ok((*hash).to_owned());
        }
        // 快捷路径 2：容器文件已有哈希缓存（file_hash 表）。
        if let Self::Ciphertext(item) = self {
            let value: Option<String> = item
                .db
                .conn
                .query_row(
                    "SELECT blake3 FROM file_hash WHERE file_id=?1",
                    [&item.node.id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(value) = value {
                return Ok(value);
            }
        }
        // 常规路径：流式读取明文逐块喂给 BLAKE3。
        let digest = {
            let mut reader = self.reader()?;
            let mut hasher = blake3::Hasher::new();
            // 64KiB 明文缓冲会在包装释放时零化；不是每次读取之后立即清除。
            let mut buffer = Zeroizing::new(vec![0u8; 64 * 1024]);
            loop {
                // 每块读取前检查取消：长文件哈希可被用户中止。
                veil_core::progress::check_cancelled()?;
                let read = checked_read(&mut *reader, &mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            hasher.finalize().to_hex().to_string()
        };
        // 计算期间数据可能已被并发修改：先 verify 再信任结果。
        self.verify()?;
        // 持有写锁时把新哈希回写缓存（读锁期间不允许写库）。
        if let Self::Ciphertext(item) = self
            && item.mode != LockMode::Read
        {
            item.store_hash(&digest)?;
        }
        Ok(digest)
    }
}

/// 为内容比较凑齐一个明文块，避免把网络或解密流短读误判成不同内容。
/// 入参：reader（&mut dyn Read）为一侧明文流；buffer（&mut [u8]）为本轮目标缓冲，允许为空。
/// 返回值：返回实际写入长度，等于容量或提前到 EOF；空缓冲返回0。
/// 异常：取消或底层读取/认证错误返回 VeilError。
/// 注意事项：每次补读前检查当前线程进度上下文的取消；不在这里自动重试 Interrupted，失败缓冲可能已部分填充。
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

/// 保留解密读取器嵌入 I/O 错误中的业务取消信号。
/// 入参：reader 为明文 Read 流；buffer 为本次可写缓冲。
/// 返回值：成功返回实际读取字节数，0 按 Read 语义代表结束或空缓冲。
/// 异常：直接内嵌 VeilError::Cancelled 的 I/O 错误恢复为 Cancelled；其余通过普通 I/O 错误转换传播。
/// 注意事项：本方法不主动查取消标志、不循环补满，也不遍历任意深度的错误来源链。
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

/// 读取两侧全部明文并累计是否逐字节相同，同时让密文读取完成认证。
/// 入参：left/right（&Prepared）分别为两侧可提供明文的来源。
/// 返回值：读至两侧 EOF 后，所有对应块长度及内容都一致为 true，否则 false。
/// 异常：纯元数据不能提供明文、取消、解密认证或读取失败返回错误。
/// 注意事项：发现不同仍继续读完两侧，避免忽略尾部认证错误；64KiB 缓冲释放时零化，读取结束后外层仍需 verify 稳定性。
fn content_equal(left: &Prepared<'_>, right: &Prepared<'_>) -> Result<bool> {
    let mut left_reader = left.reader()?;
    let mut right_reader = right.reader()?;
    // 两侧64KiB明文缓冲均在释放时零化；每轮先补满或读至各自 EOF，再比较对应区间。
    let mut left_buffer = Zeroizing::new(vec![0u8; 64 * 1024]);
    let mut right_buffer = Zeroizing::new(vec![0u8; 64 * 1024]);
    let mut same = true;
    loop {
        // 每轮各读一块；read_block 会处理短读直到缓冲填满或流结束。
        let a = read_block(&mut *left_reader, &mut left_buffer)?;
        let b = read_block(&mut *right_reader, &mut right_buffer)?;
        // 块长不同或块内容不同 ⇒ 内容不同。
        if a != b || left_buffer[..a] != right_buffer[..b] {
            same = false;
        }
        // 两侧都到达 EOF：比较结束。
        if a == 0 && b == 0 {
            break;
        }
    }
    Ok(same)
}

/// 统一比较两个来源的明文哈希、完整内容和版本身份，供冲突策略使用。
/// 入参：left/right（FileInput）为比较来源；checks（ComparisonChecks）至少选择一项，可同时选择多项。
/// 返回值：返回 ComparisonReport：未选条件为 None，选中条件记录 Same/Different/Unavailable，并汇总体结论。
/// 异常：参数、输入准备、取消、密码/租约/认证/版本链/缓存写入/数据库/I/O 错误返回 VeilError。
/// 注意事项：不判断目标路径是否可覆盖，也不修改文件内容；某些密文写租约场景会补哈希缓存。元数据无法提供选中内容或缺失哈希时会报错，缺版本才用 Unavailable。
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

/// 依次执行选中维度，复查来源稳定性后汇总冲突结论。
/// 入参：left/right（&mut Prepared）为已准备状态；checks 为已通过至少一项检查的条件。
/// 返回值：任一 Different 为 Conflict；无差异但有 Unavailable 为 Indeterminate；其余为 NoConflict。
/// 异常：哈希/读取/版本链/租约/稳定性或数据库错误直接返回。
/// 注意事项：本内部函数不再次 validate_checks；版本相同须容器 ID、文件 ID 和版本链关系都相同。即使前一维度不同，也继续执行后续所选维度。
fn compare_prepared(
    left: &mut Prepared<'_>,
    right: &mut Prepared<'_>,
    checks: ComparisonChecks,
) -> Result<ComparisonReport> {
    // 把布尔比较结果映射成状态枚举。
    let state = |same| {
        if same {
            ComparisonState::Same
        } else {
            ComparisonState::Different
        }
    };
    // 维度 1：明文哈希（命中缓存时无需解密）。
    let hash = if checks.hash {
        Some(state(left.hash()? == right.hash()?))
    } else {
        None
    };
    // 维度 2：逐字节内容比较。
    let content = if checks.content {
        Some(state(content_equal(left, right)?))
    } else {
        None
    };
    // 维度 3：版本链比较——同一容器、同一文件 ID 且版本链关系为 Same 才算相同；
    // 任一侧缺版本信息 ⇒ Unavailable（不猜测）。
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
    // 出结论前最后复查：比较期间数据被并发改动时，宁可报错也不给错误结论。
    left.verify()?;
    right.verify()?;
    // 汇总三个维度的结论。
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
