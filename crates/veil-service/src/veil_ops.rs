//! SQLCipher 容器操作：文件内容留在磁盘，数据库是唯一权威清单。
//!
//! 所属业务模块：veil-service 的容器业务编排层，对应 CLI 的
//! init / unlock / add / rm / mv / passwd / ex 等命令的底层实现。
//!
//! 文件内核心结构与函数清单：
//! - VeilManager：容器生命周期操作的门面（统一入口），所有公开操作都挂在它身上
//! - AddFileSpec / Pending：批量导入的请求描述与加密准备结果
//! - ImportBatchContext / QueuedImport：导入管线多个并行阶段的共享上下文与待提交队列项
//! - LockedDirectory：目录租约对应的路径与节点 ID 描述；读/共享写/排他写模式决定兼容性
//! - CancelledFiles：提交前中止时清理本次密文的兜底守卫
//! - init_veil / init_veil_with_id：创建容器（生成稳定 ID 与加密数据库）
//! - open_db / read_meta / read_meta_snapshot：解锁与读取元数据快照
//! - add_files_inner：文件加密写入编排（先写 .enc 密文，再提交数据库引用）
//! - commit_import_batch：导入管线批量提交（同一事务内应用全部待提交项）
//! - remove_path_deferred：删除（先逻辑删除，再异步回收密文）
//! - move_path：移动/改名（防环 + 同目录唯一名称校验）
//! - extract_file / read_file_bytes：解密导出与内存预览
//! - cleanup：失去数据库引用的孤儿密文垃圾回收
//! - change_password / change_password_full：普通改密与双密钥完整改密
//!
//! 依赖能力：
//! - storage::container：容器库 veil.db（SQLite 的加密扩展 SQLCipher）的业务表读写
//! - storage::locks：文件级租约管理（30 秒租约、5 秒续租、等待超时与退避）
//! - file_ops：1 MiB 分块 ChaCha20-Poly1305 流式加密/解密
//! - version / comparison：内容版本链比较，防止过期修改覆盖新内容
//!
//! 整体业务目标：文件内容留在磁盘上的随机命名 .enc 密文中，veil.db 里的
//! 目录树与文件引用是唯一权威清单；任何写入都遵循"密文先落盘、
//! 数据库引用后提交"，中断后以数据库当前状态为准恢复。
use crate::error::{Result, VeilError};
use crate::file_ops;
use crate::metadata::{FileEntry, MetaData};
use crate::storage::{
    container::{self, ContainerDb, DataKey, Node},
    id, invalid, json,
    locks::{self, LeaseGuard, LockManager, LockMode, LockOptions},
    now,
};
use crate::version::VersionSnapshot;
use rusqlite::{params, TransactionBehavior};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use zeroize::Zeroizing;

/// 垃圾清理时单批 SQL 查询/删除的候选对象数量上限，
/// 分批处理可避免单条 SQL 绑定过多参数，也降低单轮占库时间。
const CLEANUP_QUERY_BATCH_SIZE: usize = 256;

/// 一次批量添加中的单个源文件和 Veil 内目标路径。
///
/// 业务定位：`veil add`（含目录递归导入展开后）中"把一个本地源文件
/// 加密写入容器内某个路径"的单条请求描述。
///
/// 各字段共同描述一条导入业务：源文件在哪儿、目标路径是什么、
/// 是否携带编辑基础版本（乐观并发控制）、是否禁止覆盖已有文件。
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
    /// 描述一次本地文件写入容器的请求，默认允许替换同名文件。
    /// 入参：source（impl `Into<PathBuf>`）为本地文件位置；target（impl `Into<String>`）为容器内部目标路径。
    /// 返回值：AddFileSpec，base_version=None，reject_existing=false。
    /// 异常：无 Result 返回。
    /// 注意事项：只保存原值，不验证路径或源类型；真正写入时才规范化并检查，不应把构造成功当成已授权覆盖。
    pub fn new(source: impl Into<PathBuf>, target: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            base_version: None,
            reject_existing: false,
        }
    }

    /// 为编辑保存绑定读取时的基础版本，防止过期内容覆盖新版本。
    /// 入参：self 按值消费；base_version（VersionSnapshot）为原容器/文件身份及版本链。
    /// 返回值：带基础版本约束的 AddFileSpec。
    /// 异常：无错误返回。
    /// 注意事项：不在此查库，写入时才核对；本调用不改变 reject_existing，两种限制可同时存在。
    pub fn with_base_version(mut self, base_version: VersionSnapshot) -> Self {
        self.base_version = Some(base_version);
        self
    }

    /// 将导入请求设为拒绝已存在的同名目标。
    /// 入参：self（AddFileSpec）按值消费。
    /// 返回值：reject_existing=true 的请求，其他字段保留。
    /// 异常：无错误返回。
    /// 注意事项：不立即检查容器；准备和最终应用阶段执行约束，基础版本仍保留。
    pub fn without_overwrite(mut self) -> Self {
        self.reject_existing = true;
        self
    }
}

/// 删除操作命中的路径类型。
///
/// 业务定位：告诉调用方这次 `rm` 删掉的是一个文件还是整个目录，
/// 前端据此决定是刷新单个文件条目还是整个目录子树。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovedPathKind {
    /// 删除的是单个文件。
    File,
    /// 删除的是目录及其全部文件。
    Directory,
}

/// 移动操作命中的路径类型。
///
/// 业务定位：告诉调用方这次 `mv` 移动的是文件还是目录；
/// 目录移动只改父指针，不批量重写后代完整路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovedPathKind {
    /// 移动的是单个文件。
    File,
    /// 移动的是目录及其全部子文件。
    Directory,
}

/// 容器内路径的类型。
///
/// 业务定位：查询"容器里这个路径是什么"的结果枚举，
/// 携带规范化后的实际路径，供界面决定展示与后续操作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerPathKind {
    /// 该路径指向一个文件，携带规范化后的文件路径。
    File(String),
    /// 该路径指向一个目录，携带规范化后的目录路径。
    Directory(String),
    /// 该路径在容器中不存在，携带规范化后的查询路径。
    Missing(String),
}

/// 容器生命周期操作的门面（统一入口）。
///
/// 业务定位：所有对单个容器的高级业务操作（创建、解锁、文件增删改查、
/// 改密、清理）都通过 VeilManager 发起；它本身只记录容器目录位置，
/// 权威状态都放在磁盘上的 veil.db 里，不长期持有数据库连接。
///
/// 核心成员整体作用：
/// - veil_dir：调用方提供的容器路径，可相对，内有 veil.db 与随机命名 .enc 密文
/// - expected_veil_id：期望的容器稳定 ID；设置后每次解锁都核对真实身份，防止目录被换包
/// - lock_options：等锁期限与取消信号，不设置固定租约有效时长或后台续租间隔
///
/// 使用场景：CLI / 桌面端 / 移动端各自构造一个 VeilManager 指向目标
/// 容器目录，每次操作临时打开数据库、完成后关闭，从而支持多进程
/// 并发访问同一容器。
#[derive(Clone)]
pub struct VeilManager {
    /// 原样保存的容器目录路径，可相对或绝对；权威业务状态位于其 veil.db。
    pub veil_dir: PathBuf,
    /// 期望的容器稳定 ID；None 表示不校验身份（如新建容器场景）。
    expected_veil_id: Option<String>,
    /// 文件租约参数集合，控制锁等待时间、取消行为等。
    lock_options: LockOptions,
}
/// 单次文件写入的完整准备结果。
///
/// 业务定位：add_files_inner 在正式加密前把每个 AddFileSpec 展开成一条
/// Pending：目标父目录、稳定文件 ID、加密密钥与 nonce 等全部确定，
/// 之后加密消费这些材料，最终提交仍会重新读取活动密钥、节点并校验租约。
///
/// 字段含义（均为加密写入所必需的业务要素）：
/// - spec：原始请求（含源路径、目标路径、版本约束）
/// - parent：目标父目录的节点 ID；name：文件在父目录下的名称
/// - base：覆盖已有文件时，该文件的旧节点记录（None 表示新建）
/// - identity：稳定文件节点 ID（覆盖沿用旧 ID，新建时随机生成）
/// - key/nonce：本轮加密使用的文件数据密钥与 12 字节基础 nonce
/// - cipher：随机命名的 .enc 密文文件名；size/hash：明文大小与哈希
/// - base_key/base_hash：旧密文对应的密钥与哈希（用于内容不变检测）
/// - unchanged：新旧大小与明文哈希一致时跳过追加版本，仍会替换密文引用
struct Pending {
    /// 原导入请求，保存本地来源、内部目标及版本/覆盖约束。
    spec: AddFileSpec,
    /// 准备事务确定的目标父目录节点ID，最终提交再次确认目录有效。
    parent: String,
    /// 该文件在父目录内的单级名称。
    name: String,
    /// 覆盖前旧Node快照；新建为None，最终提交用于检测并发变化。
    base: Option<Node>,
    /// 稳定文件ID；覆盖沿用旧ID，新建时生成。
    identity: String,
    /// 准备时选中的活动数据密钥，最终提交须仍为活动密钥。
    key: DataKey,
    /// 本轮新生成的12字节基础nonce，用于分块加密；重写内容需新值。
    nonce: [u8; 12],
    /// 本轮生成的随机密文名，开始为空，加密阶段填入。
    cipher: String,
    /// 流式加密实际读取的明文字节数，开始为0。
    size: u64,
    /// 加密同时计算的BLAKE3明文哈希文本，开始为空。
    hash: String,
    /// 旧Node引用的密钥；旧哈希缺失时用于解密旧密文计算。
    base_key: Option<DataKey>,
    /// 准备时读取的旧明文哈希缓存，None时按需现场计算。
    base_hash: Option<String>,
    /// 新旧大小与哈希一致标记，只决定是否追加内容版本，不阻止重写新密文。
    unchanged: bool,
}

/// 提交事务内共享的节点、密钥快照，不在逐文件写入时重复读取。
///
/// 业务定位：批量导入可能一次提交成百上千个文件；进入写事务后先把
/// 涉及的所有节点（父目录 + 文件）与当前活动密钥一次性读进内存，
/// 逐文件应用时直接查快照，避免在短事务内反复查询数据库。
struct ApplySnapshot {
    /// 当前活动文件密钥 ID；所有新写入必须使用它（完整改密期间防止旧密钥误提交）。
    active_key_id: String,
    /// 节点 ID → 节点记录 的快照；key 为父目录 ID 或文件 ID，
    /// value 是提交时刻的节点状态（用于校验父目录存在、文件身份与版本未变）。
    nodes: HashMap<String, Node>,
}
impl ApplySnapshot {
    /// 收集本批父节点和文件节点的事务快照，并读取当前活动密钥身份。
    /// 入参：conn（&Connection）须为当前写事务；pending 为 `IntoIterator<Item=&Pending>`，每项一份待提交文件材料。
    /// 返回值：ApplySnapshot：active_key_id 及节点 ID -> Node 映射。
    /// 异常：文件身份重复、无活动密钥或节点/密钥查询失败返回错误。
    /// 注意事项：每份文件身份只允许一次，父节点去重；缺失节点不会在此全部拒绝，后续 apply 时验证。快照只供同一事务使用。
    fn load<'a>(
        conn: &rusqlite::Connection,
        pending: impl IntoIterator<Item = &'a Pending>,
    ) -> Result<Self> {
        let mut identities = HashSet::new();
        let mut files = HashSet::new();
        for file in pending {
            if !files.insert(file.identity.clone()) {
                return Err(invalid("批量文件身份重复"));
            }
            identities.insert(file.parent.clone());
            identities.insert(file.identity.clone());
        }
        let nodes = container::nodes_batch(conn, &identities.into_iter().collect::<Vec<_>>())?;
        let active_key_id = container::keys(conn)?
            .into_iter()
            .find(|key| key.status == "active")
            .ok_or_else(|| invalid("缺少活动文件密钥"))?
            .key_id
            .clone();
        Ok(Self {
            active_key_id,
            nodes,
        })
    }
}

