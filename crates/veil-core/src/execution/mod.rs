//! 进程、线程与异步任务的统一登记。这里只维护执行生命周期，不负责数据库或 IPC。
//!
//! 线程身份在线程退出前保持不变；任务身份不随调度线程变化。未知身份不能推断为死亡。
//!
//! 所属业务模块：veil-core 的执行身份层。它为"谁在执行、是否还活着"提供进程内
//! 登记簿：每个参与持锁（租约=某段时间内独占操作权的凭证）的业务线程/任务都先
//! 登记拿到稳定身份（ExecutionOwner），结束时登记为结束状态；其他进程可以据此
//! 提前回收已结束持有者的锁，而不是干等租约到期。
//!
//! 文件内核心清单：
//! - `Liveness`：存活状态三态（活着/已死/未知），未知不能当死亡用。
//! - `ProcessIdentity`/`ExecutionOwner`：进程、线程、任务三级身份。
//! - `ExecutionRuntime::global`：每进程唯一登记簿入口（fork 后拒绝复用）。
//! - `register_thread`/`current_context`：线程登记与当前执行上下文查询。
//! - `enter_task`/`spawn_thread`/`prepare_blocking`/`track_future`：各类执行单元的
//!   统一包装入口，持锁业务必须从这里创建或包装。
//! - `check_process`/`check_local`/`info`：存活检查与登记信息查询。
//!
//! 依赖能力：
//! - `process` 子模块：操作系统级进程存活检查。
//! - 线程局部存储：当前线程登记与任务栈。
//! - `crate::logging`：日志上下文随执行单元跨线程传递。
//!
//! 整体业务目标：让文件锁的 owner 能够指向"进程/线程/任务"并随时验证其死活；
//! 查询规则——本进程的线程/任务由登记簿回答，其他进程先查进程实例再委托查询，
//! 只有确认原持有者已结束才返回 Dead。

mod process;

use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io;
use std::marker::PhantomData;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::task::{Context, Poll};
use std::thread::{self, JoinHandle};

/// 登记簿最多保留的已结束记录数量；超出后最老的记录被淘汰，
/// 之后对它们的查询返回 Unknown，仍由锁租约机制兜底。
const RETAINED_FINISHED: usize = 1024;

/// 执行单元的存活状态。
///
/// 业务语义：
/// - `Alive`：确认还在执行（或进程身份完全匹配）。
/// - `Dead`：确认已结束（进程退出且未被复用、线程/任务记录已结束）。
/// - `Unknown`：无法判断——缺少记录、权限不足、通信失败等一律保守返回此值，
///   调用方按租约（到期自动失效）处理，绝不能当作已死提前回收锁。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    Alive,
    Dead,
    Unknown,
}

/// 随机实例 ID 区分进程重启；系统标识用于识别被复用的 PID。
///
/// 业务定位：一个操作系统进程实例的稳定身份快照。PID 会随时间复用，
/// `instance_id` 每次进程启动随机生成，两者共同确认"是不是我认识的这个进程"。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProcessIdentity {
    /// 操作系统进程号；只作辅助定位，不能单独当作身份。
    pub pid: u32,
    /// 运行时初始化时生成的 16 字节随机值，编码为 32 个十六进制字符；重新初始化通常得到不同值，不承诺绝无碰撞。
    pub instance_id: String,
    /// 操作系统提供的进程实例标识（如启动时间 + 系统启动 ID）；
    /// 平台不支持或获取失败时为 None，此时进程级存活只能保守判断。
    pub system_identity: Option<String>,
}

/// 执行单元身份：进程、线程或任务。
///
/// 业务定位：文件锁 `owner_identity` 的载体。持有锁的可能是整个进程、某个业务
/// 线程、或某个异步任务；三者共享同一进程身份，另带各自的稳定 ID。
///
/// 使用场景：锁记录里保存 owner；存活检查时按类型选择查询路径（本进程查登记簿，
/// 其他进程委托对方 service 查询）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionOwner {
    /// 整个进程作为持有者（如进程级操作锁）。
    Process {
        /// 所属进程身份。
        process: ProcessIdentity,
    },
    /// 进程内的一个业务线程作为持有者。
    Thread {
        /// 所属进程身份。
        process: ProcessIdentity,
        /// 本进程登记簿生成的线程身份，不是系统 TID。
        thread_id: String,
    },
    /// 进程内的一个异步任务作为持有者。
    Task {
        /// 所属进程身份。
        process: ProcessIdentity,
        /// 本进程登记簿生成的稳定任务身份；跨线程调度不变。
        task_id: String,
    },
}

impl ExecutionOwner {
    /// 功能：从任一执行者身份中读取所属进程，以便选择本地登记或跨进程探测路径。
    /// 入参：self: &ExecutionOwner，可为进程、线程或任务变体。
    /// 返回值：&ProcessIdentity，借用该身份内保存的进程信息。
    /// 异常：穷尽匹配变体，不返回错误。
    /// 注意事项：只取出已保存身份，不观察操作系统，也不验证字段可信性或进程仍存活。
    pub fn process(&self) -> &ProcessIdentity {
        match self {
            Self::Process { process }
            | Self::Thread { process, .. }
            | Self::Task { process, .. } => process,
        }
    }
}

