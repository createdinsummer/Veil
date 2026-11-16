//! 执行状态服务：组合 core 的进程检查、本进程登记及带身份验证的本机查询。
//!
//! 文件级租约的持有者是「执行身份」（进程/线程/任务）。判断一个持有者
//! 是否还活着，需要三件能力，本模块把它们组合成统一入口：
//! 1. 本进程登记簿（core ExecutionRuntime）：本进程的线程/任务由自己回答；
//! 2. 跨进程检查（core 系统检查）：确认对方进程实例是否已退出；
//! 3. 本机回环查询（transport）：通过对方进程的带凭证端点，委托它查询
//!    自己登记簿里的线程/任务。
//! 查询规则：只有确认原持有者已结束才返回 Dead；缺记录、超时、权限不足
//! 一律返回 Unknown，按租约到期处理，绝不无凭据地回收。

mod transport;

pub use transport::{ExecutionEndpoint, ExecutionServer};
// 从 core 再导出执行相关的公共类型，业务层引用时无需直接依赖 core。
pub use veil_core::execution::{
    ExecutionContext, ExecutionInfo, ExecutionOwner, ExecutionRuntime, ExecutionState, Liveness,
    ProcessIdentity, Spawned, TaskScope, TrackedFuture,
};

use std::future::Future;
use std::io;
use std::thread::JoinHandle;
use std::time::Duration;

/// 执行服务：本进程的执行身份与状态查询的统一入口。
///
/// 内部持有一个 core 执行运行时（线程/任务登记簿）；调用方持有本句柄的
/// 同时监听查询入口（listen），其他进程才能查询本进程的任务状态。
#[derive(Clone)]
pub struct ExecutionService {
    // 本进程共享登记簿：保存执行身份及状态，不包含远程进程的完整任务列表。
    runtime: ExecutionRuntime,
}

impl ExecutionService {
    /// 取得共享本进程执行登记簿的服务入口，用于给操作租约提供持有者身份。
    /// 入参：无。
    /// 返回值：成功返回服务句柄；多个句柄共享进程运行时。
    /// 异常：无法初始化进程身份或运行时资源时返回 io::Error。
    /// 注意事项：本方法不启动查询监听；跨进程查询需另行持有 listen 返回的服务端。
    pub fn global() -> io::Result<Self> {
        Ok(Self {
            runtime: ExecutionRuntime::global()?,
        })
    }

    /// 借用执行登记簿，以便接入自定义线程或异步任务调度。
    /// 入参：self 为当前服务。
    /// 返回值：与 self 同寿命的 ExecutionRuntime 引用。
    /// 异常：无错误返回。
    /// 注意事项：不创建新运行时，也不启动监听。
    pub fn runtime(&self) -> &ExecutionRuntime {
        &self.runtime
    }

    /// 开放本进程的身份查询入口，供其他本机租约使用者判断持有者状态。
    /// 入参：self 为提供登记簿的执行服务。
    /// 返回值：成功返回含回环端点的 ExecutionServer。
    /// 异常：绑定端口、生成随机凭证、配置套接字或创建线程失败时返回 io::Error。
    /// 注意事项：调用方须保留返回句柄；释放时通知并等待监听线程退出。端点包含访问凭证，仅传给可信调用方。
    pub fn listen(&self) -> io::Result<ExecutionServer> {
        transport::listen(self.clone())
    }

    /// 核对属于本进程的租约持有者是否仍在登记簿中有效。
    /// 入参：owner（&ExecutionOwner）为进程、线程或任务身份，包含原进程实例信息。
    /// 返回值：Alive 表示运行时确认有效，Dead 表示确认结束；不属于本进程或缺乏证据返回 Unknown。
    /// 异常：无 Result 错误返回。
    /// 注意事项：本方法仅询问本地运行时，不联系其他进程；登记有效不表示业务工作正在占用 CPU。
    pub fn check_local(&self, owner: &ExecutionOwner) -> Liveness {
        self.runtime.check_local(owner)
    }

    /// 借助系统信息核对原进程实例，避免把复用同一进程号的新进程认作持有者。
    /// 入参：process（&ProcessIdentity）为需要检查的进程实例。
    /// 返回值：Alive/Dead 分别表示已确认存活/结束；系统信息不足返回 Unknown。
    /// 异常：系统查询失败归入 Unknown，不向外抛出错误。
    /// 注意事项：只判断进程，不证明其中某个任务仍有效；结果是查询时的观察。
    pub fn check_process(&self, process: &ProcessIdentity) -> Liveness {
        self.runtime.check_process(process)
    }