/// 导入管线各并行加密线程共享的目录缓存、完成队列与持锁身份。
///
/// 业务定位：目录递归导入时，多个工作线程并行加密各自文件，共享
/// 目录缓存、提交队列与目录租约；本结构把这些共享资源打包传递。
pub(crate) struct ImportBatchContext {
    /// 共享目录缓存：仅在目录准备事务成功提交且目录租约覆盖时记录容器节点，避免缓存回滚项。
    pub directories: Arc<crate::directory_cache::DirectoryCache>,
    /// 提交队列的发送端：加密完成的 QueuedImport 入队，等待批量提交线程消费。
    pub sender: veil_core::queue::Sender<QueuedImport>,
    /// 覆盖整个导入目标目录的共享租约（None 表示逐文件自行持锁）。
    pub directory_guard: Option<Arc<LeaseGuard>>,
    /// 持锁执行身份：租约记在哪个进程/线程/任务名下，用于跨进程回收。
    pub owner: crate::execution::ExecutionOwner,
}

/// 已加密完成、等待批量提交的队列项。
///
/// 业务定位：导入管线的中间产物——密文已落盘但数据库引用尚未更新，
/// 批量提交线程把多个 QueuedImport 合并进一个短写事务统一应用。
pub(crate) struct QueuedImport {
    /// 这批待提交文件的全部准备数据（密文名、密钥、版本等）。
    pending: Vec<Pending>,
    /// 提交前中止时的密文清理守卫。
    cancelled_files: CancelledFiles,
    /// 覆盖本批文件的租约；提交事务内要校验其凭证仍有效。
    guard: Arc<LeaseGuard>,
    /// 共享目录租约对应的目录节点 ID（逐文件持锁时为 None）。
    directory_id: Option<String>,
    /// 加密时读到的容器身份，提交前核对防止目录被替换。
    veil_id: String,
    /// 租约是否由本次导入自行申请（true 时提交成功后由本批释放）。
    release_owned: bool,
}

