//! 进程、线程与异步任务的统一登记。这里只维护执行生命周期，不负责数据库或 IPC。
//!
//! 线程身份在线程退出前保持不变；任务身份不随调度线程变化。未知身份不能推断为死亡。

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

const RETAINED_FINISHED: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    Alive,
    Dead,
    Unknown,
}

/// 随机实例 ID 区分进程重启；系统标识用于识别被复用的 PID。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub instance_id: String,
    pub system_identity: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionOwner {
    Process {
        process: ProcessIdentity,
    },
    Thread {
        process: ProcessIdentity,
        thread_id: String,
    },
    Task {
        process: ProcessIdentity,
        task_id: String,
    },
}

impl ExecutionOwner {
    pub fn process(&self) -> &ProcessIdentity {
        match self {
            Self::Process { process }
            | Self::Thread { process, .. }
            | Self::Task { process, .. } => process,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum ExecutionState {
    Starting,
    Running,
    Completed,
    Cancelled,
    Panicked,
}

impl ExecutionState {
    fn from_byte(value: u8) -> Self {
        match value {
            0 => Self::Starting,
            1 => Self::Running,
            2 => Self::Completed,
            3 => Self::Cancelled,
            _ => Self::Panicked,
        }
    }

    fn is_finished(self) -> bool {
        !matches!(self, Self::Starting | Self::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionInfo {
    pub owner: ExecutionOwner,
    pub name: String,
    pub state: ExecutionState,
}

/// 每次调用查询当前调度线程；任务身份在跨线程调度时保持不变。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionContext {
    pub thread: ExecutionOwner,
    pub task: Option<ExecutionOwner>,
}

impl ExecutionContext {
    pub fn owner(&self) -> &ExecutionOwner {
        self.task.as_ref().unwrap_or(&self.thread)
    }
}

struct Record {
    owner: ExecutionOwner,
    name: String,
    state: AtomicU8,
    panicked: AtomicBool,
}

impl Record {
    fn state(&self) -> ExecutionState {
        ExecutionState::from_byte(self.state.load(Ordering::Acquire))
    }
}

#[derive(Default)]
struct Registry {
    records: HashMap<ExecutionOwner, Arc<Record>>,
    finished: VecDeque<ExecutionOwner>,
}

struct Inner {
    process: ProcessIdentity,
    next_id: AtomicU64,
    registry: Mutex<Registry>,
}

impl Inner {
    fn finish(&self, record: &Record, state: ExecutionState) {
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

#[derive(Clone)]
pub struct ExecutionRuntime(Arc<Inner>);

thread_local! {
    static LOCAL_THREAD: RefCell<Option<ThreadRegistration>> = const { RefCell::new(None) };
    static TASK_STACK: RefCell<Vec<ExecutionOwner>> = const { RefCell::new(Vec::new()) };
}

struct ThreadRegistration {
    runtime: Weak<Inner>,
    record: Arc<Record>,
}

impl Drop for ThreadRegistration {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.upgrade() {
            runtime.finish(&self.record, ExecutionState::Completed);
        }
    }
}

impl ExecutionRuntime {
    /// 每个进程只有一份登记簿，service 实例与不同调度器共享它。
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
        if inner.process.pid != std::process::id() {
            return Err(io::Error::other(
                "fork 后不能复用父进程的执行登记，请使用独立启动的进程",
            ));
        }
        Ok(Self(Arc::clone(inner)))
    }

    pub fn process(&self) -> &ProcessIdentity {
        &self.0.process
    }

    pub fn process_owner(&self) -> ExecutionOwner {
        ExecutionOwner::Process {
            process: self.process().clone(),
        }
    }

    pub fn check_process(&self, identity: &ProcessIdentity) -> Liveness {
        if identity.pid == self.process().pid {
            return if identity == self.process() {
                Liveness::Alive
            } else {
                Liveness::Dead
            };
        }
        process::check(identity)
    }

    /// 只回答本进程已登记的身份；缺少记录或查询其他进程时返回 Unknown。
    pub fn check_local(&self, owner: &ExecutionOwner) -> Liveness {
        if owner.process() != self.process() {
            return Liveness::Unknown;
        }
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

    fn record(&self, name: String, is_thread: bool) -> Arc<Record> {
        let next = self
            .0
            .next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("执行实例编号耗尽");
        let id = format!("{}-{next:016x}", self.process().instance_id);
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
        self.0
            .registry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .records
            .insert(owner, Arc::clone(&record));
        record
    }

    fn register_thread(&self, reserved: Option<Arc<Record>>) -> ExecutionOwner {
        LOCAL_THREAD.with(|local| {
            let mut local = local.borrow_mut();
            if let Some(registration) = local.as_ref() {
                return registration.record.owner.clone();
            }
            let record = reserved.unwrap_or_else(|| {
                self.record(
                    thread::current()
                        .name()
                        .unwrap_or("外部运行线程")
                        .to_owned(),
                    true,
                )
            });
            record
                .state
                .store(ExecutionState::Running as u8, Ordering::Release);
            let owner = record.owner.clone();
            *local = Some(ThreadRegistration {
                runtime: Arc::downgrade(&self.0),
                record,
            });
            owner
        })
    }

    /// 接入主线程、线程池等由外部运行时创建的线程；重复调用不会重复登记。
    pub fn current_context(&self) -> ExecutionContext {
        ExecutionContext {
            thread: self.register_thread(None),
            task: TASK_STACK.with(|stack| stack.borrow().last().cloned()),
        }
    }

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

    /// 新线程必须先登记，再执行业务闭包。返回值携带可用于锁归属的身份。
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
        let record = self.record(name.clone(), true);
        let owner = record.owner.clone();
        let runtime = self.clone();
        let child_record = Arc::clone(&record);
        let result = thread::Builder::new().name(name).spawn(move || {
            runtime.register_thread(Some(Arc::clone(&child_record)));
            let completion = ThreadCompletion {
                runtime: runtime.clone(),
                record: child_record,
            };
            let result = work(runtime.current_context());
            drop(completion);
            result
        });
        match result {
            Ok(handle) => Ok(Spawned { owner, handle }),
            Err(error) => {
                self.0.finish(&record, ExecutionState::Cancelled);
                Err(error)
            }
        }
    }

    /// 交给外部线程池执行；只有实际工作结束或闭包在执行前被丢弃才结束任务。
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
        (owner, move || {
            lifetime.runtime.register_thread(None);
            lifetime.start();
            let _scope = ActiveScope::new(&lifetime);
            let result = work(lifetime.runtime.current_context());
            lifetime.complete();
            result
        })
    }

    /// 与具体调度器无关的 Future 包装。任务跨线程调度不会改变 owner。
    pub fn track_future<F: Future>(&self, name: impl Into<String>, future: F) -> TrackedFuture<F> {
        TrackedFuture {
            future: Some(Box::pin(future)),
            lifetime: TaskLifetime {
                runtime: self.clone(),
                record: self.record(name.into(), false),
            },
        }
    }
}

pub struct Spawned<H> {
    pub owner: ExecutionOwner,
    pub handle: H,
}

struct ThreadCompletion {
    runtime: ExecutionRuntime,
    record: Arc<Record>,
}

impl Drop for ThreadCompletion {
    fn drop(&mut self) {
        let state = if thread::panicking() || self.record.panicked.load(Ordering::Acquire) {
            ExecutionState::Panicked
        } else {
            ExecutionState::Completed
        };
        self.runtime.0.finish(&self.record, state);
    }
}

struct TaskLifetime {
    runtime: ExecutionRuntime,
    record: Arc<Record>,
}

impl TaskLifetime {
    fn start(&self) {
        let _ = self.record.state.compare_exchange(
            ExecutionState::Starting as u8,
            ExecutionState::Running as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
    fn complete(&self) {
        self.runtime
            .0
            .finish(&self.record, ExecutionState::Completed);
    }
}

impl Drop for TaskLifetime {
    fn drop(&mut self) {
        let state = if thread::panicking() || self.record.panicked.load(Ordering::Acquire) {
            ExecutionState::Panicked
        } else {
            ExecutionState::Cancelled
        };
        self.runtime.0.finish(&self.record, state);
    }
}

struct ActiveScope {
    record: Arc<Record>,
    // 线程局部上下文只允许在建立它的线程上释放。
    _not_send: PhantomData<Rc<()>>,
}

impl ActiveScope {
    fn new(lifetime: &TaskLifetime) -> Self {
        TASK_STACK.with(|stack| stack.borrow_mut().push(lifetime.record.owner.clone()));
        Self {
            record: Arc::clone(&lifetime.record),
            _not_send: PhantomData,
        }
    }
}

impl Drop for ActiveScope {
    fn drop(&mut self) {
        let _ = TASK_STACK.try_with(|stack| {
            let mut stack = stack.borrow_mut();
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
pub struct TaskScope {
    scope: Option<ActiveScope>,
    lifetime: TaskLifetime,
}

impl TaskScope {
    pub fn owner(&self) -> &ExecutionOwner {
        &self.lifetime.record.owner
    }
    pub fn cancel(self) {
        self.lifetime
            .runtime
            .0
            .finish(&self.lifetime.record, ExecutionState::Cancelled);
    }
}

impl Drop for TaskScope {
    fn drop(&mut self) {
        self.scope.take();
        if !thread::panicking() {
            self.lifetime.complete();
        }
    }
}

pub struct TrackedFuture<F: Future> {
    future: Option<Pin<Box<F>>>,
    lifetime: TaskLifetime,
}

impl<F: Future> TrackedFuture<F> {
    pub fn owner(&self) -> &ExecutionOwner {
        &self.lifetime.record.owner
    }
}

impl<F: Future> Future for TrackedFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
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
