//! 执行身份登记簿的单元测试。
//!
//! 覆盖的核心语义（与业务规则一一对应）：
//! - 身份比对保守：未知记录/外来进程一律 Unknown，不误报死亡。
//! - 线程与任务生命周期独立：嵌套任务按栈进出，取消/完成互不影响。
//! - 受管线程在执行前可见、join 后判死；panic 记为 Panicked。
//! - Future 跨线程调度身份不变；未完成即丢弃记 Cancelled。
//! - Future 析构完成前不宣告任务结束（其他进程不能提前回收锁）。
//! - 日志上下文随调度线程恢复。
use super::*;
use std::sync::mpsc;
use std::task::Waker;
use std::time::Duration;

/// 功能：同步推进测试 Future 一次，以观察包装器的身份、状态和清理边界。
/// 入参：future: &mut F，为实现 Future + Unpin 的测试对象，允许借用后临时固定位置。
/// 返回值：`Poll<F::Output>`，原样返回此次内部调度结果。
/// 异常：内部 poll 的 panic 原样传播，使调用测试失败或由用例外层捕获。
/// 注意事项：使用 noop waker，不会自动安排后续调度；Pending 后需用例手动再次 poll，不是完整异步执行器。
fn poll_once<F: Future + Unpin>(future: &mut F) -> Poll<F::Output> {
    Pin::new(future).poll(&mut Context::from_waker(Waker::noop()))
}

/// 验证本进程身份为 Alive、旧实例（PID 相同但实例 ID 不同）为 Dead，
/// 以及未登记记录与外来进程查询都保守返回 Unknown。
#[test]
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn process_identity_and_unknown_records_are_conservative() {
    let runtime = ExecutionRuntime::global().unwrap();
    assert_eq!(runtime.check_process(runtime.process()), Liveness::Alive);
    // 改掉实例 ID 模拟"同 PID 但已是重启后的进程"：应判 Dead
    let mut old = runtime.process().clone();
    old.instance_id.push_str("-old");
    assert_eq!(runtime.check_process(&old), Liveness::Dead);
    // 本进程内未登记的线程：不猜测，Unknown
    let unknown = ExecutionOwner::Thread {
        process: runtime.process().clone(),
        thread_id: "unregistered".into(),
    };
    assert_eq!(runtime.check_local(&unknown), Liveness::Unknown);
    // 其他进程的身份本进程无法回答：Unknown
    let foreign = ExecutionOwner::Process { process: old };
    assert_eq!(runtime.check_local(&foreign), Liveness::Unknown);
}

/// 验证线程与任务生命周期独立：嵌套任务按栈进入/退出，内层完成不影响外层，
/// 取消外层任务后登记簿状态为 Cancelled，线程身份全程不变。
#[test]
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn thread_and_task_lifetimes_are_independent() {
    let runtime = ExecutionRuntime::global().unwrap();
    let before = runtime.current_context();
    // 进入两层嵌套任务
    let outer = runtime.enter_task("外层任务");
    let outer_owner = outer.owner().clone();
    let inner = runtime.enter_task("内层任务");
    let inner_owner = inner.owner().clone();
    let inside = runtime.current_context();
    // 线程身份不变；当前任务 = 最内层任务
    assert_eq!(inside.thread, before.thread);
    assert_eq!(inside.task.as_ref(), Some(&inner_owner));
    drop(inner);
    // 内层任务结束后，当前任务回到外层；内层已判死
    assert_eq!(runtime.current_context().task.as_ref(), Some(&outer_owner));
    assert_eq!(runtime.check_local(&inner_owner), Liveness::Dead);
    // 显式取消外层任务
    outer.cancel();
    assert_eq!(
        runtime.info(&outer_owner).unwrap().state,
        ExecutionState::Cancelled
    );
    // 任务栈清空后恢复无任务状态；线程仍然活着
    assert_eq!(runtime.current_context().task, before.task);
    assert_eq!(runtime.check_local(&before.thread), Liveness::Alive);
}