impl QueuedImport {
    /// 取得待提交批次的首个目标，供失败日志标记业务对象。
    /// 入参：self（&QueuedImport）为非空待提交项。
    /// 返回值：首个 Pending.spec.target 的 &str。
    /// 异常：pending 为空会因索引越界 panic。
    /// 注意事项：内部构造方须保证非空；不是该批全部目标的汇总。
    pub(crate) fn target(&self) -> &str {
        &self.pending[0].spec.target
    }
    /// 功能：读取待提交导入批次中的文件数，供批次调度统计工作量。
    /// 入参：self 为当前 QueuedImport；pending 每项是一份已准备的导入文件。
    /// 返回值：返回 pending 的元素数，空批次为 0。
    /// 异常：不访问数据库，不返回错误。
    /// 注意事项：计数不表示这些文件已成功提交或物理发布。
    pub(crate) fn file_count(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
impl QueuedImport {
    /// 为提交防御测试故意破坏首份待提交文件的密钥身份。
    /// 入参：self（&mut QueuedImport）为测试创建的非空队列项。
    /// 返回值：返回首个目标路径 String，供断言定位。
    /// 异常：空 pending 时索引 panic。
    /// 注意事项：仅 cfg(test) 可用；只改测试内存材料，不直接改数据库密钥。
    pub(crate) fn invalidate_key(&mut self) -> String {
        self.pending[0].key.key_id = "invalid".into();
        self.pending[0].spec.target.clone()
    }
}

/// 路径和节点 ID 在取得租约后共同校验，供任务内工作线程复用。
///
/// 业务定位：目录租约申请成功后，把"规范化路径 + 目录节点 ID"绑定
/// 在一起。后续每个文件写入都要先确认目标路径落在该目录锁覆盖范围内，
/// 并凭目录 ID 在租约凭证集中定位对应的目录凭证。
pub struct LockedDirectory {
    /// 规范化后的目录路径（根目录为 ""），用于判断某个目标路径是否在其子树内。
    path: String,
    /// 目录的稳定节点 ID，用于在租约凭证集中定位对应的目录凭证。
    id: String,
}
impl LockedDirectory {
    /// 将已知目录路径与节点身份包装成导出租约范围描述。
    /// 入参：path（String）为规范化容器目录路径，根为空；id（String）为该目录节点 ID。
    /// 返回值：LockedDirectory，保存路径和身份。
    /// 异常：无错误返回。
    /// 注意事项：不申请租约、不验库，调用方须先取得对应目录读租约并核对身份。
    pub(crate) fn for_export(path: String, id: String) -> Self {
        Self { path, id }
    }
    /// 借用目录范围描述绑定的节点身份。
    /// 入参：self（&LockedDirectory）为已构造范围。
    /// 返回值：&str，目录节点 ID。
    /// 异常：无错误返回。
    /// 注意事项：不证明租约仍有效或节点仍在原路径。
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
    /// 判断目标逻辑路径是否位于本目录的后代范围。
    /// 入参：self 为目录描述；target（&str）应已规范化。
    /// 返回值：根描述覆盖任意路径；其他目录要求其前缀后紧跟“/”，满足为 true。
    /// 异常：无错误返回。
    /// 注意事项：非根目录自身路径不会返回 true，只匹配后代；纯字符串判断，不规范化、不查节点或验租约。
    pub(crate) fn covers(&self, target: &str) -> bool {
        self.path.is_empty()
            || target
                .strip_prefix(&self.path)
                .is_some_and(|suffix| suffix.starts_with('/'))
    }
    /// 取得目录路径的字节长度，供选择最具体的覆盖目录。
    /// 入参：self（&LockedDirectory）为范围描述。
    /// 返回值：usize，UTF-8字符串字节数，根为0。
    /// 异常：无错误返回。
    /// 注意事项：不是字符数量或路径层级数，不用于直接判断合法深度。
    pub(crate) fn path_len(&self) -> usize {
        self.path.len()
    }
}

/// 仅在提交前中止时清除本次操作创建的密文；提交结果不确定时交给常规恢复流程。
///
/// 业务定位：加密落盘与数据库提交之间隔着时间窗。若在中止/出错时
/// 数据库尚未提交，这些新密文就是无人引用的垃圾，本结构在析构时
/// 兜底删除它们；一旦事务已提交，则绝对不能删（数据库已引用）。
struct CancelledFiles {
    /// 本次生成的待清理密文路径，由容器目录拼接，未强制绝对化。
    files: Vec<PathBuf>,
    /// 本次创建的暂存目录路径，析构时尝试整目录移除。
    staging: Vec<PathBuf>,
    /// 是否必须保留密文；提交成功或结果不确定时置true，避免误删可能已引用内容。
    committed: std::cell::Cell<bool>,
}

/// 沿容器逻辑路径找到可继续补建子目录的最深现存目录。
/// 入参：conn 为调用方连接/事务；path（&str）为已规范化的目录路径，可为空表示根。
/// 返回值：Node，首段缺失前最后一个现存目录，路径全存在时为目标目录。
/// 异常：根缺失、途中遇到非目录或查询失败返回错误。
/// 注意事项：跳过空分段，不处理“..”含义，合法性由调用方保证；查询本身不申请目录租约。
fn deepest_directory(conn: &rusqlite::Connection, path: &str) -> Result<Node> {
    // prefix：逐段累积已走到的路径前缀，用于查询下一级是否仍存在。
    let mut prefix = String::new();
    // current：目前确认存在的最深目录节点，起点是根目录。
    let mut current = container::resolve(conn, "")?.ok_or_else(|| invalid("根目录缺失"))?;
    // 沿路径逐段下钻：能查到目录就更新 current；查到文件报错；查不到即停。
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
    /// 在尚需清理时尽力移除本次密文与空暂存目录。
    /// 入参：self（&mut CancelledFiles）持本次文件/暂存路径及保留标记。
    /// 返回值：无；committed=true 直接保留所有路径，false 时逐项尝试删除。
    /// 异常：删除失败被忽略，不覆盖业务错误。
    /// 注意事项：staging 仅 remove_dir，不递归删除非空目录；committed 也可能为避免不确定提交误删而置位，并非只代表已确认成功。后续需另行垃圾扫描。
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
    /// 构造面向一个容器目录的业务入口，不绑定预期身份。
    /// 入参：veil_dir（PathBuf）为调用方提供的目录，可相对、可尚不存在。
    /// 返回值：VeilManager，采用默认30秒等锁策略和无预期 ID。
    /// 异常：无错误返回。
    /// 注意事项：仅保存路径，不绝对化、不建库、不验密码；真正操作时另行检查。
    pub fn new(veil_dir: PathBuf) -> Self {
        Self {
            veil_dir,
            expected_veil_id: None,
            lock_options: locks::default_options(),
        }
    }
    /// 构造绑定期望容器身份的业务入口，防止操作错误目录副本。
    /// 入参：veil_dir（PathBuf）为容器目录，保留原路径；veil_id（impl `Into<String>`）为期望身份。
    /// 返回值：带 expected_veil_id 的 VeilManager。
    /// 异常：无错误返回，不在此校验 UUID。
    /// 注意事项：后续开库才比较实际身份；构造不加锁、不证明目录可用。
    pub fn for_veil(veil_dir: PathBuf, veil_id: impl Into<String>) -> Self {
        Self {
            veil_dir,
            expected_veil_id: Some(veil_id.into()),
            lock_options: locks::default_options(),
        }
    }
    /// 替换后续申请资源租约的等待和取消策略。
    /// 入参：self 按值消费；options（LockOptions）包含 timeout 和可选 cancellation。
    /// 返回值：更新选项后的 VeilManager。
    /// 异常：无错误返回。
    /// 注意事项：不控制固定租约时长或续租间隔，也不改变已申请的守卫；timeout=None 可无限等锁。
    pub fn with_lock_options(mut self, options: LockOptions) -> Self {
        self.lock_options = options;
        self
    }
    /// 解锁当前容器并尝试刷新本机展示登记。
    /// 入参：self 提供目录及可选预期身份；password（&str）为 SQLCipher 数据库密码。
    /// 返回值：成功返回 ContainerDb，供调用方执行业务。
    /// 异常：密码、身份、格式、数据库或生命周期锁等开库错误传播。
    /// 注意事项：缓存刷新失败在包装函数记录后被此入口忽略；本机位置冲突也不能仅凭 open_db 成功排除。底层可缓存连接和受保护的密码副本，不等于返回后销毁所有密码副本。
    pub(crate) fn open_db(&self, password: &str) -> Result<ContainerDb> {
        let db = ContainerDb::open(&self.veil_dir, password, self.expected_veil_id.as_deref())?;
        // 展示缓存更新失败不影响独立容器解锁，也不能替代容器业务状态。
        let _ = self.refresh_local_cache(&db);
        Ok(db)
    }
    /// 执行本机缓存刷新并统一记录失败告警。
    /// 入参：self 提供实际目录；db（&ContainerDb）为已解锁来源。
    /// 返回值：原样返回内部 `Result<(), VeilError>`。
    /// 异常：内部位置/缓存/数据库或文件系统错误记录告警后仍返回 Err。
    /// 注意事项：本函数没有吞掉错误；open_db 或 rename_container 等调用点自行忽略。日志不构成登记成功证明。
    fn refresh_local_cache(&self, db: &ContainerDb) -> Result<()> {
        let result = self.refresh_local_cache_inner(db);
        if let Err(error) = &result {
            veil_core::logging::failure(
                veil_core::logging::Level::Warn,
                "service",
                "refresh_local_cache",
                "REFRESH_LOCAL_CACHE_FAILED",
                error,
                &[("database", veil_core::logging::Field::Path(&self.veil_dir))],
            );
        }
        result
    }
    /// 将已登记且位置一致的容器真实名称、创建时间与最近访问同步到应用库。
    /// 入参：self 提供当前目录；db 为读取身份/名称/创建时间的容器库。
    /// 返回值：更新完成为 Ok(())；应用库文件不存在或容器未登记时跳过成功。
    /// 异常：本机登记位置不一致为 VeilIdConflict；路径、未绑定空间、时间转换、JSON或 SQL 等错误传播。
    /// 注意事项：登记名称、身份缓存、最近记录分步写入，不保证整体原子；前一步成功后后一步仍可能失败。仅操作已登记容器，不自动接管未登记副本。
    fn refresh_local_cache_inner(&self, db: &ContainerDb) -> Result<()> {
        use crate::storage::app::{app_data_dir, AppStore, RecentContainer};
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
    /// 构造访问本容器租约表的管理器。
    /// 入参：password（&str）为开库所需密码，内部复制到受零化包装存储。
    /// 返回值：LockManager，采用本 VeilManager 的等待/取消策略。
    /// 异常：无错误返回，不在此验密码。
    /// 注意事项：构造不申请锁，密码副本可随守卫和续租线程保留。
    pub(crate) fn lock_manager(&self, password: &str) -> LockManager {
        LockManager::new(&self.veil_dir, password).with_options(self.lock_options.clone())
    }
    /// 为新空容器生成随机身份并初始化加密数据库。
    /// 入参：name（&str）为展示名；password（&str）为非空密码；目录来自 self。
    /// 返回值：新 MetaData，含容器身份、空文件树和活动数据密钥。
    /// 异常：参数、目标库占用、密码/格式/初始化/数据库/I/O 错误返回 VeilError。
    /// 注意事项：生成的新 ID 若与已绑定 expected_veil_id 不同会被拒绝；仅创建容器数据，不在这里生成链接或应用库登记，返回含解密密钥须保密。
    pub fn init_veil(&self, name: &str, password: &str) -> Result<MetaData> {
        self.init_veil_with_id(&crate::metadata::generate_veil_id(), name, password)
    }
    /// 按指定稳定身份初始化新的加密容器数据。
    /// 入参：identity（&str）为合法容器 UUID，有预期身份时须相同；name（&str）为非空白展示名；password（&str）为非空数据库密码。
    /// 返回值：创建成功返回含密钥的 MetaData。
    /// 异常：预期身份不符为 VeilIdConflict；其余创建/参数/数据库/文件错误传播。
    /// 注意事项：不是按旧ID恢复旧内容；生成新的空数据与密钥。目标已存在加密库不可覆盖，失败可能留下目录或库文件。
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
    /// 读取一个事务内一致的容器身份、目录、文件引用与密钥快照。
    /// 入参：password（&str）为容器密码。
    /// 返回值：返回 MetaData，files 每项为一个文件、directories 为目录、data_keys 含解密密钥材料。
    /// 异常：开库、BEGIN、元数据读取或 ROLLBACK 失败返回错误；结束读事务失败会优先覆盖原读取结果。
    /// 注意事项：不申请内容资源租约，数据库读事务只保护表快照，不固定独立密文文件；结果不跨请求缓存，敏感密钥由调用方妥善持有。
    pub fn read_meta(&self, password: &str) -> Result<MetaData> {
        let db = self.open_db(password)?;
        // 单个读事务保证目录树、文件引用、密钥集合属于同一个快照。
        db.conn.execute_batch("BEGIN DEFERRED")?;
        let result = db.metadata();
        db.conn.execute_batch("ROLLBACK")?;
        result
    }
    /// 在容器共享读租约下取得解密元数据与加密数据库主文件字节。
    /// 入参：password（&str）为容器密码。
    /// 返回值：(MetaData, `Vec<u8>`)，第二项是 veil.db 原始加密文件，不是JSON、不含独立.enc。
    /// 异常：等锁、凭证、开库、元数据或主文件读取失败返回错误。
    /// 注意事项：读前后都验租约，约束遵守协议的内容写入；非操作系统瞬时快照，不阻止租约表自身续期。整份数据库读入内存，不可当作完整备份。
    pub fn read_meta_snapshot(&self, password: &str) -> Result<(MetaData, Vec<u8>)> {
        // 这是元数据快照，不是容器备份：只需在共享读租约内读取数据库文件。
        let guard = self
            .lock_manager(password)
            .acquire_container(LockMode::Read)?;
        let db = self.open_db(password)?;
        let credentials = guard.credentials()?;
        locks::validate_many(
            &db.conn,
            &credentials
                .iter()
                .map(|credential| (credential, Some(LockMode::Read)))
                .collect::<Vec<_>>(),
        )?;
        let metadata = db.metadata()?;
        let database = fs::read(self.veil_dir.join(veil_core::database::CONTAINER_DB))?;
        let credentials = guard.credentials()?;
        locks::validate_many(
            &db.conn,
            &credentials
                .iter()
                .map(|credential| (credential, Some(LockMode::Read)))
                .collect::<Vec<_>>(),
        )?;
        Ok((metadata, database))
    }
    /// 兼容旧的元数据回调接口，只把回调给出的名称应用到容器。
    /// 入参：password 为容器密码；update（FnOnce(&mut MetaData)）接收可修改的敏感元数据副本，业务上仅应改 veil_name。
    /// 返回值：受检查字段未被改动且名称写入成功返回 Ok(())。
    /// 异常：容器 ID、创建时间、文件数量/序列化内容或数据密钥变化时拒绝；开库/序列化比较相关及名称写入错误传播，回调 panic 不捕获。
    /// 注意事项：并未比对 directories，修改该字段不会写库也不一定报错；读取与改名非同一事务，不能宣称所有其他字段都校验不变。元数据及其 clone 含密钥副本。
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
    /// 更新数据库中的容器展示名，并尽力刷新本机展示缓存。
    /// 入参：name（&str）须 trim 后非空，但按原文保存；password（&str）为容器密码。
    /// 返回值：UPDATE 执行成功返回 Ok(())，不改变容器和文件身份。
    /// 异常：空白名称或开库/UPDATE失败返回错误。
    /// 注意事项：本方法不申请资源租约，也不在此校验100字符或路径分隔符；缓存刷新失败仅记日志，不撤销权威名称更新。
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
    /// 取得所有文件的扁平展示清单。
    /// 入参：password（&str）为容器密码。
    /// 返回值：`Vec<FileEntry>`，每项一个文件的身份、路径、密文名、大小及版本等信息，无文件为空。
    /// 异常：read_meta 的开库/快照读取/结束事务错误传播。
    /// 注意事项：通过完整元数据快照取文件列表，非分页；不读取或认证全部密文正文。
    pub fn list_files(&self, password: &str) -> Result<Vec<FileEntry>> {
        Ok(self.read_meta(password)?.files.clone())
    }
    /// 按偏移分页列出一个容器目录的直接子节点。
    /// 入参：parent（&str）为内部目录路径，根为空；offset（u32）为跳过节点数；limit（u32）为最多条数；password 为容器密码。
    /// 返回值：`Vec<Node>`，包含文件与子目录，不递归；不足一页仅返回实际条目。
    /// 异常：开库、路径解析/父类型或数据库查询错误传播。
    /// 注意事项：分页调用间没有固定快照，目录变化可能改变页边界；列表不代替后续写入的租约/版本校验。
    pub fn list_children(
        &self,
        parent: &str,
        offset: u32,
        limit: u32,
        password: &str,
    ) -> Result<Vec<Node>> {
        self.open_db(password)?.children(parent, offset, limit)
    }
    /// 在容器逻辑树内补建缺失的目标及中间目录。
    /// 入参：path（&str）为内部目录路径；password 为容器密码。
    /// 返回值：成功返回最终目录节点 ID（String），不是完整路径。
    /// 异常：路径无效、父链被文件占用、目录/容器范围租约冲突或数据库事务失败返回错误。
    /// 注意事项：在即时写事务检查最深已有祖先的结构修改权限再创建；不单独持有新目录的持续租约。已存在目标也走范围可用性检查。
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
    /// 建立导入目标目录并统一申请共享写租约，之后复核路径身份。
    /// 入参：paths（&[String]）每项为规范化内部目录路径，根为空；password 为容器密码；cancellation（`Arc<AtomicBool>`）替换等锁取消信号。
    /// 返回值：(LeaseGuard, `Vec<LockedDirectory>`)，描述列表按输入顺序，租约资源内部去重。
    /// 异常：空集合、目标是文件、创建/等锁/取消/开库失败或加锁期间目录身份变化返回错误。
    /// 注意事项：缺失目录会在申请整组锁前分次创建，失败不回滚已建目录；循环前置取消依赖线程progress上下文，传入原子标志主要用于申请等待。
    pub(crate) fn lock_import_directories(
        &self,
        paths: &[String],
        password: &str,
        cancellation: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(LeaseGuard, Vec<LockedDirectory>)> {
        // 结果容器：每个目标目录的 (规范化路径, 节点 ID) 配对。
        let mut directories = Vec::new();
        for path in paths {
            // 每个目标目录逐一遍历：先响应取消信号，再解析/补建目录。
            veil_core::progress::check_cancelled()?;
            let db = self.open_db(password)?;
            // 解析该目录：已存在且是目录 → 直接用；是文件 → 报错；
            // 不存在但路径非空 → 顺带创建目录链；根目录缺失 → 报错。
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
    /// 规范化并查询容器内部路径的当前类型。
    /// 入参：path（&str）为查询路径，“.”、“/”或空统一为根；password 为容器密码。
    /// 返回值：ContainerPathKind::File/Directory/Missing，均带规范化路径。
    /// 异常：开库、路径非法或节点查询失败返回错误。
    /// 注意事项：不申请资源租约，仅查询时快照，不保证后续对象仍存在。
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
    /// 将一个本地文件按原文件名写入容器根目录。
    /// 入参：path（&Path）为本地文件路径；password（&str）为容器密码。
    /// 返回值：返回写入后的随机 .enc 文件名 String。
    /// 异常：文件名无法转UTF-8或批量添加流程中的路径/源文件/密码/租约/加密/事务错误传播。
    /// 注意事项：默认允许覆盖根目录同名文件，不带编辑基础版本；保留旧文件身份并按内容变化更新版本。
    pub fn add_file(&self, path: &Path, password: &str) -> Result<String> {
        let target = path
            .file_name()
            .and_then(|p| p.to_str())
            .ok_or_else(|| invalid("源文件名无效"))?;
        Ok(self
            .add_files(&[AddFileSpec::new(path, target)], password)?
            .remove(0))
    }
    /// 加密一组本地文件，并在最后一个事务中统一更新文件引用。
    /// 入参：files（&[AddFileSpec]）每项含来源、内部目标、基础版本和覆盖限制；password 为容器密码。
    /// 返回值：`Vec<String>` 按请求顺序返回各随机密文名。
    /// 异常：目标/源/版本冲突、密码/锁、取消、加密、数据库/I/O 或无法确认提交结果返回 VeilError。
    /// 注意事项：准备阶段会先提交缺失目录创建，最终文件引用统一提交；失败可能留下空目录，提交不确定时保留密文供复核。普通路径自行申请 SharedWrite 租约，并非每个文件独立提交。
    pub fn add_files(&self, files: &[AddFileSpec], password: &str) -> Result<Vec<String>> {
        self.add_files_inner(files, password, true, None, None, None, None)
    }
    /// 复用编辑任务已持有的单文件共享写租约保存内容。
    /// 入参：file（&AddFileSpec）为恰好一个已有文件的替换请求；password 为容器密码；guard（&LeaseGuard）须含该文件 SharedWrite 凭证。
    /// 返回值：成功返回新密文名 String。
    /// 异常：目标不存在、守卫不匹配/失效，或底层版本/加密/事务/I/O失败返回错误。
    /// 注意事项：本函数不取得调用方守卫所有权，提交后不替调用方释放共享租约；应提供 base_version 阻止过期保存。
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
    /// 复用既有共享写目录租约，向其后代位置批量写入文件。
    /// 入参：files 为 AddFileSpec 切片；password 为容器密码；directory（&str）为目录逻辑路径，根为空；guard 须含此目录 SharedWrite 凭证。
    /// 返回值：按请求顺序返回密文名 `Vec<String>`。
    /// 异常：目录缺失/非目录、无匹配共享写凭证、范围越界及底层写入错误传播。
    /// 注意事项：非根目录自身不属于 covers 的可写文件位置，只允许后代；该方法不消费/释放外部守卫，仍需遵守版本与覆盖限制。
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
    /// 为测试复用常规添加路径，传入保留的兼容清理参数。
    /// 入参：files 为待添加请求切片；password 为容器密码。
    /// 返回值：返回与请求对应的密文名列表。
    /// 异常：与 add_files_inner 相同的业务/数据库/I/O错误传播。
    /// 注意事项：仅 cfg(test)；_cleanup 当前未被读取，false 不会禁用 CancelledFiles 清理，不能依名称推断取消后文件一定保留。
    pub(crate) fn add_files_without_cleanup(
        &self,
        files: &[AddFileSpec],
        password: &str,
    ) -> Result<Vec<String>> {
        self.add_files_inner(files, password, false, None, None, None, None)
    }
    /// 测试专用：在已持有的目录租约下向指定目录批量写入。
    #[cfg(test)]
    /// 为测试复用已锁目录的添加路径。
    /// 入参：files 为请求切片；password 为容器密码；guard 为目录租约；directory（&LockedDirectory）绑定路径与节点身份。
    /// 返回值：成功返回密文名列表。
    /// 异常：范围/凭证/版本、加密/数据库/I/O错误传播。
    /// 注意事项：仅 cfg(test)；传入的 _cleanup=false 当前无行为差异，正常清理守卫仍生效。
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
    /// 为导入流水线准备密文，并把待提交引用及租约移交完成队列。
    /// 入参：files 为本轮请求切片；password 为容器密码；directory_guard 为可复用目录守卫/范围，None自行持文件锁；transaction_gate（&`Mutex<()>`）串行化准备与提交；batch（&ImportBatchContext）含共享目录缓存、完成发送端和持有者身份。
    /// 返回值：入队成功后返回本批密文名 `Vec<String>`，不表示文件引用已提交。
    /// 异常：参数/源/版本/租约/加密/准备事务失败、队列关闭或取消返回 VeilError。
    /// 注意事项：完成队列有界，发送可能等待；准备中的目录创建已单独提交，文件引用由后续 commit_import_batch 统一提交。外部目录守卫须和 batch.directory_guard 对应。
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
    /// 统一执行目录与文件准备、申请/复用租约、加密发布及文件引用提交或排队。
    /// 入参：files（&[AddFileSpec]）每项一份写入请求；password 为数据库密码；_cleanup（bool）为保留兼容参数，当前未使用；directory_guard 为可选目录租约/范围；transaction_gate 为可选准备/提交门锁；file_guard 为可选已有单文件共享写守卫；batch 为可选流水线上下文，Some 延后文件引用提交。
    /// 返回值：按请求顺序返回密文名；batch=None 时引用已提交或重新确认，Some 时仅保证待提交项成功入队。
    /// 异常：重复/越界目标、源非文件、类型/版本/锁冲突、取消、加密/文件/数据库/队列错误或 CommitUncertain；活动密钥等内部不变量破坏可触发 unwrap panic。
    /// 注意事项：准备事务先创建目录并提交，再在事务外加密；新密文不覆盖发布，随后最终事务验证活动密钥/父节点/版本/租约。顺序为门锁→目录缓存锁→数据库事务，取凭证避开写事务以减少锁顺序反转。提交不确定先保留密文再重读确认；失败不能承诺所有磁盘/目录变更回滚。
    fn add_files_inner(
        &self,
        files: &[AddFileSpec],
        password: &str,
        _cleanup: bool,
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
        // 顺序固定为事务门锁、目录缓存锁、数据库事务；仅在目录租约覆盖时复用节点。
        let mut directory_cache = batch
            .filter(|_| directory_guard.is_some())
            .map(|context| {
                context
                    .directories
                    .container
                    .lock()
                    .map_err(|_| invalid("目录缓存不可用"))
            })
            .transpose()?;
        let mut new_directories = HashMap::new();
        let tx = db
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let key_ring = container::keys(&tx)?;
        let key = key_ring
            .clone()
            .into_iter()
            .find(|k| k.status == "active")
            .unwrap();
        let normalized = files
            .iter()
            .map(|file| normalize_veil_dir(&file.target))
            .collect::<Result<Vec<_>>>()?;
        let mut targets = HashSet::new();
        for target in &normalized {
            // 逐一登记规范化后的目标路径，重复路径说明请求本身冲突，直接拒绝。
            if !targets.insert(target.clone()) {
                return Err(invalid("批量目标重复"));
            }
        }
        let mut resolved = container::resolve_batch(&tx, &normalized)?;
        let mut parents = HashMap::<String, Node>::new();
        let mut pending = Vec::new();
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
        }
        // 逐请求准备：确定父目录、校验版本、分配文件 ID 与密钥/nonce，
        // 全部准备完后先提交目录创建；加密在事务外进行，最终提交前仍会重新开库验证。
        for (spec, target) in files.iter().zip(normalized) {
            // 目录锁只覆盖其子树；目标路径越界说明调用方配错了锁。
            if directory_guard.is_some_and(|(_, directory)| !directory.covers(&target)) {
                return Err(invalid("操作超出目录锁范围"));
            }
            if !spec.source.is_file() {
                return Err(invalid("源路径必须是文件"));
            }
            let (parent_path, name) = split_parent(&target);
            let parent = match parents.get(parent_path) {
                Some(parent) => parent.clone(),
                None => {
                    if directory_credential.is_none() {
                        let ancestor = deepest_directory(&tx, parent_path)?;
                        locks::ensure_parent_available(&tx, &ancestor.id)?;
                    }
                    let parent = if let Some(known) = directory_cache.as_deref() {
                        container::ensure_directory_cached(
                            &tx,
                            parent_path,
                            known,
                            &mut new_directories,
                        )?
                    } else {
                        container::ensure_directory(&tx, parent_path)?
                    };
                    parents.insert(parent_path.to_owned(), parent.clone());
                    parent
                }
            };
            let (node, base_hash, exact) = resolved
                .remove(&target)
                .ok_or_else(|| invalid("批量查询缺少目标路径"))?;
            if !exact && node.kind != "directory" {
                return Err(VeilError::FileAlreadyExists(target));
            }
            let base = exact.then_some(node);
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
                base_hash,
                base,
                identity,
                key: key.clone(),
                nonce: file_ops::generate_base_nonce(),
                cipher: String::new(),
                size: 0,
                hash: String::new(),
                unchanged: false,
            });
        }
        tx.commit()?;
        if let Some(known) = directory_cache.as_deref_mut() {
            known.extend(new_directories);
        }
        drop(directory_cache);
        drop(prepare_gate);
        drop(db);
        let own_guard = if directory_guard.is_none() && file_guard.is_none() {
            // 没有复用租约时，为每个文件申请带父目录信息的预留文件 SharedWrite 租约。
            // 父目录信息用于与目录范围比重叠，不是额外给每个父目录加一把目录锁。
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
        // 取出本次写入全程要用的租约凭证集合，后续加密与提交阶段共用。
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
            // 重新打开数据库：等锁期间别的进程可能已改写这些文件。
            let current = self.open_db(password)?;
            // 收集所有"覆盖已有文件"的请求身份，用于批量重读最新节点。
            let identities = pending
                .iter()
                .filter(|file| file.base.is_some())
                .map(|file| file.identity.clone())
                .collect::<Vec<_>>();
            let nodes = container::nodes_batch(&current.conn, &identities)?;
            for p in &pending {
                if let Some(base) = &p.base {
                    if nodes.get(&p.identity) != Some(base) {
                        return Err(VeilError::VersionConflict("文件在申请锁期间已变化".into()));
                    }
                }
            }
        }
        // 逐文件加密落盘：先写暂存区，再发布为随机命名 .enc 密文。
        for p in &mut pending {
            // 通知进度系统：开始处理这个目标文件。
            veil_core::progress::begin_file(&p.spec.target);
            // 从凭证集合中挑出覆盖当前文件的凭证（目录锁按目录 ID，文件锁按文件 ID）。
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
            // 租约专属的暂存根目录：.staging/<lock_id>，与密文名中的 lock_id 对应。
            let lease_staging = self.veil_dir.join(".staging").join(&credential.lock_id);
            // 同一目录租约由多个工作线程共享；各文件使用自己的子目录。
            let staging = if directory_guard.is_some() || file_guard.is_some() {
                lease_staging.join(id())
            } else {
                lease_staging.clone()
            };
            if let Some(context) = batch {
                // 只缓存共享暂存目录，不为每个文件的唯一临时子目录保留缓存项。
                context.directories.ensure_filesystem(&lease_staging)?;
                if staging != lease_staging {
                    crate::fsutil::create_dir_all_durable(&staging)?;
                }
            } else {
                crate::fsutil::create_dir_all_durable(&staging)?;
            }
            cancelled_files.staging.push(staging.clone());
            // 密文名用租约ID加随机UUID，不包含原文件名；随机降低碰撞概率，真正发布仍须拒绝覆盖。
            p.cipher = format!("{}_{}.enc", credential.lock_id, id());
            let staged = staging.join(&p.cipher);
            cancelled_files.files.push(staged.clone());
            fault("before_cipher_write");
            // 流式加密：明文分块 ChaCha20-Poly1305，哈希一并计算，不整体进内存。
            let (size, hash) = file_ops::encrypt_staged_file_with_hash(
                &p.spec.source,
                &staged,
                &*p.key.material()?,
                &p.nonce,
            )?;
            p.size = size;
            p.hash = hash.to_hex().to_string();
            fault("after_cipher_write");
            // 不覆盖地发布暂存密文到容器目录；不支持原子重命名的平台可回退为排他创建并复制。
            // 此刻文件引用尚未提交，发布失败仍可能有需后续核对的磁盘结果。
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
            // 覆盖已有文件且大小相同时，比对哈希判断内容是否真的变了；
            // 相同内容不追加版本（unchanged=true），避免无意义版本膨胀。
            if let Some(base) = &p.base {
                if base.size == Some(p.size) {
                    // 旧哈希优先取缓存（file_hash 表）；没有就现场解密旧密文计算。
                    let old_hash = match p.base_hash.as_deref() {
                        Some(hash) => hash.to_owned(),
                        None => file_ops::plaintext_digest(
                            &self.veil_dir.join(base.cipher()?),
                            &*p.base_key.as_ref().unwrap().material()?,
                            &base.nonce()?,
                            p.size,
                        )?
                        .to_hex()
                        .to_string(),
                    };
                    // 旧文件的版本快照：容器身份 + 文件 ID + 内容版本链。
                    let version = VersionSnapshot {
                        veil_id: veil_id.clone(),
                        file_id: base.id.clone(),
                        version_history: base.versions()?.to_vec(),
                    };
                    use crate::comparison::{
                        compare_files, ComparisonChecks, ComparisonState, FileInput,
                    };
                    // 只按哈希比较新旧内容：相同则 unchanged=true，提交时不追加版本。
                    p.unchanged = compare_files(
                        FileInput::ContainerMetadata {
                            version: &version,
                            hash: Some(&old_hash),
                        },
                        FileInput::ContainerMetadata {
                            version: &version,
                            hash: Some(&p.hash),
                        },
                        ComparisonChecks {
                            hash: true,
                            ..Default::default()
                        },
                    )?
                    .hash
                        == Some(ComparisonState::Same);
                }
            }
        }
        veil_core::progress::check_cancelled()?;
        if let Some(batch) = batch {
            // 导入管线路径：加密结果不在此提交，而是打包入队交给批量提交线程。
            let batch_guard = match &own_guard {
                Some(guard) => Arc::clone(guard),
                None => batch
                    .directory_guard
                    .as_ref()
                    .cloned()
                    .ok_or_else(|| invalid("导入锁凭证缺失"))?,
            };
            let ciphers = pending.iter().map(|file| file.cipher.clone()).collect();
            // 队列项：待提交的 Pending 列表 + 租约守卫 + 清理守卫。
            let queued = QueuedImport {
                pending,
                guard: batch_guard,
                directory_id: directory_guard.map(|(_, directory)| directory.id.clone()),
                veil_id,
                release_owned: own_guard.is_some(),
                cancelled_files,
            };
            let count = queued.pending.len();
            for _ in 0..count {
                // 进度计数：文件进入提交队列。
                veil_core::progress::file_queued();
            }
            // 入队：发送失败（队列关闭或取消）时回退进度计数并报错。
            if let Err(error) = batch
                .sender
                .send_with(queued, || veil_core::progress::check_cancelled().is_err())
            {
                for _ in 0..count {
                    veil_core::progress::file_dequeued();
                }
                return Err(match error {
                    veil_core::queue::SendError::Cancelled(_) => VeilError::Cancelled,
                    _ => invalid("导入提交队列已关闭"),
                });
            }
            return Ok(ciphers);
        }
        // 重新打开可识别其他进程改密；旧密码连接不能提交较早启动的写入。
        let credentials = guard.credentials()?;
        let mut db = self.open_db(password)?;
        // 身份核对：写密文期间目录可能已被替换成别的容器。
        if db.info()?.0 != veil_id {
            return Err(VeilError::VeilIdConflict("写入期间容器身份已变化".into()));
        }
        // 导入场景的提交门锁：保证准备与提交阶段串行，不与批量提交线程交错。
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
                log_commit_confirmation(&self.veil_dir, pending.len(), false);
                return Err(VeilError::CommitUncertain(std::io::Error::other(
                    "无法确认文件写入事务结果，请重新读取容器状态",
                )));
            }
            log_commit_confirmation(&self.veil_dir, pending.len(), true);
        }
        if let Some(guard) = &own_guard {
            guard.released_in_transaction();
        }
        fault("after_metadata_commit");
        // 本次写入的临时目录通常已空；仅尝试删除空目录，不碰其他任务的文件。
        for directory in &cancelled_files.staging {
            let _ = fs::remove_dir(directory);
        }
        drop(own_guard);
        Ok(pending.into_iter().map(|p| p.cipher).collect())
    }
    /// 把多个加密准备项的文件引用合并进一个写事务，验证并释放本批自行持有的租约。
    /// 入参：batch（`Vec<QueuedImport>`）每项含待提交文件、清理守卫和租约；password 为容器密码；stopped（&AtomicBool）为管线停止信号。
    /// 返回值：引用提交成功或重开数据库确认全部新密文引用后返回 Ok(())，更新出队/完成进度。
    /// 异常：取消、身份/密钥/节点/版本/租约/数据库错误传播；逐文件应用失败包装为 ImportBatchFailed；无法确认提交结果返回 CommitUncertain。
    /// 注意事项：失败不都意味着回滚：提交不确定保留密文。只有 release_owned 项在事务内释放并清空守卫，借用目录租约保留；此前准备事务创建的目录不回滚。调用方负责外层事务门锁，函数本身不取得 gate。
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
            // 与batch按下标对应的凭证快照列表；先取凭证Mutex再开写事务，避免与续租数据库等待顺序相反。
            let credentials = batch
                .iter()
                .map(|item| {
                    // 逐队列项取出租约凭证；失败时附上该批文件的展示路径。
                    item.guard
                        .credentials()
                        .map_err(|error| VeilError::ImportBatchFailed {
                            file: item.target().to_owned(),
                            source: Box::new(error),
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
                    let snapshot =
                        ApplySnapshot::load(tx, batch.iter().flat_map(|item| item.pending.iter()))?;
                    // 所有待验证的（凭证、模式），目录共用时可重复；owned仅含本批负责释放的凭证引用。
                    let mut requested = Vec::new();
                    let mut owned = Vec::new();
                    for (item, credentials) in batch.iter().zip(&credentials) {
                        // 为每个队列项挑出覆盖其文件的租约凭证；自行持锁的项
                        // 同时登记到 owned，待提交成功后一并释放。
                        let group = Self::required_credentials(
                            &item.pending,
                            credentials,
                            item.directory_id.as_deref(),
                        )?;
                        if item.release_owned {
                            owned.extend(group.iter().map(|(credential, _)| *credential));
                        }
                        requested.extend(group);
                    }
                    locks::validate_many(tx, &requested)?;
                    for item in &batch {
                        for file in &item.pending {
                            // 逐文件应用提交（基于快照），失败时附上该文件路径。
                            Self::apply_pending_cached(
                                tx,
                                std::slice::from_ref(file),
                                &identity,
                                &snapshot,
                            )
                            .map_err(|error| {
                                VeilError::ImportBatchFailed {
                                    file: file.spec.target.clone(),
                                    source: Box::new(error),
                                }
                            })?;
                        }
                    }
                    locks::validate_many(tx, &requested)?;
                    locks::release_validated_many(tx, &owned)?;
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
                    let files = batch.iter().map(|item| item.pending.len()).sum();
                    if !matches!(confirmed, Ok(true)) {
                        log_commit_confirmation(&self.veil_dir, files, false);
                        return Err(VeilError::CommitUncertain(error));
                    }
                    log_commit_confirmation(&self.veil_dir, files, true);
                }
                other => other?,
            }
            for item in &batch {
                if item.release_owned {
                    // 自行申请的租约已在提交事务内释放，标记守卫避免重复释放。
                    item.guard.released_in_transaction();
                }
            }
            fault("after_metadata_commit");
            Ok(())
        })();
        for item in &batch {
            for _ in &item.pending {
                veil_core::progress::file_dequeued();
            }
            if result.is_ok() {
                for directory in &item.cancelled_files.staging {
                    let _ = fs::remove_dir(directory);
                }
                for _ in &item.pending {
                    veil_core::progress::file_done(false);
                }
            }
        }
        result
    }