    /// 为租约回收提供保守的持有者存活结论：证据不足时保持 Unknown。
    /// 入参：owner（&ExecutionOwner）为原持有者；endpoint（`Option<&ExecutionEndpoint>`）为可选的原进程查询入口；timeout（Duration）为远程连接、发包与收包共用预算。
    /// 返回值：本进程查本地登记簿；远程进程确认结束返回 Dead，进程级身份确认存活返回 Alive；其余向匹配端点查询，缺端点或通信失败返回 Unknown。
    /// 异常：通信和系统检查错误不向外返回，按 Unknown 处理。
    /// 注意事项：timeout 仅约束远程传输，不覆盖前面的本地/系统检查；Unknown 需交由租约到期规则处理，不能当成 Dead。
    pub fn check_owner(
        &self,
        owner: &ExecutionOwner,
        endpoint: Option<&ExecutionEndpoint>,
        timeout: Duration,
    ) -> Liveness {
        // 自己进程的身份：直接查本进程登记簿。
        if owner.process() == self.runtime.process() {
            return self.check_local(owner);
        }
        // 对方进程已确认退出：无论线程/任务登记如何，都可以判定 Dead。
        let process_status = self.check_process(owner.process());
        if process_status == Liveness::Dead {
            return Liveness::Dead;
        }
        // 只关心进程存活的进程级身份：进程活着就是 Alive。
        if matches!(owner, ExecutionOwner::Process { .. }) && process_status == Liveness::Alive {
            return Liveness::Alive;
        }
        // 线程/任务身份：必须有匹配该进程实例的端点才能委托查询。
        match endpoint {
            Some(endpoint) if endpoint.process == *owner.process() => {
                transport::query(endpoint, owner, timeout).unwrap_or(Liveness::Unknown)
            }
            _ => Liveness::Unknown,
        }
    }

    /// 为当前同步业务执行段登记任务身份，使租约能跟随此作用域结束。
    /// 入参：name（impl `Into<String>`）为便于诊断的任务名称，不是唯一标识。
    /// 返回值：TaskScope 守卫，持有任务身份并在释放时结束登记。
    /// 异常：无 Result 错误返回。
    /// 注意事项：守卫不能跨线程移动；跨异步挂起点应采用被跟踪的 Future，避免混淆线程上的当前身份。
    pub fn enter_task(&self, name: impl Into<String>) -> TaskScope {
        self.runtime.enter_task(name)
    }

    /// 创建已登记身份的业务线程，供其工作申请可追踪的操作租约。
    /// 入参：name（impl `Into<String>`）为线程诊断名称；work（FnOnce(ExecutionContext) -> T）为接收执行上下文的工作，闭包和结果 T 均须 Send + 'static。
    /// 返回值：成功返回 `Spawned<JoinHandle<T>>`，同时包含登记身份和用于等待业务结果的线程句柄。
    /// 异常：操作系统无法创建线程时返回 io::Error；工作中的 panic 在线程句柄 join 时体现。
    /// 注意事项：身份在工作结束时登记完成；丢弃 JoinHandle 不等于取消工作。
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

    /// 先为阻塞业务登记任务，再交由调用方选择的线程池执行。
    /// 入参：name 为诊断名称；work（FnOnce(ExecutionContext) -> T，Send + 'static）为业务工作；spawn 接收可执行一次的装箱闭包并返回调度句柄 H，结果 T 须 Send + 'static。
    /// 返回值：`Spawned<H>`，包含提前生成的身份和调度器返回的句柄。
    /// 异常：无 Result 返回；调度器和工作自身的 panic 不在本方法内捕获。
    /// 注意事项：取消外部等待不必然停止阻塞工作；只有包装工作完成或被释放，才结束其登记。
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

    /// 把异步业务包装为具有稳定登记身份的 Future，再交给调用方调度器。
    /// 入参：name 为诊断名称；future（F: Future）为业务异步计算；spawn（FnOnce(`TrackedFuture<F>`) -> H）接收跟踪包装并返回句柄 H。
    /// 返回值：`Spawned<H>`，身份不会因 Future 被不同线程轮询而改变。
    /// 异常：无 Result 返回；调度器及 Future 自身的 panic 不在此捕获。
    /// 注意事项：本方法不提供异步运行环境；执行和取消由调度器负责，包装 Future 完成或释放时结束登记。
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