/// 验证受管线程在工作开始前即可被查询为 Alive，join 后判 Dead；
/// panic 的线程登记为 Panicked。
#[test]
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
/// 异常：测试准备或断言失败会 panic；场景中预期的错误由测试体捕获或核对，不作为生产错误返回。
fn managed_thread_is_visible_before_work_and_dead_after_join() {
    let runtime = ExecutionRuntime::global().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();
    let worker = runtime
        .spawn_thread("受管线程", move |context| {
            // 线程一开始就把自己的线程身份发给测试线程
            ready_tx.send(context.thread).unwrap();
            stop_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        })
        .unwrap();
    // 预登记的 owner 与线程内报告的线程身份一致
    assert_eq!(
        worker.owner,
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap()
    );
    assert_eq!(runtime.check_local(&worker.owner), Liveness::Alive);
    // 放行线程并 join：线程结束后判 Dead
    stop_tx.send(()).unwrap();
    worker.handle.join().unwrap();
    assert_eq!(runtime.check_local(&worker.owner), Liveness::Dead);
    // panic 线程：join 返回 Err，登记簿记录 Panicked
    let panicked = runtime
        .spawn_thread("异常线程", |_| panic!("预期的线程异常"))
        .unwrap();
    assert!(panicked.handle.join().is_err());
    assert_eq!(
        runtime.info(&panicked.owner).unwrap().state,
        ExecutionState::Panicked
    );
}

/// 验证 Future 在不同线程上被 poll 时，任务身份保持不变。
#[test]
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn future_keeps_task_identity_when_polled_on_another_thread() {
    let runtime = ExecutionRuntime::global().unwrap();
    // 记录每次 poll 时的执行上下文，供结束后比对
    let seen = Arc::new(Mutex::new(Vec::new()));
    let future_runtime = runtime.clone();
    let future_seen = Arc::clone(&seen);
    let mut polls = 0;
    let mut future = runtime.track_future(
        "可迁移任务",
        std::future::poll_fn(move |_| {
            future_seen
                .lock()
                .unwrap()
                .push(future_runtime.current_context());
            polls += 1;
            if polls == 1 {
                Poll::Pending
            } else {
                Poll::Ready(42)
            }
        }),
    );
    let owner = future.owner().clone();
    // 第一次调度在线程 A 上：Pending 后把 future 传回
    let first = runtime
        .spawn_thread("首次调度", move |_| {
            assert_eq!(poll_once(&mut future), Poll::Pending);
            future
        })
        .unwrap();
    let mut future = first.handle.join().unwrap();
    // 调度线程已结束，但任务还没结束：仍 Alive
    assert_eq!(runtime.check_local(&first.owner), Liveness::Dead);
    assert_eq!(runtime.check_local(&owner), Liveness::Alive);
    // 第二次调度在线程 B 上：Ready 完成
    let second = runtime
        .spawn_thread("再次调度", move |_| {
            assert_eq!(poll_once(&mut future), Poll::Ready(42));
        })
        .unwrap();
    second.handle.join().unwrap();
    let contexts = seen.lock().unwrap();
    // 两次 poll 的线程不同，但任务身份一致；完成后状态为 Completed
    assert_ne!(contexts[0].thread, contexts[1].thread);
    assert_eq!(contexts[0].task.as_ref(), Some(&owner));
    assert_eq!(contexts[1].task.as_ref(), Some(&owner));
    assert_eq!(
        runtime.info(&owner).unwrap().state,
        ExecutionState::Completed
    );
}