/// 单个执行记录的运行状态。
///
/// 状态机：Starting → Running →（Completed / Cancelled / Panicked）。
/// 状态用单字节编码存储（`#[repr(u8)]`），在 `Record` 里用原子变量读写。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum ExecutionState {
    /// 已登记但尚未开始执行。
    Starting,
    /// 正在执行。
    Running,
    /// 正常结束。
    Completed,
    /// 被显式取消（如 TaskScope::cancel、Future 未完成即被丢弃）。
    Cancelled,
    /// 执行中发生 panic。
    Panicked,
}

impl ExecutionState {
    /// 功能：将登记记录的原子状态字节还原成执行状态，供查询和结束判定使用。
    /// 入参：value: u8，为从 Record.state 读取的编码；0/1/2/3 分别表示 Starting/Running/Completed/Cancelled。
    /// 返回值：对应 ExecutionState，其他字节统一解释为 Panicked。
    /// 异常：不返回格式错误，不访问共享状态。
    /// 注意事项：这是内部编码解释，未知字节被保守当作结束态；不能拿它验证外部状态协议是否合法。
    fn from_byte(value: u8) -> Self {
        match value {
            0 => Self::Starting,
            1 => Self::Running,
            2 => Self::Completed,
            3 => Self::Cancelled,
            _ => Self::Panicked,
        }
    }

    /// 功能：判断登记记录是否已经进入结束态，供存活查询和防重复结束使用。
    /// 入参：self: ExecutionState 为一份状态值。
    /// 返回值：Completed、Cancelled 或 Panicked 为 true；Starting、Running 为 false。
    /// 异常：纯枚举判断，不返回错误。
    /// 注意事项：Starting 也按未结束处理，包括尚未实际运行的排队任务；结束态描述登记的工作生命周期，不必然表示系统线程已退出。
    fn is_finished(self) -> bool {
        !matches!(self, Self::Starting | Self::Running)
    }
}

/// 查询接口返回的执行单元信息快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionInfo {
    /// 执行单元身份。
    pub owner: ExecutionOwner,
    /// 登记时给的名字（线程名/任务名），用于展示与日志。
    pub name: String,
    /// 当前运行状态。
    pub state: ExecutionState,
}

/// 每次调用查询当前调度线程；任务身份在跨线程调度时保持不变。
///
/// 业务定位：一段代码执行时的"我在哪、属于谁"上下文；持锁归属由调用方根据
/// `owner()` 选择线程或任务身份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionContext {
    /// 当前调度线程的身份（自动登记）。
    pub thread: ExecutionOwner,
    /// 当前任务栈顶的任务身份；不在任务中时为 None。
    pub task: Option<ExecutionOwner>,
}

impl ExecutionContext {
    /// 功能：给当前工作选择最细的执行归属，用于租约持有者和日志关联。
    /// 入参：self: &ExecutionContext 为当前线程及可选任务的上下文快照。
    /// 返回值：有 task 时借用任务身份，否则借用 thread 身份。
    /// 异常：不查登记簿，不返回错误。
    /// 注意事项：这是上下文选择，不自行申请锁；上下文只反映采集时的任务栈，不能代替后续存活检查。
    pub fn owner(&self) -> &ExecutionOwner {
        self.task.as_ref().unwrap_or(&self.thread)
    }
}

/// 登记簿里的一条执行记录：owner + 名字 + 原子状态。
struct Record {
    /// 该记录对应的执行单元身份（键，也是锁的归属）。
    owner: ExecutionOwner,
    /// 登记名字，查询时随状态返回。
    name: String,
    /// 状态字节；用原子量支持跨线程读写。
    state: AtomicU8,
    /// 是否发生过 panic：与状态分开记录，因为 Future 析构前不能提前宣告结束。
    panicked: AtomicBool,
}

impl Record {
    /// 功能：读取一条执行记录的状态，供查询和结束状态转换使用。
    /// 入参：self: &Record 为共享登记记录。
    /// 返回值：ExecutionState，为此次 Acquire 原子读取的字节所解释的状态。
    /// 异常：不返回错误，未知字节由 from_byte 解释为 Panicked。
    /// 注意事项：Acquire 用于配合其他线程的 Release 写入建立可见性，不表示等待所有并发更新完成或保证之后状态不变。
    fn state(&self) -> ExecutionState {
        ExecutionState::from_byte(self.state.load(Ordering::Acquire))
    }
}

/// 进程内登记簿：所有执行记录 + 已结束队列。
#[derive(Default)]
struct Registry {
    /// 全部执行记录：owner → 记录。每键一条，装的是该执行单元的最新状态。
    records: HashMap<ExecutionOwner, Arc<Record>>,
    /// 已结束的 owner 队列（先进先出），用于上限淘汰最老的已结束记录。
    finished: VecDeque<ExecutionOwner>,
}

/// 每进程一份的登记簿内核。
struct Inner {
    /// 本进程的身份快照。
    process: ProcessIdentity,
    /// 自增编号源：为新线程/任务生成不重复的 16 进制序号。
    next_id: AtomicU64,
    /// 受互斥锁保护的登记簿。
    registry: Mutex<Registry>,
}