    /// 丢弃尚未提交的导入准备结果并回退排队进度。
    /// 入参：batch（`Vec<QueuedImport>`）为本次要放弃的完成队列项，每项可含多份文件。
    /// 返回值：无；逐文件标记出队后释放整批所有权。
    /// 异常：无错误返回；密文/空目录删除失败被守卫忽略。
    /// 注意事项：不撤销此前已提交目录，不回滚其他批次；共享守卫仅减少Arc引用，自持守卫最后释放时可能访问数据库归还租约，因此不是完全不访问数据库。
    pub(crate) fn cancel_import_batch(batch: Vec<QueuedImport>) {
        for item in &batch {
            for _ in &item.pending {
                veil_core::progress::file_dequeued();
            }
        }
        drop(batch);
    }

    /// 在已有事务中核验准备材料、更新文件引用，并按需归还租约。
    /// 入参：tx（&Transaction）为写事务；pending（&[Pending]）每项为一份准备文件；credentials 为可用凭证切片；directory_id 为可选共用目录节点ID；release_owned（bool）决定是否删除凭证对应租约行；veil_id 为期望容器ID。
    /// 返回值：全部文件应用和要求的租约释放语句执行成功为 Ok(())，尚未提交事务。
    /// 异常：快照/凭证缺失、模式/期限/密钥/版本冲突或SQL错误传播。
    /// 注意事项：调用方负责提交/回滚；两次验租约也检查耗时/时钟变化，不应描述成写事务期间其他写事务能任意撤销租约。
    fn apply_pending(
        tx: &rusqlite::Transaction<'_>,
        pending: &[Pending],
        credentials: &[locks::Credential],
        directory_id: Option<&str>,
        release_owned: bool,
        veil_id: &str,
    ) -> Result<()> {
        let snapshot = ApplySnapshot::load(tx, pending)?;
        let requested = Self::required_credentials(pending, credentials, directory_id)?;
        locks::validate_many(tx, &requested)?;
        Self::apply_pending_cached(tx, pending, veil_id, &snapshot)?;
        locks::validate_many(tx, &requested)?;
        if release_owned {
            locks::release_validated_many(
                tx,
                &requested
                    .iter()
                    .map(|(credential, _)| *credential)
                    .collect::<Vec<_>>(),
            )?;
        }
        Ok(())
    }
    /// 为每份准备文件找到覆盖它的凭证和预期写模式。
    /// 入参：pending 为待写文件切片；credentials（&[Credential]）为守卫快照，借用寿命覆盖返回值；directory_id 为共用目录ID，None按文件identity寻找。
    /// 返回值：`Vec<(&Credential, Option<LockMode>)>`，每项对应一个文件，模式为SharedWrite或Write；共目录项可能重复引用同张凭证。
    /// 异常：未找到对应目录/文件凭证返回参数错误。
    /// 注意事项：本函数仅匹配资源编码，不验证有效期/令牌或目录路径覆盖；后续 validate_many 完成数据库验证。
    fn required_credentials<'a>(
        pending: &[Pending],
        credentials: &'a [locks::Credential],
        directory_id: Option<&str>,
    ) -> Result<Vec<(&'a locks::Credential, Option<LockMode>)>> {
        pending
            .iter()
            .map(|file| {
                let credential = credentials
                    .iter()
                    .find(|credential| match directory_id {
                        Some(directory) => credential.directory_id() == Some(directory),
                        None => credential.file_id() == Some(file.identity.as_str()),
                    })
                    .ok_or_else(|| invalid("锁凭证缺失"))?;
                Ok((
                    credential,
                    Some(if credential.is_shared_write() {
                        LockMode::SharedWrite
                    } else {
                        LockMode::Write
                    }),
                ))
            })
            .collect()
    }
    /// 在同一提交快照下，把新密文接入文件节点并更新哈希。
    /// 入参：tx 为当前写事务；pending 为准备文件切片；veil_id 为容器身份；snapshot 为事务内装载的活动密钥ID和父/文件节点映射。
    /// 返回值：所有INSERT/UPDATE及file_hash写入成功为 Ok(())，由外层提交。
    /// 异常：活动密钥变化、父目录无效、位置/身份/版本变化、大小越界或SQL失败返回错误；新建唯一约束冲突映射FileAlreadyExists。
    /// 注意事项：不重新加密，不在此验租约；快照必须由调用方在同一事务准备。旧密文名仅收集在局部obsolete且未被返回或删除，实际残留依赖后续垃圾扫描。内容未变保留版本/更新时间，但仍换密文引用和加密时间。
    fn apply_pending_cached(
        tx: &rusqlite::Transaction<'_>,
        pending: &[Pending],
        veil_id: &str,
        snapshot: &ApplySnapshot,
    ) -> Result<()> {
        let active_key_id = &snapshot.active_key_id;
        // 旧密文名列表：覆盖时记录失引用名称；当前未返回/使用此集合，后续须另行垃圾扫描回收。
        let mut obsolete = Vec::new();
        for p in pending {
            // 逐文件应用提交：校验活动密钥、父目录与版本后更新引用。
            if &p.key.key_id != active_key_id {
                return Err(invalid("活动文件密钥已变化，请重试"));
            }
            let parent = snapshot
                .nodes
                .get(&p.parent)
                .ok_or_else(|| invalid("父目录已删除"))?;
            if parent.kind != "directory" {
                return Err(invalid("父节点不是目录"));
            }
            // current：该文件身份的当前节点（快照版），用于覆盖场景的版本复核。
            let current = snapshot.nodes.get(&p.identity);
            let mut versions = if let Some(base) = &p.base {
                // 覆盖场景：文件必须还在原位（同父、同名），版本链仍与读取时一致。
                let current =
                    current.ok_or_else(|| VeilError::VersionConflict("文件已删除".into()))?;
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
                // 版本链从旧节点继承，改名/移动/重加密都不改变版本链。
                current.versions()?.to_vec()
            } else {
                // 新建场景：该文件身份必须尚未被占用，否则说明目标被并发创建。
                if current.is_some() {
                    return Err(VeilError::VersionConflict("文件身份已被占用".into()));
                }
                Vec::new()
            };
            // 内容有变化才追加新版本 UUID；相同内容覆盖不产生新版本。
            if !p.unchanged {
                versions.push(crate::metadata::generate_version_id());
            }
            let size = i64::try_from(p.size).map_err(|_| invalid("文件过大"))?;
            if let Some(base) = &p.base {
                // 覆盖场景：更新当前密文引用、明文大小、密钥、nonce、
                // 加密时间与版本链；updated_at 在内容未变时沿用旧时间。
                let updated = if p.unchanged { base.updated_at } else { now() };
                tx.execute("UPDATE file_info SET cipher_name=?1,size=?2,key_id=?3,nonce=?4,encrypted_at=?5,updated_at=?8,version_history=?6 WHERE id=?7",params![p.cipher,size,active_key_id,p.nonce.as_slice(),now(),json(&versions)?,p.identity,updated])?;
            } else {
                // 新建场景：插入 file_info 文件行（kind='file'，created_at=updated_at=now）。
                tx.execute(
                    "INSERT INTO file_info VALUES(?1,?2,?3,'file',?4,?4,?5,?6,?7,?8,?4,?9)",
                    params![
                        p.identity,
                        p.parent,
                        p.name,
                        now(),
                        p.cipher,
                        size,
                        active_key_id,
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
            // 哈希表按文件 ID 幂等写入 BLAKE3；覆盖时更新为新内容哈希。
            tx.execute("INSERT INTO file_hash(file_id,blake3) VALUES(?1,?2) ON CONFLICT(file_id) DO UPDATE SET blake3=excluded.blake3", params![p.identity, p.hash])?;
        }
        Ok(())
    }
    /// 解析文件身份、申请指定模式租约后重新开库读取当前节点和解密密钥。
    /// 入参：name（&str）为容器文件路径；password 为数据库密码；mode（LockMode）为读取所需模式，导出SharedWrite、普通预览ReadOnly、稳定比较Read。
    /// 返回值：(ContainerDb, Node, LeaseGuard, `Zeroizing<[u8;32]>`)：已开库、加锁后重读节点、守卫及密钥副本。
    /// 异常：文件不存在、密码/容器身份变化、租约/凭证/密钥或数据库失败返回错误。
    /// 注意事项：等锁后按原节点ID重读，并不要求节点与等锁前所有字段相同。ReadOnly不建租约行、不能阻止写；调用方读取/发布前后仍按模式验证。
    pub(crate) fn read_locked(
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

    /// 将单个容器文件解密发布到本地，允许覆盖已有输出。
    /// 入参：name（&str）为内部文件路径；output（&Path）为精确本地文件位置；password 为容器密码。
    /// 返回值：完成发布、目录同步和读事务结束返回 Ok(())。
    /// 异常：源/密钥/租约、取消、认证/解密、目录/发布/同步/数据库失败返回错误。
    /// 注意事项：采用SharedWrite租约；输出无Stamp确认。发布后同步或事务结束失败也会返回Err，目标可能已经更新，不能解释为无写入。
    pub fn extract_file(&self, name: &str, output: &Path, password: &str) -> Result<()> {
        self.extract_file_with_version(name, output, password)
            .map(|_| ())
    }
    /// 导出文件并返回编辑保存可绑定的基础版本。
    /// 入参：name 为内部文件路径；output 为精确输出文件，允许覆盖；password 为容器密码。
    /// 返回值：VersionSnapshot，包含读取时容器ID、稳定文件ID和内容版本链。
    /// 异常：底层解锁/等锁/认证/文件发布/同步/事务错误传播。
    /// 注意事项：保存回容器时须显式携带快照，返回版本不等于持续独占文件；不自动记录编辑会话。
    pub fn extract_file_with_version(
        &self,
        name: &str,
        output: &Path,
        password: &str,
    ) -> Result<VersionSnapshot> {
        self.extract_file_inner(name, output, password, true)
            .map(|(version, _)| version)
    }
    /// 持SharedWrite租约解密到目标旁暂存文件，校验凭证后按覆盖策略发布。
    /// 入参：name 为容器内部文件路径；output 为本地文件；password 为密码；overwrite（bool）为true使用persist覆盖，false使用persist_noclobber拒绝占用。
    /// 返回值：(VersionSnapshot, bool)，后项表示是否发现ReadOnly标记；当前明确申请SharedWrite且无降级路径，成功时通常为false。
    /// 异常：解锁/执行上下文/租约/取消/认证/读写/同步/读事务错误传播；内部文件size缺失可unwrap panic。
    /// 注意事项：文件发布及父目录同步发生在校验用读事务内，发布后失败不回滚文件。与多文件导出流水线不同，本方法未做输出在容器外/父路径无链接的专项检查；调用方须提供合适目标。
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
    /// 导出单文件且拒绝覆盖已有外部目标。
    /// 入参：name 为内部文件路径；output 为精确本地目标；password 为容器密码。
    /// 返回值：成功发布并完成同步/事务结束返回Ok(())。
    /// 异常：目标存在、平台不支持所用不覆盖发布、租约/密码/认证/文件或数据库错误传播。
    /// 注意事项：当前使用tempfile persist_noclobber，未调用多文件流水线的共享回退发布函数；已发布后同步错误仍可能返回Err。
    pub fn extract_file_without_overwrite(
        &self,
        name: &str,
        output: &Path,
        password: &str,
    ) -> Result<()> {
        self.extract_file_inner(name, output, password, false)
            .map(|_| ())
    }
    /// 不覆盖导出，并透出底层读取模式提示。
    /// 入参：name 为内部文件路径；output 为精确本地目标；password 为容器密码。
    /// 返回值：bool，底层若含ReadOnly凭证则true；当前SharedWrite申请不自动降级，正常成功为false。
    /// 异常：与extract_file_inner相同的业务/文件/数据库错误传播。
    /// 注意事项：false不代表不可发生外部改动或租约永不失效；此值不是认证强度或文件持久性保证。
    pub fn extract_file_without_overwrite_with_status(
        &self,
        name: &str,
        output: &Path,
        password: &str,
    ) -> Result<bool> {
        self.extract_file_inner(name, output, password, false)
            .map(|(_, unstable)| unstable)
    }
    /// 在预览大小限制内解密文件到内存。
    /// 入参：name 为内部文件路径；password 为容器密码；limit（u64）为允许的明文字节上限，等于上限允许读取。
    /// 返回值：`Vec<u8>`为完整认证后的明文字节，空文件可为空。
    /// 异常：超过限制、源/密码/密钥/认证/读取/取消等错误传播。
    /// 注意事项：采用ReadOnly标记，不阻止并发写入，且本包装丢弃unstable提示；返回Vec不自动零化，调用方负责敏感明文生命周期。
    pub fn read_file_bytes(&self, name: &str, password: &str, limit: u64) -> Result<Vec<u8>> {
        self.read_file_bytes_with_status(name, password, limit)
            .map(|(bytes, _)| bytes)
    }
    /// 为预览解密完整字节，并明确提示未持稳定内容租约。
    /// 入参：name 为容器内部文件路径；password 为密码；limit（u64）为明文长度上限。
    /// 返回值：(`Vec<u8>`, bool)，bool在发现ReadOnly标记时为true；当前申请ReadOnly，因此正常读取为true。
    /// 异常：超限、文件/密钥/密码/认证/数据库/取消等错误传播；size缺失可panic，按长度分配还受进程可用内存限制。
    /// 注意事项：ReadOnly无数据库租约行，读后validate对此类标记不检查实际文件引用/期限，不能承诺返回仍是当前版本；明文Vec不零化。
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
    /// 调用通用路径删除并忽略返回类型，随后尽力同步回收密文。
    /// 入参：name 为容器内部路径；password 为容器密码。
    /// 返回值：逻辑删除成功且租约释放完成返回Ok(())，密文删除失败不影响成功。
    /// 异常：路径/类型/锁/数据库等底层错误传播。
    /// 注意事项：名称虽为remove_file，实际可删除目录及后代；无额外文件类型限制，不将物理删除失败反馈给调用方。
    pub fn remove_file(&self, name: &str, password: &str) -> Result<()> {
        self.remove_path(name, password).map(|_| ())
    }
    /// 先移除文件或子树的数据库引用，再逐项尝试删除已失引用密文。
    /// 入参：path 为容器内部目标；password 为容器密码。
    /// 返回值：RemovedPathKind::File或Directory，表示逻辑删除的对象类型。
    /// 异常：逻辑删除、租约释放等错误传播；remove_file失败在此静默忽略。
    /// 注意事项：物理回收不是数据库事务的一部分，也不自动重试；返回成功可能仍有孤儿密文，需后续cleanup。
    pub fn remove_path(&self, path: &str, password: &str) -> Result<RemovedPathKind> {
        let (kind, ciphers) = self.remove_path_deferred(path, password)?;
        for cipher in ciphers {
            let _ = fs::remove_file(cipher);
        }
        Ok(kind)
    }
    /// 删除指定文件/子树的目录项，返回待由调用方回收的密文路径。
    /// 入参：path（&str）为内部路径；password 为容器密码。
    /// 返回值：(RemovedPathKind, `Vec<PathBuf>`)，每条为本容器目录拼接的失引用密文位置，未强制绝对化。
    /// 异常：路径不存在/变化、租约/密码/数据库或随后释放凭证失败返回错误。
    /// 注意事项：子树扫描在写租约下进行；DELETE使用单语句隐式事务，之后release_in在另一次自动提交操作中执行，并非同一事务。释放失败时引用可能已删除且本次不返回待回收列表；空/根路径不允许normalize。
    pub(crate) fn remove_path_deferred(
        &self,
        path: &str,
        password: &str,
    ) -> Result<(RemovedPathKind, Vec<PathBuf>)> {
        let execution = crate::ExecutionService::global()?;
        let _scope = execution.enter_task("删除路径");
        let path = normalize_veil_dir(path)?;
        let db = self.open_db(password)?;
        let node = db
            .resolve(&path)?
            .ok_or_else(|| VeilError::FileNotFound(path.clone()))?;
        drop(db);
        let guard = if node.kind == "directory" {
            self.lock_manager(password)
                .acquire_directory(&node.id, LockMode::Write)?
        } else {
            self.lock_manager(password)
                .acquire(std::slice::from_ref(&node.id), LockMode::Write)?
        };
        let credentials = guard.credentials()?;
        let db = self.open_db(password)?;
        for credential in &credentials {
            locks::validate(&db.conn, credential, Some(LockMode::Write))?;
        }
        let current = db.resolve(&path)?;
        if current.as_ref().map(|current| &current.id) != Some(&node.id) {
            return Err(VeilError::VersionConflict(
                "删除前目标路径已变化，请重试".into(),
            ));
        }
        // 写租约已覆盖目标资源；扫描不需要占用 SQLite 写事务。
        // 子树中每个文件当前引用的密文名；逻辑删除成功后转成待回收磁盘路径。
        let ciphers = db
            .subtree(&node.id)?
            .into_iter()
            .filter(|node| node.kind == "file")
            .map(|node| node.cipher().map(str::to_owned))
            .collect::<Result<Vec<_>>>()?;
        // 单条递归 DELETE 自带 SQLite 隐式事务，无需手工 BEGIN IMMEDIATE。
        // 递归 CTE 从目标节点出发收集整棵子树，一次性删除全部目录项。
        db.conn.execute("WITH RECURSIVE tree(id) AS (SELECT id FROM file_info WHERE id=?1 UNION ALL SELECT f.id FROM file_info f JOIN tree t ON f.parent_id=t.id) DELETE FROM file_info WHERE id IN (SELECT id FROM tree)",[&node.id])?;
        for credential in &credentials {
            locks::release_in(&db.conn, credential)?;
        }
        guard.released_in_transaction();
        drop(db);
        drop(guard);
        Ok((
            if node.kind == "file" {
                RemovedPathKind::File
            } else {
                RemovedPathKind::Directory
            },
            ciphers
                .into_iter()
                .map(|cipher| self.veil_dir.join(cipher))
                .collect(),
        ))
    }
    /// 将容器内部节点移动或改名，忽略返回类型及实际落点。
    /// 入参：from/to（&str）为内部源/目标路径；password 为容器密码。
    /// 返回值：move_path成功返回Ok(())。
    /// 异常：路径/范围锁/目录结构/唯一约束或数据库错误传播。
    /// 注意事项：实际也支持目录，不额外限定文件；不改变磁盘密文名和内容版本。
    pub fn rename_file(&self, from: &str, to: &str, password: &str) -> Result<()> {
        self.move_path(from, to, password).map(|_| ())
    }
    /// 在一个短写事务内改变节点的父目录、名称和更新时间。
    /// 入参：from 为内部源路径；to 为目标路径，已存在目录则落到其下沿用源名；password 为容器密码。
    /// 返回值：(MovedPathKind, String)，后项实际目标路径；源目标同一节点时返回规范化源路径。
    /// 异常：源缺失、目标文件占用、父目录缺失、资源冲突、成环或SQL/提交失败返回错误；最终同级名称唯一冲突可直接表现为数据库错误。
    /// 注意事项：事务内只检查已有范围租约冲突，不建立持续新租约；源目标相同也先检查源冲突。只改当前节点，后代路径由父链推导，版本和密文不变。
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
        // 目录移动检查整个目录范围的已有租约；文件移动检查自身资源，不在此新建租约行。
        let source_resource = if n.kind == "directory" {
            locks::directory_resource(&n.id)
        } else {
            n.id.clone()
        };
        // 短事务内检查源资源当前无冲突写锁，避免移动与并发写入交错。
        locks::ensure_available(&tx, &source_resource, LockMode::Write)?;
        if let Some(destination) = container::resolve(&tx, &to)? {
            // 目标就是源自身：视为成功的无操作，避免无意义报错。
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
            // 目标是文件：同名冲突；目标是目录：改落点到该目录下、沿用原名。
            if destination.kind != "directory" {
                return Err(VeilError::FileAlreadyExists(to));
            }
            to = format!("{to}/{}", n.name);
        }
        // 拆分目标为父目录路径 + 末段名称，并确认父目录真实存在。
        let (parent, name) = split_parent(&to);
        let parent = container::resolve(&tx, parent)?
            .filter(|n| n.kind == "directory")
            .ok_or_else(|| invalid("目标父目录不存在"))?;
        // 目标父目录必须可用（无冲突锁），防止与并发删除竞争。
        locks::ensure_parent_available(&tx, &parent.id)?;
        // 防环检查：从目标父目录向上回溯祖先链，若包含源节点，
        // 说明要把目录移进自己的子树，必须拒绝。
        let cycle:bool=tx.query_row("WITH RECURSIVE ancestors(id,parent_id) AS (SELECT id,parent_id FROM file_info WHERE id=?1 UNION ALL SELECT f.id,f.parent_id FROM file_info f JOIN ancestors a ON f.id=a.parent_id) SELECT EXISTS(SELECT 1 FROM ancestors WHERE id=?2)",params![parent.id,n.id],|r|r.get(0))?;
        if cycle {
            return Err(VeilError::WorkError("不能把目录移动到自身或子目录".into()));
        }
        // 移动只改这一行：父指针 + 名称 + 更新时间，后代节点与版本链不动。
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
    /// 尝试回收协议识别的无引用密文和失效暂存目录，并记录失败告警。
    /// 入参：password 为容器密码。
    /// 返回值：usize为本轮成功删除对象数；开库遇数据库忙返回0。
    /// 异常：内部失败告警后原样返回Err，并非吞掉错误；错误前可能已删除部分对象。
    /// 注意事项：不是常驻定时服务，不保证自动下一轮执行；不承担安全擦除，未知文件保留。
    pub fn cleanup(&self, password: &str) -> Result<usize> {
        let result = self.cleanup_inner(password);
        if let Err(error) = &result {
            veil_core::logging::failure(
                veil_core::logging::Level::Warn,
                "service",
                "cleanup",
                "CLEANUP_FAILED",
                error,
                &[("database", veil_core::logging::Field::Path(&self.veil_dir))],
            );
        }
        result
    }
    /// 按数据库引用和租约期限复核候选，再分批物理清理。
    /// 入参：self提供目录/可选身份；password 为开库密码。
    /// 返回值：成功返回删除对象总数，单个暂存目录及其内容整体计1；忙批次跳过。
    /// 异常：非忙数据库错误、扫描/序列化/删除失败返回VeilError，先前已删对象不回滚。
    /// 注意事项：开库零等待；候选先全部收集到内存，再每256项批量查保护集。查保护与磁盘删除不在共同事务，依赖文件名/租约写入协议；不主动探测持有者存活。
    fn cleanup_inner(&self, password: &str) -> Result<usize> {
        let db = match ContainerDb::open_with_timeout(
            &self.veil_dir,
            password,
            self.expected_veil_id.as_deref(),
            std::time::Duration::ZERO,
        ) {
            Ok(db) => db,
            Err(VeilError::Database(error))
                if matches!(
                    error.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
                ) =>
            {
                return Ok(0);
            }
            Err(error) => return Err(error),
        };
        // 初始快照中仍被引用的密文可以直接保留；候选项还会逐个只读复核。
        let referenced: HashSet<String> = {
            let mut query = db
                .conn
                .prepare("SELECT cipher_name FROM file_info WHERE cipher_name IS NOT NULL")?;
            query
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        // orphaned_files：磁盘上有、数据库未引用的候选密文 (路径, 文件名, lock_id)。
        let mut orphaned_files = Vec::new();
        for entry in fs::read_dir(&self.veil_dir)? {
            // 遍历容器目录：只认形如 <lock_id>_<uuid>.enc 的协议密文，
            // 其余文件（如 veil.db、日志）一律跳过不动。
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !entry.file_type()?.is_file() || container::validate_cipher(&name).is_err() {
                continue;
            }
            // 数据库仍引用的密文直接跳过。
            if referenced.contains(&name) {
                continue;
            }
            // 从密文文件名取出 lock_id，用于随后核对对应租约是否仍有效。
            let lock_id = name.split('_').next().unwrap().to_owned();
            orphaned_files.push((entry.path(), name, lock_id));
        }
        // staging_directories：.staging 下以 lock_id（UUID）命名的暂存目录。
        let mut staging_directories = Vec::new();
        let staging = self.veil_dir.join(".staging");
        if staging.is_dir() {
            for entry in fs::read_dir(&staging)? {
                // 只认 UUID 命名的子目录（与租约 lock_id 对应），其他目录保留。
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if !entry.file_type()?.is_dir() || !crate::metadata::is_uuid_v4(&name) {
                    continue;
                }
                staging_directories.push((entry.path(), name));
            }
        }
        // removed：本轮实际删除的对象计数。
        let mut removed = 0;
        for candidates in orphaned_files.chunks(CLEANUP_QUERY_BATCH_SIZE) {
            // 分批复核孤儿密文：一次 SQL 批量查出"仍被引用"或"租约仍有效"的保护集合。
            let names = candidates
                .iter()
                .map(|(_, name, _)| name)
                .collect::<Vec<_>>();
            let lock_ids = candidates
                .iter()
                .map(|(_, _, lock_id)| lock_id)
                .collect::<Vec<_>>();
            let names = json(&names)?;
            let lock_ids = json(&lock_ids)?;
            // protected：('file',密文名) 表示仍被引用；('lock',lock_id) 表示租约未过期。
            let protected = match (|| -> rusqlite::Result<Vec<(String, String)>> {
                let mut query = db.conn.prepare(
                    "SELECT 'file',cipher_name FROM file_info WHERE cipher_name IN (SELECT value FROM json_each(?1)) UNION ALL SELECT 'lock',lock_id FROM operation_locks WHERE lock_id IN (SELECT value FROM json_each(?2)) AND expires_at>?3",
                )?;
                query
                    .query_map(params![names, lock_ids, now()], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })?
                    .collect()
            })() {
                Ok(protected) => protected,
                Err(error)
                    if matches!(
                        error.sqlite_error_code(),
                        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
                    ) =>
                {
                    // 数据库正忙：整批跳过，留待下一轮清理。
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let protected = protected.into_iter().collect::<HashSet<_>>();
            for (path, name, lock_id) in candidates {
                // 只删除既无数据库引用、又无有效租约保护的密文。
                if !protected.contains(&("file".to_owned(), name.clone()))
                    && !protected.contains(&("lock".to_owned(), lock_id.clone()))
                {
                    fs::remove_file(path)?;
                    removed += 1;
                }
            }
        }
        for candidates in staging_directories.chunks(CLEANUP_QUERY_BATCH_SIZE) {
            // 分批复核暂存目录：只删对应租约已失效的目录。
            let lock_ids = candidates
                .iter()
                .map(|(_, lock_id)| lock_id)
                .collect::<Vec<_>>();
            let lock_ids = json(&lock_ids)?;
            // active：仍持有未过期租约的 lock_id 集合，其暂存目录不能动。
            let active = match (|| -> rusqlite::Result<HashSet<String>> {
                let mut query = db.conn.prepare(
                    "SELECT lock_id FROM operation_locks WHERE lock_id IN (SELECT value FROM json_each(?1)) AND expires_at>?2",
                )?;
                query
                    .query_map(params![lock_ids, now()], |row| row.get(0))?
                    .collect()
            })() {
                Ok(active) => active,
                Err(error)
                    if matches!(
                        error.sqlite_error_code(),
                        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
                    ) =>
                {
                    // 数据库正忙：整批跳过，留待下一轮清理。
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            for (path, lock_id) in candidates {
                // 租约已过期的暂存目录整体删除（含残留半成品密文）。
                if !active.contains(lock_id) {
                    fs::remove_dir_all(path)?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }
    /// 仅切换SQLCipher数据库密码保护，不重加密独立文件密文。
    /// 入参：old（&str）为当前数据库密码；new（&str）为非空新密码。
    /// 返回值：重新用new打开成功即Ok(())，即使rekey曾返回错误也以可重开结果为准。
    /// 异常：等锁/旧密码/凭证/改密失败返回错误；新旧均无法打开为CommitUncertain。
    /// 注意事项：持容器Write租约，确认切换后用新密码释放；释放失败被忽略，不误报改密失败。已借出旧连接不会被主动终止，重新开库验证仍必要。
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
    /// 通过持久化双密钥状态逐文件重加密，再切换数据库密码并移除旧密钥。
    /// 入参：old为原数据库密码；new为非空目标密码。旧密码仅解密失败时尝试new续做。
    /// 返回值：轮换全部文件、确认新密码且移除旧密钥后Ok(())；旧密码失败而新密码可开且只剩一把密钥时幂等成功。
    /// 异常：密码/锁/节点/活动密钥变化、重加密/发布/数据库错误传播；旧引用未清零拒绝移除密钥；内部密钥/size不变量破坏可panic。
    /// 注意事项：不是一次全局回滚事务：准备检查根范围冲突但不长期持根租约，迁移逐文件Write租约，其他文件可并发变化。每文件独立提交，以库内key_id续做；旧密文/暂存残留需另行cleanup。密码切换成功但收尾失败时可能已经需要新密码。
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
            // 数据库已用新密码、且只剩一把密钥：上次轮换实际已完成，幂等返回。
            return Ok(());
        }
        let mut db = self.open_db(password)?;
        let tx = db
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let root = container::resolve(&tx, "")?.ok_or_else(|| invalid("根目录缺失"))?;
        // 准备事务内检查根范围无冲突租约；这里只检查，不持有贯穿整轮轮换的根目录守卫。
        locks::ensure_available(&tx, &locks::directory_resource(&root.id), LockMode::Write)?;
        let mut keys = container::keys(&tx)?;
        if keys.len() == 1 {
            // 单密钥状态：把唯一密钥标为 retiring，再生成新 active 密钥。
            // 双密钥状态持久化后，即使崩溃也能按库内状态恢复轮换。
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
        // 阶段二：逐文件迁移。每轮挑一个仍引用旧密钥的文件处理，
        // 直到没有任何文件引用旧密钥（数据库引用是唯一进度依据）。
        loop {
            let db = self.open_db(password)?;
            let keys = db.keys()?;
            // active：当前活动（新）密钥，所有迁移目标。
            let active = keys.iter().find(|k| k.status == "active").unwrap();
            // candidate：下一个仍引用旧密钥的文件 ID；没有则迁移完成。
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
            // 没有旧密钥引用，跳出循环进入改密与收尾阶段。
            let Some(identity) = candidate else {
                break;
            };
            drop(db);
            // 为这个文件申请写锁：迁移期间该文件不能被并发读写。
            let guard = self
                .lock_manager(password)
                .acquire(std::slice::from_ref(&identity), LockMode::Write)?;
            let db = self.open_db(password)?;
            let n = db
                .node(&identity)?
                .ok_or_else(|| VeilError::FileNotFound(identity.clone()))?;
            let keys = db.keys()?;
            let active = keys.iter().find(|k| k.status == "active").unwrap();
            // 等待锁期间该文件可能已被并发迁移；已用新密钥则直接跳过。
            if n.key_id.as_deref() == Some(&active.key_id) {
                continue;
            }
            // previous：该文件当前引用的旧密钥，用于解密旧密文。
            let previous = keys
                .iter()
                .find(|k| Some(&k.key_id) == n.key_id.as_ref())
                .ok_or_else(|| invalid("文件引用未知密钥"))?;
            // 取出本次写锁凭证，随后用它构造密文名与暂存目录。
            let credential = guard.credentials()?.remove(0);
            // 新密文沿用 <lock_id>_<uuid>.enc 命名约定，落盘前先写暂存区。
            let cipher = format!("{}_{}.enc", credential.lock_id, id());
            let nonce = file_ops::generate_base_nonce();
            let staging = self.veil_dir.join(".staging").join(&credential.lock_id);
            crate::fsutil::create_dir_all_durable(&staging)?;
            // 旧密钥解密 + 新密钥加密的流式重写：明文不整体进内存。
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
            // 提交前复核：写锁凭证仍有效、节点未被并发改动、活动密钥未变。
            locks::validate(&tx, &c, Some(LockMode::Write))?;
            if container::node(&tx, &identity)?.as_ref() != Some(&n)
                || !container::keys(&tx)?
                    .iter()
                    .any(|k| k.status == "active" && k.key_id == active_id)
            {
                return Err(VeilError::VersionConflict("轮换提交条件已变化".into()));
            }
            // 更新该文件的密文引用、密钥与 nonce；文件 ID/版本链不变。
            tx.execute("UPDATE file_info SET cipher_name=?1,key_id=?2,nonce=?3,encrypted_at=?4 WHERE id=?5",params![cipher,active_id,nonce.as_slice(),now(),identity])?;
            // 同一事务内释放写锁：提交即宣告迁移完成，密文引用与锁一致。
            locks::release_in(&tx, &c)?;
            tx.commit()?;
            guard.released_in_transaction();
            drop(guard);
            fault("after_rotation_file");
        }
        fault("before_database_rekey");
        // 阶段三前半：所有文件已用新密钥，切换到新数据库密码。
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
        // 收尾前最后确认：没有任何文件仍引用旧密钥。
        let remaining: i64 = tx.query_row(
            "SELECT count(*) FROM file_info WHERE kind='file' AND key_id<>?1",
            [&active],
            |r| r.get(0),
        )?;
        if remaining != 0 {
            return Err(invalid("旧文件密钥仍被引用"));
        }
        // 从当前数据库密钥集合移除retiring旧密钥，回到单密钥；不清除其他进程已持有的密钥副本或旧备份。
        keys.retain(|k| k.status == "active");
        fault("before_retiring_key_removal");
        tx.execute(
            "UPDATE div_info SET data_keys=?1 WHERE id=1",
            [json(&keys)?],
        )?;
        tx.commit()?;
        drop(db);
        fault("after_retiring_key_removal");
        Ok(())
    }
}
/// 验证编辑基础快照仍指向当前容器中的同一文件和相同版本链。
/// 入参：identity（&str）为容器ID；node（`Option<&Node>`）为当前文件节点；base（&VersionSnapshot）为编辑开始时快照。
/// 返回值：身份和版本链均相同返回Ok(())。
/// 异常：节点缺失、容器/文件身份或版本关系不同为VersionConflict；版本链解析/验证错误传播。
/// 注意事项：仅比较内存元数据，不查新连接或申请租约；调用方应在保护最终写入的事务中提供当前节点，不比较内容哈希。
fn verify_base(identity: &str, node: Option<&Node>, base: &VersionSnapshot) -> Result<()> {
    let Some(node) = node else {
        return Err(VeilError::VersionConflict("基础文件已删除".into()));
    };
    use crate::comparison::{compare_files, ComparisonChecks, ComparisonState, FileInput};
    let current = VersionSnapshot {
        veil_id: identity.to_owned(),
        file_id: node.id.clone(),
        version_history: node.versions()?.to_vec(),
    };
    // 使用当前事务中的快照比较，不重新打开数据库或申请文件租约。
    let report = compare_files(
        FileInput::ContainerMetadata {
            version: &current,
            hash: None,
        },
        FileInput::ContainerMetadata {
            version: base,
            hash: None,
        },
        ComparisonChecks {
            version: true,
            ..Default::default()
        },
    )?;
    if report.version != Some(ComparisonState::Same) {
        return Err(VeilError::VersionConflict(
            "文件身份或基础版本不匹配".into(),
        ));
    }
    Ok(())
}
/// 把规范化文件逻辑路径拆为父路径与单级名称。
/// 入参：path（&str）为已校验内部路径。
/// 返回值：(&str,&str)，按最后一个“/”拆分；无分隔符返回空父路径及原文。
/// 异常：无错误返回。
/// 注意事项：不验证空串、根路径、非法组件，返回借用不能超出输入寿命。
fn split_parent(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}
/// 把可用的容器内部文件/目录路径转为统一相对形式。
/// 入参：path（&str）为非空逻辑路径，禁止首斜杠/反斜杠、任何反斜杠或NUL；允许尾斜杠及“.”组件。
/// 返回值：去掉全部尾斜杠及“.”组件后，用“/”拼接的非空String。
/// 异常：绝对样式、空组件、任何“..”或清理后为空返回参数错误。
/// 注意事项：不接受空串、“.”或“/”作为根，需根语义的入口须自行转换；不在此检查文件名长度、控制字符或文件系统平台保留名。
pub fn normalize_veil_dir(path: &str) -> Result<String> {
    if path.is_empty() || path.starts_with(['/', '\\']) || path.contains(['\\', '\0']) {
        return Err(invalid("容器路径无效"));
    }
    let path = path.trim_end_matches('/');
    // parts：规范化后的路径分段，逐段校验后重新拼接。
    let mut parts = Vec::new();
    for part in path.split('/') {
        // "." 分段直接丢弃（表示当前目录，不影响语义）。
        if part == "." {
            continue;
        }
        // 空分段（双斜杠）与 ".." 一律拒绝，防止路径穿越。
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
/// 记录写事务结果不确定后，重新核对密文引用是否得到确认。
/// 入参：directory（&Path）为容器目录；files（usize）为涉及文件数；confirmed（bool）为引用已全部确认的结果。
/// 返回值：无，写入固定事件和脱敏路径/计数。
/// 异常：无Result错误返回。
/// 注意事项：不执行实际复核或恢复，不证明文件系统持久性；确认逻辑由调用方完成。
fn log_commit_confirmation(directory: &Path, files: usize, confirmed: bool) {
    use veil_core::logging::{self, Field, Level};
    logging::event(
        if confirmed { Level::Warn } else { Level::Error },
        "service",
        "confirm_file_commit",
        if confirmed {
            "FILE_COMMIT_RECOVERED"
        } else {
            "FILE_COMMIT_UNCONFIRMED"
        },
        "文件写入提交异常后的状态核对",
        &[
            ("database", Field::Path(directory)),
            ("file_count", Field::Count(files as u64)),
            ("phase", Field::Label("verify_metadata_references")),
            (
                "outcome",
                Field::Label(if confirmed { "committed" } else { "unknown" }),
            ),
        ],
    );
}

/// 在测试或故障注入构建中触发指定阶段的模拟故障点。
/// 入参：stage（&str）为与测试约定的阶段标签。
/// 返回值：无；普通构建不调用fault_point。
/// 异常：启用时按底层故障配置可能panic等，原样传播。
/// 注意事项：不用于正常业务错误处理；普通构建空操作，不能据注释承诺所有构建/优化级别均零开销。
fn fault(stage: &str) {
    #[cfg(any(test, feature = "fault-injection"))]
    {
        veil_core::fsutil::fault_point(stage);
    }
    #[cfg(not(any(test, feature = "fault-injection")))]
    let _ = stage;
}

/// CancelledFiles 守卫的单元测试模块。
#[cfg(test)]
mod cancellation_tests {
    use super::*;
    /// 验证取消清理只删未提交密文：committed=false 的守卫析构时删文件，
    /// committed=true 的守卫析构时必须保留已提交密文。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