/// 验证 Future 每次 poll 都恢复创建时的日志上下文，且不污染调度线程的环境。
#[test]
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn future_restores_logging_context_on_each_scheduler_thread() {
    let runtime = ExecutionRuntime::global().unwrap();
    let request = crate::logging::request_scope();
    let expected = crate::logging::request_id();
    let mut polls = 0;
    let mut future = runtime.track_future(
        "诊断关联",
        std::future::poll_fn(move |_| {
            // poll 期间应看到创建时的请求 ID，而不是调度线程的环境
            assert_eq!(crate::logging::request_id(), expected);
            polls += 1;
            if polls == 1 {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        }),
    );
    drop(request);
    // 第一次调度：调度线程挂上自己的请求 ID，poll 内部应被创建时的上下文覆盖
    let first = thread::spawn(move || {
        let _ambient = crate::logging::attach_request(Some("scheduler-first".into()));
        assert!(poll_once(&mut future).is_pending());
        // poll 返回后调度线程环境恢复成自己的
        assert_eq!(
            crate::logging::request_id().as_deref(),
            Some("scheduler-first")
        );
        future
    });
    let mut future = first.join().unwrap();
    // 第二次调度同理：换一个线程再验证一次
    thread::spawn(move || {
        let _ambient = crate::logging::attach_request(Some("scheduler-second".into()));
        assert!(poll_once(&mut future).is_ready());
        assert_eq!(
            crate::logging::request_id().as_deref(),
            Some("scheduler-second")
        );
    })
    .join()
    .unwrap();
}

/// 测试专用 Future：析构时检查任务此刻仍为 Alive。
///
/// 用它验证"任务在内部 Future 析构完成前不能宣告结束"的顺序保证：
/// 若登记簿过早把任务置为 Completed/Cancelled，这里的断言就会失败。
struct CleanupFuture {
    /// 被测登记簿，析构时用它检查任务是否仍为 Alive。
    runtime: ExecutionRuntime,
    /// 包装器建立后回填的任务身份；Mutex 协调测试中的共享访问，None 表示尚未回填。
    owner: Arc<Mutex<Option<ExecutionOwner>>>,
    /// 测试控制开关：true 立即 Ready，false 保持 Pending 以测试丢弃路径。
    ready: bool,
}

impl Future for CleanupFuture {
    type Output = ();
    /// 功能：为析构顺序测试提供立即结束或一直挂起的可控 Future。
    /// 入参：self: `Pin<&mut CleanupFuture>` 为测试对象；匿名 Context 参数未使用。
    /// 返回值：ready=true 为 Ready(())，false 为 Pending，不修改 ready。
    /// 异常：没有主动失败分支。
    /// 注意事项：不注册唤醒，不执行真实任务；未就绪分支依赖用例主动丢弃或再次调度。
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        if self.ready {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl Drop for CleanupFuture {
    /// 功能：在内部 Future 正在清理时核对其任务仍被登记为 Alive，防止租约提前回收。
    /// 入参：self: &mut CleanupFuture，包含运行时和由用例回填的共享 owner。
    /// 返回值：无；断言当下 check_local 为 Alive。
    /// 异常：owner 锁中毒、尚未填写身份或观察状态不符会 panic，使测试失败。
    /// 注意事项：这是测试断言型析构器，不用于生产；验证的是内部 Future 清理发生在登记结束之前。
    fn drop(&mut self) {
        let owner = self.owner.lock().unwrap();
        assert_eq!(
            self.runtime.check_local(owner.as_ref().unwrap()),
            Liveness::Alive
        );
    }
}

/// 验证任务直到内部 Future 析构完成后才宣告结束；
/// 正常完成记 Completed，未完成即丢弃记 Cancelled。
#[test]
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn task_is_not_declared_dead_until_future_cleanup_finishes() {
    let runtime = ExecutionRuntime::global().unwrap();
    // ready=false 时 poll 一次仍 Pending（未完成），drop 后应记 Cancelled；
    // ready=true 时 poll 一次即 Ready（完成），drop 后应记 Completed。
    for ready in [false, true] {
        let shared_owner = Arc::new(Mutex::new(None));
        let mut future = runtime.track_future(
            "清理中的任务",
            CleanupFuture {
                runtime: runtime.clone(),
                owner: Arc::clone(&shared_owner),
                ready,
            },
        );
        let owner = future.owner().clone();
        *shared_owner.lock().unwrap() = Some(owner.clone());
        assert_eq!(poll_once(&mut future).is_ready(), ready);
        drop(future);
        let expected = if ready {
            ExecutionState::Completed
        } else {
            ExecutionState::Cancelled
        };
        assert_eq!(runtime.info(&owner).unwrap().state, expected);
    }
}

/// 验证从未被 poll 的 Future 与未开始执行的阻塞任务闭包被丢弃时，
/// 登记记录都按 Cancelled 结束。
#[test]
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn cancelling_an_unpolled_future_and_dropping_queued_work_end_their_records() {
    let runtime = ExecutionRuntime::global().unwrap();
    let future = runtime.track_future("尚未调度", std::future::pending::<()>());
    let owner = future.owner().clone();
    // 登记后即 Alive（还没执行但存在）
    assert_eq!(runtime.check_local(&owner), Liveness::Alive);
    // 未 poll 过即丢弃：记 Cancelled
    drop(future);
    assert_eq!(
        runtime.info(&owner).unwrap().state,
        ExecutionState::Cancelled
    );
    // 阻塞任务闭包还没交给线程池执行就被丢弃：同样 Cancelled
    let (owner, queued) = runtime.prepare_blocking("未开始的阻塞任务", |_| ());
    drop(queued);
    assert_eq!(
        runtime.info(&owner).unwrap().state,
        ExecutionState::Cancelled
    );
}

/// 验证阻塞任务即使等待句柄被丢弃，也保持 Alive 直到实际工作结束。
#[test]
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn detached_blocking_job_stays_alive_until_actual_work_finishes() {
    let runtime = ExecutionRuntime::global().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    // 任务闭包会阻塞等待放行信号，模拟真正在跑的阻塞工作
    let (owner, work) = runtime.prepare_blocking("不能被外层取消的工作", move |context| {
        ready_tx.send(context.owner().clone()).unwrap();
        stop_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    let worker = thread::spawn(move || {
        work();
        done_tx.send(()).unwrap();
    });
    // 闭包开始执行后报告的身份应与预分配 owner 一致
    assert_eq!(
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        owner
    );
    // 丢弃等待句柄不等于工作停止：任务必须仍为 Alive
    drop(worker);
    assert_eq!(runtime.check_local(&owner), Liveness::Alive);
    // 放行让工作真正结束，随后任务判 Dead
    stop_tx.send(()).unwrap();
    done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(runtime.check_local(&owner), Liveness::Dead);
}

/// 验证 poll 中 panic 不会留下脏任务上下文，且任务在 Future 销毁前仍为 Alive。
#[test]
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
/// 异常：测试准备或断言失败会 panic；场景中预期的错误由测试体捕获或核对，不作为生产错误返回。
fn caught_poll_panic_does_not_leave_stale_task_context() {
    let runtime = ExecutionRuntime::global().unwrap();
    let before = runtime.current_context();
    let mut future = runtime.track_future(
        "异常 Future",
        std::future::poll_fn(|_| -> Poll<()> { panic!("预期的 Future 异常") }),
    );
    let owner = future.owner().clone();
    // 捕获 panic：poll 抛异常后上下文必须恢复原状（任务栈守卫已正确弹出）
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| poll_once(&mut future))).is_err()
    );
    assert_eq!(runtime.current_context(), before);
    // Future 尚未销毁，不能因为一次 poll 异常就授权其他进程回收资源。
    assert_eq!(runtime.check_local(&owner), Liveness::Alive);
    // 销毁 Future 后，任务以 Panicked 结束
    drop(future);
    assert_eq!(
        runtime.info(&owner).unwrap().state,
        ExecutionState::Panicked
    );
}