impl Inner {
    /// 功能：把尚未结束的记录置为结束态，并保留有限历史以支持后续租约回收判断。
    /// 入参：self: &Inner 为登记簿内核；record: &Record 为要结束的线程或任务；state: ExecutionState 应为 Completed/Cancelled/Panicked。
    /// 返回值：无；首次有效转换后把 owner 加入 finished，超出 1024 条时从 records 淘汰最早结束项。
    /// 异常：不返回 Result；登记簿锁中毒时取回状态继续维护。
    /// 注意事项：CAS 式更新仅让第一次结束生效；调用者不得传入未结束状态，本函数未额外验证此内部前置条件。
    /// 状态转换后才加历史队列锁，淘汰后查询变为 Unknown；不停止任何线程或 Future，也不操作数据库锁。
    fn finish(&self, record: &Record, state: ExecutionState) {
        // 只有从"未结束"状态转换才算有效；重复结束（如析构与显式 cancel 并发）
        // 的第二次调用会被拒绝，保证结束态只设置一次。
        let previous = record
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                (!ExecutionState::from_byte(old).is_finished()).then_some(state as u8)
            });
        if previous.is_err() {
            return;
        }
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        registry.finished.push_back(record.owner.clone());
        // 只淘汰已结束记录。淘汰后查询返回 Unknown，仍由锁租约兜底。
        while registry.finished.len() > RETAINED_FINISHED {
            if let Some(owner) = registry.finished.pop_front() {
                registry.records.remove(&owner);
            }
        }
    }
}

/// 执行运行时：登记簿的公开句柄，可在任意线程克隆共享。
#[derive(Clone)]
pub struct ExecutionRuntime(Arc<Inner>);

thread_local! {
    /// 当前线程的登记：无登记为 None；重复登记时直接返回已有身份。
    static LOCAL_THREAD: RefCell<Option<ThreadRegistration>> = const { RefCell::new(None) };
    /// 当前线程的任务栈：每层任务一个 owner；栈顶是"当前任务"。
    static TASK_STACK: RefCell<Vec<ExecutionOwner>> = const { RefCell::new(Vec::new()) };
}

/// 线程的登记守卫：存活期间线程身份有效，析构时宣告线程完成。
struct ThreadRegistration {
    /// 登记簿弱引用：登记簿被销毁（进程退出）时静默失效。
    runtime: Weak<Inner>,
    /// 本线程对应的记录。
    record: Arc<Record>,
}

impl Drop for ThreadRegistration {
    /// 功能：线程局部登记对象释放时将该线程的登记记录结束，避免永久显示为活动。
    /// 入参：self: &mut ThreadRegistration，持有登记簿弱引用及本线程记录。
    /// 返回值：无；运行时仍存在时尝试写入 Completed，运行时已销毁则跳过。
    /// 异常：不返回 Result；登记簿锁中毒由 finish 恢复。
    /// 注意事项：受管线程可能已由 ThreadCompletion 写入 Panicked/Completed，重复结束不会覆盖；外部线程此守卫本身不区分退出原因。
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.upgrade() {
            runtime.finish(&self.record, ExecutionState::Completed);
        }
    }
}

