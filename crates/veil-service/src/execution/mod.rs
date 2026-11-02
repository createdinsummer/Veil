//! 执行状态服务：组合 core 的进程检查、本进程登记及带身份验证的本机查询。

mod transport;

pub use transport::{ExecutionEndpoint, ExecutionServer};
pub use veil_core::execution::{
    ExecutionContext, ExecutionInfo, ExecutionOwner, ExecutionRuntime, ExecutionState, Liveness,
    ProcessIdentity, Spawned, TaskScope, TrackedFuture,
};

use std::future::Future;
use std::io;
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Clone)]
pub struct ExecutionService {
    runtime: ExecutionRuntime,
}

impl ExecutionService {
    pub fn global() -> io::Result<Self> {
        Ok(Self {
            runtime: ExecutionRuntime::global()?,
        })
    }

    pub fn runtime(&self) -> &ExecutionRuntime {
        &self.runtime
    }

    /// 调用方持有服务句柄以维持查询入口；不将服务变成依赖全局后台守护进程的架构。
    pub fn listen(&self) -> io::Result<ExecutionServer> {
        transport::listen(self.clone())
    }

    /// 处理其他进程的查询，只回答当前进程登记过的身份。
    pub fn check_local(&self, owner: &ExecutionOwner) -> Liveness {
        self.runtime.check_local(owner)
    }

    pub fn check_process(&self, process: &ProcessIdentity) -> Liveness {
        self.runtime.check_process(process)
    }

    /// 不对目标工作线程发 ping；目标进程的控制入口查询自己的登记簿。
    /// timeout 覆盖连接及整次响应等待；无响应绝不推断为线程或任务退出。
    pub fn check_owner(
        &self,
        owner: &ExecutionOwner,
        endpoint: Option<&ExecutionEndpoint>,
        timeout: Duration,
    ) -> Liveness {
        if owner.process() == self.runtime.process() {
            return self.check_local(owner);
        }
        let process_status = self.check_process(owner.process());
        if process_status == Liveness::Dead {
            return Liveness::Dead;
        }
        if matches!(owner, ExecutionOwner::Process { .. }) && process_status == Liveness::Alive {
            return Liveness::Alive;
        }
        match endpoint {
            Some(endpoint) if endpoint.process == *owner.process() => {
                transport::query(endpoint, owner, timeout).unwrap_or(Liveness::Unknown)
            }
            _ => Liveness::Unknown,
        }
    }

    pub fn enter_task(&self, name: impl Into<String>) -> TaskScope {
        self.runtime.enter_task(name)
    }

    pub fn spawn_thread<F, T>(
        &self,
        name: impl Into<String>,
        work: F,
    ) -> io::Result<Spawned<JoinHandle<T>>>
    where
        F: FnOnce(ExecutionContext) -> T + Send + 'static,
        T: Send + 'static,
    {
        self.runtime.spawn_thread(name, work)
    }

    /// 调度器由调用方传入，可接入 Tauri、Tokio 或其他线程池。
    /// 外层等待任务被取消不改变本阻塞任务的登记状态。
    pub fn spawn_blocking<F, T, S, H>(
        &self,
        name: impl Into<String>,
        work: F,
        spawn: S,
    ) -> Spawned<H>
    where
        F: FnOnce(ExecutionContext) -> T + Send + 'static,
        T: Send + 'static,
        S: FnOnce(Box<dyn FnOnce() -> T + Send>) -> H,
    {
        let (owner, work) = self.runtime.prepare_blocking(name, work);
        Spawned {
            owner,
            handle: spawn(Box::new(work)),
        }
    }

    /// 由调度器接收已经完成登记的 Future，归属不随任务的调度线程改变。
    pub fn spawn_async<F, S, H>(&self, name: impl Into<String>, future: F, spawn: S) -> Spawned<H>
    where
        F: Future,
        S: FnOnce(TrackedFuture<F>) -> H,
    {
        let future = self.runtime.track_future(name, future);
        let owner = future.owner().clone();
        Spawned {
            owner,
            handle: spawn(future),
        }
    }
}