impl ExecutionRuntime {
    /// 功能：为同一进程的服务实例和调度器提供共享执行登记簿。
    /// 入参：无；首次调用采集当前进程身份并初始化计数器及登记表。
    /// 返回值：Ok(ExecutionRuntime)，克隆句柄共享同一 Inner；初始化失败或 fork 后误用父进程实例返回 io::Error。
    /// 异常：随机身份采集失败文本会缓存在 OnceLock 中，后续调用继续返回同一初始化失败；PID 改变也返回错误。
    /// 注意事项：不会为 fork 子进程重建登记簿；应使用独立启动的进程。初始化只执行一次，不会因重试调用而重新取随机数。
    pub fn global() -> io::Result<Self> {
        static RUNTIME: OnceLock<Result<Arc<Inner>, String>> = OnceLock::new();
        let inner = RUNTIME
            .get_or_init(|| {
                process::current()
                    .map(|process| {
                        Arc::new(Inner {
                            process,
                            next_id: AtomicU64::new(1),
                            registry: Mutex::new(Registry::default()),
                        })
                    })
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(|error| io::Error::other(error.clone()))?;
        // fork 出的子进程会继承父进程内存里的登记簿快照，但 PID 已变：
        // 直接拒绝使用，避免把子进程登记成父进程的线程。
        if inner.process.pid != std::process::id() {
            return Err(io::Error::other(
                "fork 后不能复用父进程的执行登记，请使用独立启动的进程",
            ));
        }
        Ok(Self(Arc::clone(inner)))
    }

    /// 功能：提供当前运行时保存的进程身份，供构造线程/任务归属和比较使用。
    /// 入参：self: &ExecutionRuntime 为本进程登记簿句柄。
    /// 返回值：&ProcessIdentity，借用初始化时采集的身份。
    /// 异常：不再次查询操作系统，不返回错误。
    /// 注意事项：身份在运行时生命周期内保持不变；fork 后直接复用既有句柄不受此读取方法重新检查，应遵守 global 的进程约束。
    pub fn process(&self) -> &ProcessIdentity {
        &self.0.process
    }

    /// 功能：构造把整个进程作为租约持有者的身份。
    /// 入参：self: &ExecutionRuntime，为持有进程身份的运行时。
    /// 返回值：ExecutionOwner::Process，拥有一份 ProcessIdentity 副本。
    /// 异常：只复制身份，不返回业务错误。
    /// 注意事项：不创建线程/任务记录；进程归属的存活不能表示任意特定业务任务仍在执行。
    pub fn process_owner(&self) -> ExecutionOwner {
        ExecutionOwner::Process {
            process: self.process().clone(),
        }
    }

    /// 功能：确认目标是否仍为原进程实例，避免 PID 复用导致误保留或误回收租约。
    /// 入参：self: &ExecutionRuntime 为当前运行时；identity: &ProcessIdentity 为持有者记录中的预期身份。
    /// 返回值：Liveness；同 PID 时完整身份相等为 Alive、不等为 Dead；其他 PID 交给系统观察，证据不足为 Unknown。
    /// 异常：不以 Result 返回系统失败，跨进程观察的错误降级为 Unknown。
    /// 注意事项：调用方应先验证记录来源与字段；此方法只检查进程，不确认进程内某个线程或任务的状态。
    pub fn check_process(&self, identity: &ProcessIdentity) -> Liveness {
        if identity.pid == self.process().pid {
            // 同一 PID：实例 ID 也一致才认为是自己，否则是旧实例记录（已退出）
            return if identity == self.process() {
                Liveness::Alive
            } else {
                Liveness::Dead
            };
        }
        process::check(identity)
    }

    /// 功能：依据当前进程登记簿回答一个已登记线程或任务是否仍未结束。
    /// 入参：self: &ExecutionRuntime 为当前登记簿；owner: &ExecutionOwner 为待查完整身份。
    /// 返回值：进程身份不属于自己为 Unknown；本进程 Process 变体为 Alive；有记录的 Starting/Running 为 Alive，结束态为 Dead；无记录为 Unknown。
    /// 异常：不返回 Result；登记簿锁中毒时恢复内部表读取。
    /// 注意事项：不发 IPC、不查询远端；已淘汰的结束记录回到 Unknown，不能据缺失记录断言死亡。
    pub fn check_local(&self, owner: &ExecutionOwner) -> Liveness {
        // 其他进程的身份不在本进程登记簿里：不猜测，返回 Unknown
        if owner.process() != self.process() {
            return Liveness::Unknown;
        }
        // 进程级身份：本进程还在运行，恒为 Alive
        if matches!(owner, ExecutionOwner::Process { .. }) {
            return Liveness::Alive;
        }
        self.info(owner).map_or(Liveness::Unknown, |info| {
            if info.state.is_finished() {
                Liveness::Dead
            } else {
                Liveness::Alive
            }
        })
    }

    /// 功能：查询一条执行记录的展示名称和状态，支持状态诊断及本地存活判断。
    /// 入参：self: &ExecutionRuntime 为登记簿；owner: &ExecutionOwner 为完整记录键。
    /// 返回值：有键时 Some(ExecutionInfo) 独立快照，否则 None；通常 Process 变体不单独登记记录。
    /// 异常：锁中毒时恢复登记表，不返回业务错误。
    /// 注意事项：快照返回后状态可能变化；None 可能是从未登记或已淘汰，不能等同于已结束。
    pub fn info(&self, owner: &ExecutionOwner) -> Option<ExecutionInfo> {
        let registry = self
            .0
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        registry.records.get(owner).map(|record| ExecutionInfo {
            owner: owner.clone(),
            name: record.name.clone(),
            state: record.state(),
        })
    }

    /// 功能：为受管线程或任务预登记身份，使排队和创建阶段也能被存活检查看到。
    /// 入参：self: &ExecutionRuntime 为所属运行时；name: String 为展示名称；is_thread: bool 决定生成 Thread 或 Task 身份。
    /// 返回值：`Arc<Record>`，含进程随机实例前缀与递增序号构造的 ID，初态为 Starting，panicked 为 false。
    /// 异常：64 位递增编号耗尽时 expect panic；登记簿锁中毒时恢复访问。
    /// 注意事项：记录先入表但尚未开始工作；结束后需由对应生命周期守卫写入结束态，不能仅丢弃一个 Arc 就期待取消。
    fn record(&self, name: String, is_thread: bool) -> Arc<Record> {
        // 编号自增；耗尽（几乎不可能）时 panic 保护逻辑正确性
        let next = self
            .0
            .next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("执行实例编号耗尽");
        let id = format!("{}-{next:016x}", self.process().instance_id);
        // 按类型生成线程或任务身份
        let owner = if is_thread {
            ExecutionOwner::Thread {
                process: self.process().clone(),
                thread_id: id,
            }
        } else {
            ExecutionOwner::Task {
                process: self.process().clone(),
                task_id: id,
            }
        };
        let record = Arc::new(Record {
            owner: owner.clone(),
            name,
            state: AtomicU8::new(ExecutionState::Starting as u8),
            panicked: AtomicBool::new(false),
        });
        // 插入登记簿，owner 为键
        self.0
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .records
            .insert(owner, Arc::clone(&record));
        record
    }

    /// 功能：把当前调度线程接入登记簿，为后续执行上下文提供稳定线程身份。
    /// 入参：self: &ExecutionRuntime；reserved: `Option<Arc<Record>>` 为 spawn_thread 已预建的记录，None 表示必要时现场创建。
    /// 返回值：ExecutionOwner::Thread 的副本；线程已有登记时直接返回原身份。
    /// 异常：线程局部存储或 RefCell 借用冲突可能 panic，编号耗尽也会 panic；不返回 Result。
    /// 注意事项：已有登记时不使用 reserved；只处理当前线程，守卫保存在 TLS 至线程退出，初次登记将记录置为 Running。
    fn register_thread(&self, reserved: Option<Arc<Record>>) -> ExecutionOwner {
        LOCAL_THREAD.with(|local| {
            let mut local = local.borrow_mut();
            // 线程已登记：直接复用，重复调用不会重复登记
            if let Some(registration) = local.as_ref() {
                return registration.record.owner.clone();
            }
            // 没有预建记录就现场建一条：线程名作为展示名
            let record = reserved.unwrap_or_else(|| {
                self.record(
                    thread::current()
                        .name()
                        .unwrap_or("外部运行线程")
                        .to_owned(),
                    true,
                )
            });
            // 登记即视为开始运行
            record
                .state
                .store(ExecutionState::Running as u8, Ordering::Release);
            let owner = record.owner.clone();
            // 把登记守卫放入线程局部：线程退出时自动置 Completed
            *local = Some(ThreadRegistration {
                runtime: Arc::downgrade(&self.0),
                record,
            });
            owner
        })
    }

    /// 功能：为当前业务代码采集“运行在哪个线程、属于哪个任务”的身份上下文。
    /// 入参：self: &ExecutionRuntime 为当前进程运行时。
    /// 返回值：ExecutionContext，thread 为自动登记的当前线程，task 为本线程任务栈顶或 None。
    /// 异常：登记、TLS/RefCell 访问等内部前置条件失败可能 panic，不返回 Result。
    /// 注意事项：不会把其他线程的栈自动传来；异步任务须用 track_future 在每次 poll 时建立本线程任务作用域。
    pub fn current_context(&self) -> ExecutionContext {
        ExecutionContext {
            thread: self.register_thread(None),
            task: TASK_STACK.with(|stack| stack.borrow().last().cloned()),
        }
    }

    /// 功能：为当前线程的一段同步工作建立可查询的任务身份，并在作用域结束时登记结果。
    /// 入参：self: &ExecutionRuntime；name: impl `Into<String>` 为任务展示名，不作为唯一键。
    /// 返回值：TaskScope，任务已置 Running 且压入当前线程任务栈，持有至工作完成。
    /// 异常：编号耗尽、TLS 或 RefCell 借用冲突可能 panic，不返回 Result。
    /// 注意事项：同步作用域含不可跨线程发送的 ActiveScope，须在建立线程释放；跨线程 Future 应使用 track_future。
    /// 正常析构记 Completed，显式 cancel 记 Cancelled，panic 展开由生命周期守卫记录，不等于中断任意外部工作。
    pub fn enter_task(&self, name: impl Into<String>) -> TaskScope {
        self.register_thread(None);
        let lifetime = TaskLifetime {
            runtime: self.clone(),
            record: self.record(name.into(), false),
        };
        lifetime.start();
        let scope = ActiveScope::new(&lifetime);
        TaskScope {
            scope: Some(scope),
            lifetime,
        }
    }

    /// 功能：创建在开始前即已登记身份的业务线程，让锁归属可在创建与运行阶段被查询。
    /// 入参：self: &ExecutionRuntime；name: impl `Into<String>` 同时作为系统线程名与登记名；
    /// work: F 为取得 ExecutionContext 后执行一次并产出 T 的闭包，F/T 必须可跨线程且不借用短生命周期数据。
    /// 返回值：Ok(`Spawned<JoinHandle<T>>`) 含预登记 owner 与可等待线程结果的句柄。
    /// 异常：线程创建失败返回 io::Error，并把预登记记录置 Cancelled；work 的 panic 发生在子线程，等待句柄时可观察到。
    /// 注意事项：捕获创建线程的日志上下文并在子线程恢复；闭包结束即登记完成，系统线程的 TLS 析构可能还在随后运行。
    /// 丢弃 JoinHandle 不会停止线程，也不会使仍在运行的记录提前结束。
    pub fn spawn_thread<F, T>(
        &self,
        name: impl Into<String>,
        work: F,
    ) -> io::Result<Spawned<JoinHandle<T>>>
    where
        F: FnOnce(ExecutionContext) -> T + Send + 'static,
        T: Send + 'static,
    {
        let name = name.into();
        // 预登记：线程创建成功前记录已是 Starting，供创建失败时清理
        let record = self.record(name.clone(), true);
        let owner = record.owner.clone();
        let runtime = self.clone();
        let child_record = Arc::clone(&record);
        // 捕获当前日志上下文：新线程内恢复请求关联等日志信息
        let logging_context = crate::logging::capture_context();
        let result = thread::Builder::new().name(name).spawn(move || {
            let _logging = logging_context.attach();
            // 线程体内复用预建记录完成登记
            runtime.register_thread(Some(Arc::clone(&child_record)));
            let completion = ThreadCompletion {
                runtime: runtime.clone(),
                record: child_record,
            };
            let result = work(runtime.current_context());
            // 正常返回路径：ThreadCompletion 析构把状态置为 Completed
            drop(completion);
            result
        });
        match result {
            Ok(handle) => Ok(Spawned { owner, handle }),
            Err(error) => {
                // 线程没建出来：预建记录按取消处理，不留存活假象
                self.0.finish(&record, ExecutionState::Cancelled);
                Err(error)
            }
        }
    }

    /// 功能：给外部线程池准备一项带稳定身份和日志上下文的阻塞工作，不绑定具体调度器。
    /// 入参：self: &ExecutionRuntime；name: impl `Into<String>` 为展示名；work: F 为取得执行上下文后执行一次、返回 T 的 Send + 'static 闭包。
    /// 返回值：(ExecutionOwner, impl FnOnce() -> T + Send + 'static)，owner 可用于租约归属，闭包供调用方真正提交执行。
    /// 异常：准备阶段不返回 Result；编号耗尽可能 panic；执行 work 时的 panic 在实际运行线程继续传播。
    /// 注意事项：未执行即丢弃闭包会在其捕获的 TaskLifetime 析构后记 Cancelled；取消等待句柄不能强行结束已经开始的阻塞工作。
    /// 正常执行时先登记线程、置 Running、压任务栈，工作返回后记 Completed；日志上下文随实际工作恢复。
    pub fn prepare_blocking<F, T>(
        &self,
        name: impl Into<String>,
        work: F,
    ) -> (ExecutionOwner, impl FnOnce() -> T + Send + 'static)
    where
        F: FnOnce(ExecutionContext) -> T + Send + 'static,
        T: Send + 'static,
    {
        let lifetime = TaskLifetime {
            runtime: self.clone(),
            record: self.record(name.into(), false),
        };
        let owner = lifetime.record.owner.clone();
        let logging_context = crate::logging::capture_context();
        (owner, move || {
            let _logging = logging_context.attach();
            // 在实际执行线程上登记线程身份（线程池线程不要求预先受管）
            lifetime.runtime.register_thread(None);
            lifetime.start();
            let _scope = ActiveScope::new(&lifetime);
            let result = work(lifetime.runtime.current_context());
            lifetime.complete();
            result
        })
    }

    /// 功能：给异步工作建立稳定任务身份，随每次调度恢复线程任务栈和日志上下文。
    /// 入参：self: &ExecutionRuntime；name: impl `Into<String>` 为任务名；future: F 为任意 Future，取得其所有权。
    /// 返回值：`TrackedFuture<F>`，包装时记录为 Starting，内部 Future 固定存放在堆上并保存创建时的日志上下文。
    /// 异常：准备阶段不返回 Result，编号耗尽可能 panic；内部 Future 的执行或析构 panic 由后续 poll/drop 路径传播。
    /// 注意事项：包装本身不启动或等待任务，不保证 F 可跨线程；实际 Send 能力仍取决于 F 及捕获数据。
    /// 任务 ID 跨调度线程保持不变，内部 Future 完成清理后才允许登记为结束。
    pub fn track_future<F: Future>(&self, name: impl Into<String>, future: F) -> TrackedFuture<F> {
        TrackedFuture {
            future: Some(Box::pin(future)),
            logging_context: crate::logging::capture_context(),
            lifetime: TaskLifetime {
                runtime: self.clone(),
                record: self.record(name.into(), false),
            },
        }
    }
}

/// 受管线程的创建结果：owner 身份 + 线程句柄。
pub struct Spawned<H> {
    /// 线程的稳定身份，用于锁归属与存活检查。
    pub owner: ExecutionOwner,
    /// 底层线程 JoinHandle，用于等待线程结束。
    pub handle: H,
}

/// 受管线程的完成守卫：线程闭包结束时把状态置为 Completed 或 Panicked。
struct ThreadCompletion {
    /// 结束状态写回的所属运行时。
    runtime: ExecutionRuntime,
    /// 对应当前受管线程业务闭包的共享记录。
    record: Arc<Record>,
}

impl Drop for ThreadCompletion {
    /// 功能：在受管线程业务闭包返回或展开 panic 时结束其执行记录。
    /// 入参：self: &mut ThreadCompletion，为当前子线程的完成守卫。
    /// 返回值：无；当前线程正在 panic 或记录已标记异常时尝试置 Panicked，否则置 Completed。
    /// 异常：不捕获原业务 panic；登记簿锁中毒按 finish 的恢复路径处理。
    /// 注意事项：只登记结果，不关闭其他资源或强制结束线程；先前已存在结束态时不会被二次结束覆盖。
    fn drop(&mut self) {
        let state = if thread::panicking() || self.record.panicked.load(Ordering::Acquire) {
            ExecutionState::Panicked
        } else {
            ExecutionState::Completed
        };
        self.runtime.0.finish(&self.record, state);
    }
}

/// 任务的生命周期：record 所有权 + 运行时的引用。
///
/// TaskLifetime 负责结束记录的职责，Record 本身仍由登记簿与其他守卫通过 Arc 共享；正常完成走 complete，析构兜底
/// 按"被丢弃未完成"处理为 Cancelled（或 Panicked）。
struct TaskLifetime {
    /// 任务所属的登记簿句柄，保证工作期间运行时仍存活。
    runtime: ExecutionRuntime,
    /// 本任务的共享状态，生命周期守卫负责通知其真正结束。
    record: Arc<Record>,
}

impl TaskLifetime {
    /// 功能：把预登记的任务从排队态推进到正在运行，供存活查询展示。
    /// 入参：self: &TaskLifetime 为任务记录的生命周期拥有者。
    /// 返回值：无；仅当当前状态恰为 Starting 时 CAS 写入 Running，其他状态保持不变。
    /// 异常：原子比较更新不返回业务错误，CAS 失败被正常忽略。
    /// 注意事项：不会把已结束任务重新启动，也不执行实际 work 或 Future；状态和真实工作推进由包装流程协调。
    fn start(&self) {
        let _ = self.record.state.compare_exchange(
            ExecutionState::Starting as u8,
            ExecutionState::Running as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
    /// 功能：在任务正常工作和必要清理结束后登记成功完成。
    /// 入参：self: &TaskLifetime，为正在收尾的任务生命周期。
    /// 返回值：无；通过登记簿 finish 尝试置 Completed 并加入结束历史。
    /// 异常：不返回 Result，登记簿锁中毒由 finish 恢复。
    /// 注意事项：已有结束态不被覆盖；调用方须保证实际工作和需保护的清理已经结束，不能提前让锁回收方看到完成。
    fn complete(&self) {
        self.runtime
            .0
            .finish(&self.record, ExecutionState::Completed);
    }
}

impl Drop for TaskLifetime {
    /// 功能：在任务生命周期所有者释放时兜底登记取消或异常，防止未执行/未完成工作永远显示存活。
    /// 入参：self: &mut TaskLifetime，持有运行时及任务记录的共享引用。
    /// 返回值：无；正在 panic 或此前记录 panic 时尝试置 Panicked，否则尝试置 Cancelled。
    /// 异常：不吞原 panic，finish 的锁中毒按恢复处理。
    /// 注意事项：正常 complete 已设结束态时这里不覆盖；内部工作或 Future 应先析构，才能以此结束状态允许租约回收。
    fn drop(&mut self) {
        let state = if thread::panicking() || self.record.panicked.load(Ordering::Acquire) {
            ExecutionState::Panicked
        } else {
            ExecutionState::Cancelled
        };
        self.runtime.0.finish(&self.record, state);
    }
}

/// 把任务压入当前线程任务栈的作用域守卫。
struct ActiveScope {
    /// 此次压栈的任务记录，析构时按 owner 找到并移除对应栈项。
    record: Arc<Record>,
    // 线程局部上下文只允许在建立它的线程上释放。
    // PhantomData<Rc<()>> 使该类型 !Send，防止被挪到其他线程破坏任务栈。
    _not_send: PhantomData<Rc<()>>,
}

impl ActiveScope {
    /// 功能：临时把一个任务设置为当前线程的活动任务，让内层代码的 owner 查询指向它。
    /// 入参：lifetime: &TaskLifetime 为已经登记的任务，借其 record 生成栈项。
    /// 返回值：ActiveScope，持有记录引用和禁止跨线程发送的标记。
    /// 异常：TLS 访问失败或任务栈 RefCell 借用冲突可能 panic，不返回 Result。
    /// 注意事项：每个栈元素代表一层活动任务，最末项为当前归属；守卫必须在创建线程释放，不负责单独结束任务记录。
    fn new(lifetime: &TaskLifetime) -> Self {
        TASK_STACK.with(|stack| stack.borrow_mut().push(lifetime.record.owner.clone()));
        Self {
            record: Arc::clone(&lifetime.record),
            _not_send: PhantomData,
        }
    }
}

impl Drop for ActiveScope {
    /// 功能：离开一次活动调度片段时恢复任务栈，并在异常展开时先记下 panic 原因。
    /// 入参：self: &mut ActiveScope，为当前线程的栈守卫。
    /// 返回值：无；从栈尾向前找到本 owner 并移除，正在 panic 时把 record.panicked 置 true。
    /// 异常：TLS 已不可访问时 try_with 错误被忽略；RefCell 借用冲突仍可能 panic。
    /// 注意事项：并非立即把记录置为结束态，Future 或闭包的清理可能尚在执行；实际结束由 TaskLifetime/正常完成路径处理。
    fn drop(&mut self) {
        let _ = TASK_STACK.try_with(|stack| {
            let mut stack = stack.borrow_mut();
            // 从栈中找并移除本任务的 owner（按后进先出语义一般在栈顶）
            if let Some(index) = stack.iter().rposition(|owner| *owner == self.record.owner) {
                stack.remove(index);
            }
        });
        if thread::panicking() {
            // Future 可能还在展开或执行析构；先记原因，实际销毁后再宣告结束。
            self.record.panicked.store(true, Ordering::Release);
        }
    }
}

/// 同步任务作用域；正常离开作用域视为完成，取消需显式调用 cancel。
///
/// 使用场景：一段必须显式开始/结束的同步业务工作（如持锁执行导入导出），
/// 用 `enter_task` 进入；正常返回后析构即 Completed，中途放弃可调 cancel。
pub struct TaskScope {
    /// 活跃作用域（任务栈守卫）；取消时取出。
    scope: Option<ActiveScope>,
    /// 任务生命周期；析构兜底结束任务。
    lifetime: TaskLifetime,
}

impl TaskScope {
    /// 功能：取得同步任务的稳定身份，供本作用域内申请租约和查询存活使用。
    /// 入参：self: &TaskScope 为尚持有生命周期的同步任务。
    /// 返回值：&ExecutionOwner，借用该任务记录的身份。
    /// 异常：不查询数据库或操作系统，不返回错误。
    /// 注意事项：身份引用不延长任务工作生命周期；不得把“能读到身份”当作租约已经申请或仍有效。
    pub fn owner(&self) -> &ExecutionOwner {
        &self.lifetime.record.owner
    }
    /// 功能：将当前同步任务明确登记为取消，并消费作用域，恢复当前线程先前任务栈。
    /// 入参：self: TaskScope，按值取得整个任务作用域。
    /// 返回值：无；第一次结束有效时记录为 Cancelled，随后正常析构不覆盖该结束态。
    /// 异常：不返回 Result，登记簿锁中毒按 finish 恢复。
    /// 注意事项：不会中断额外启动的线程或 I/O；调用方只能在本段工作确已停止时取消该身份，避免锁过早回收。
    pub fn cancel(self) {
        self.lifetime
            .runtime
            .0
            .finish(&self.lifetime.record, ExecutionState::Cancelled);
    }
}

impl Drop for TaskScope {
    /// 功能：同步任务作用域结束时移除活动栈项，并在非 panic 路径登记正常完成。
    /// 入参：self: &mut TaskScope，包含可选活动栈守卫和任务生命周期。
    /// 返回值：无；先取出并析构 ActiveScope，再在非异常展开路径调用 complete。
    /// 异常：不捕获业务 panic；异常原因由活动栈守卫标记，后续 TaskLifetime 析构记录 Panicked。
    /// 注意事项：显式 cancel 已设置的结束态不被完成动作覆盖；正常提前离开作用域同样算 Completed，不自动理解业务是否成功。
    fn drop(&mut self) {
        self.scope.take();
        if !thread::panicking() {
            self.lifetime.complete();
        }
    }
}

/// 被跟踪的 Future：poll 时自动携带任务身份与日志上下文。
pub struct TrackedFuture<F: Future> {
    /// 被包装的 Future；完成后取出并销毁。
    future: Option<Pin<Box<F>>>,
    /// 创建时捕获的日志上下文（请求关联等），每次 poll/析构时恢复。
    logging_context: crate::logging::LogContext,
    /// 任务生命周期：完成/丢弃时更新任务状态。
    lifetime: TaskLifetime,
}

impl<F: Future> Drop for TrackedFuture<F> {
    /// 功能：在异步包装被释放时，先沿原日志上下文销毁内部 Future，再由生命周期字段登记结束。
    /// 入参：self: &mut `TrackedFuture<F>`，包含可选内部 Future、日志上下文和任务生命周期。
    /// 返回值：无；取出并析构 future，字段析构随后处理未完成任务的 Cancelled/Panicked 兜底。
    /// 异常：内部 Future 的析构 panic 不在此捕获；日志上下文恢复也沿各自守卫约束执行。
    /// 注意事项：Ready 路径可能已取走内部 Future，此时无重复析构；仅丢弃外部等待句柄是否释放包装取决于所用调度器。
    fn drop(&mut self) {
        let _logging = self.logging_context.attach();
        // 未完成任务的清理也使用任务原有的关联号和脱敏上下文。
        drop(self.future.take());
    }
}

impl<F: Future> TrackedFuture<F> {
    /// 功能：取得异步任务在跨线程调度前后保持不变的身份，供锁归属和诊断使用。
    /// 入参：self: &`TrackedFuture<F>` 为被跟踪的异步任务。
    /// 返回值：&ExecutionOwner，借用 TaskLifetime 中记录的任务身份。
    /// 异常：不执行 Future 或查询外部资源，不返回错误。
    /// 注意事项：返回身份不启动任务；Starting 阶段也能取到身份，执行状态须另向登记簿查询。
    pub fn owner(&self) -> &ExecutionOwner {
        &self.lifetime.record.owner
    }
}

impl<F: Future> Future for TrackedFuture<F> {
    type Output = F::Output;

    /// 功能：在异步调度器每次推进任务时恢复任务归属和日志环境，并将真实结束时点登记给租约回收方。
    /// 入参：self: `Pin<&mut TrackedFuture<F>>` 为固定位置的包装可变借用；cx: &mut Context 为调度器提供的唤醒上下文，原样交给内部 Future。
    /// 返回值：内部 Pending 则返回 Pending 并恢复本线程先前环境；内部 Ready(T) 时先销毁内部 Future、登记 Completed，再返回 Ready(T)。
    /// 异常：已结束或已标记 panic 的任务再次 poll 会断言 panic；内部 Future 缺失会 expect panic；内部 poll/析构 panic 原样传播。
    /// 注意事项：每次 poll 只短暂建立当前线程任务栈，不把同步 ActiveScope 跨 await 保存；F::Output 的后续生命周期不受本记录跟踪。
    /// Future 的清理完成前不会主动标记 Completed，避免外部依据登记过早回收仍被清理代码使用的锁。
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let _logging = this.logging_context.attach();
        // 已结束（或已 panic）的任务不能再被调度：调用方逻辑错误
        assert!(
            !this.lifetime.record.state().is_finished()
                && !this.lifetime.record.panicked.load(Ordering::Acquire),
            "不能再次调度已结束的任务"
        );
        this.lifetime.runtime.register_thread(None);
        this.lifetime.start();
        let _scope = ActiveScope::new(&this.lifetime);
        match this.future.as_mut().expect("任务已结束").as_mut().poll(cx) {
            Poll::Ready(value) => {
                // 必须完成 Future 的析构，才能让其他进程据此提前回收锁。
                // Future 析构可能还有清理逻辑（如释放锁），完成后才宣告任务结束。
                drop(this.future.take());
                this.lifetime.complete();
                Poll::Ready(value)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests;
