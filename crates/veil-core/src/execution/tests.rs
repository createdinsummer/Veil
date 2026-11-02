use super::*;
use std::sync::mpsc;
use std::task::Waker;
use std::time::Duration;

fn poll_once<F: Future + Unpin>(future: &mut F) -> Poll<F::Output> {
    Pin::new(future).poll(&mut Context::from_waker(Waker::noop()))
}

#[test]
fn process_identity_and_unknown_records_are_conservative() {
    let runtime = ExecutionRuntime::global().unwrap();
    assert_eq!(runtime.check_process(runtime.process()), Liveness::Alive);
    let mut old = runtime.process().clone();
    old.instance_id.push_str("-old");
    assert_eq!(runtime.check_process(&old), Liveness::Dead);
    let unknown = ExecutionOwner::Thread {
        process: runtime.process().clone(),
        thread_id: "unregistered".into(),
    };
    assert_eq!(runtime.check_local(&unknown), Liveness::Unknown);
    let foreign = ExecutionOwner::Process { process: old };
    assert_eq!(runtime.check_local(&foreign), Liveness::Unknown);
}

#[test]
fn thread_and_task_lifetimes_are_independent() {
    let runtime = ExecutionRuntime::global().unwrap();
    let before = runtime.current_context();
    let outer = runtime.enter_task("外层任务");
    let outer_owner = outer.owner().clone();
    let inner = runtime.enter_task("内层任务");
    let inner_owner = inner.owner().clone();
    let inside = runtime.current_context();
    assert_eq!(inside.thread, before.thread);
    assert_eq!(inside.task.as_ref(), Some(&inner_owner));
    drop(inner);
    assert_eq!(runtime.current_context().task.as_ref(), Some(&outer_owner));
    assert_eq!(runtime.check_local(&inner_owner), Liveness::Dead);
    outer.cancel();
    assert_eq!(
        runtime.info(&outer_owner).unwrap().state,
        ExecutionState::Cancelled
    );
    assert_eq!(runtime.current_context().task, before.task);
    assert_eq!(runtime.check_local(&before.thread), Liveness::Alive);
}

#[test]
fn managed_thread_is_visible_before_work_and_dead_after_join() {
    let runtime = ExecutionRuntime::global().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();
    let worker = runtime
        .spawn_thread("受管线程", move |context| {
            ready_tx.send(context.thread).unwrap();
            stop_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        })
        .unwrap();
    assert_eq!(
        worker.owner,
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap()
    );
    assert_eq!(runtime.check_local(&worker.owner), Liveness::Alive);
    stop_tx.send(()).unwrap();
    worker.handle.join().unwrap();
    assert_eq!(runtime.check_local(&worker.owner), Liveness::Dead);
    let panicked = runtime
        .spawn_thread("异常线程", |_| panic!("预期的线程异常"))
        .unwrap();
    assert!(panicked.handle.join().is_err());
    assert_eq!(
        runtime.info(&panicked.owner).unwrap().state,
        ExecutionState::Panicked
    );
}

#[test]
fn future_keeps_task_identity_when_polled_on_another_thread() {
    let runtime = ExecutionRuntime::global().unwrap();
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
    let first = runtime
        .spawn_thread("首次调度", move |_| {
            assert_eq!(poll_once(&mut future), Poll::Pending);
            future
        })
        .unwrap();
    let mut future = first.handle.join().unwrap();
    assert_eq!(runtime.check_local(&first.owner), Liveness::Dead);
    assert_eq!(runtime.check_local(&owner), Liveness::Alive);
    let second = runtime
        .spawn_thread("再次调度", move |_| {
            assert_eq!(poll_once(&mut future), Poll::Ready(42));
        })
        .unwrap();
    second.handle.join().unwrap();
    let contexts = seen.lock().unwrap();
    assert_ne!(contexts[0].thread, contexts[1].thread);
    assert_eq!(contexts[0].task.as_ref(), Some(&owner));
    assert_eq!(contexts[1].task.as_ref(), Some(&owner));
    assert_eq!(
        runtime.info(&owner).unwrap().state,
        ExecutionState::Completed
    );
}

struct CleanupFuture {
    runtime: ExecutionRuntime,
    owner: Arc<Mutex<Option<ExecutionOwner>>>,
    ready: bool,
}

impl Future for CleanupFuture {
    type Output = ();
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        if self.ready {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl Drop for CleanupFuture {
    fn drop(&mut self) {
        let owner = self.owner.lock().unwrap();
        assert_eq!(
            self.runtime.check_local(owner.as_ref().unwrap()),
            Liveness::Alive
        );
    }
}

#[test]
fn task_is_not_declared_dead_until_future_cleanup_finishes() {
    let runtime = ExecutionRuntime::global().unwrap();
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

#[test]
fn cancelling_an_unpolled_future_and_dropping_queued_work_end_their_records() {
    let runtime = ExecutionRuntime::global().unwrap();
    let future = runtime.track_future("尚未调度", std::future::pending::<()>());
    let owner = future.owner().clone();
    assert_eq!(runtime.check_local(&owner), Liveness::Alive);
    drop(future);
    assert_eq!(
        runtime.info(&owner).unwrap().state,
        ExecutionState::Cancelled
    );
    let (owner, queued) = runtime.prepare_blocking("未开始的阻塞任务", |_| ());
    drop(queued);
    assert_eq!(
        runtime.info(&owner).unwrap().state,
        ExecutionState::Cancelled
    );
}

#[test]
fn detached_blocking_job_stays_alive_until_actual_work_finishes() {
    let runtime = ExecutionRuntime::global().unwrap();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let (owner, work) = runtime.prepare_blocking("不能被外层取消的工作", move |context| {
        ready_tx.send(context.owner().clone()).unwrap();
        stop_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    let worker = thread::spawn(move || {
        work();
        done_tx.send(()).unwrap();
    });
    assert_eq!(
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        owner
    );
    drop(worker);
    assert_eq!(runtime.check_local(&owner), Liveness::Alive);
    stop_tx.send(()).unwrap();
    done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(runtime.check_local(&owner), Liveness::Dead);
}

#[test]
fn caught_poll_panic_does_not_leave_stale_task_context() {
    let runtime = ExecutionRuntime::global().unwrap();
    let before = runtime.current_context();
    let mut future = runtime.track_future(
        "异常 Future",
        std::future::poll_fn(|_| -> Poll<()> { panic!("预期的 Future 异常") }),
    );
    let owner = future.owner().clone();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| poll_once(&mut future))).is_err()
    );
    assert_eq!(runtime.current_context(), before);
    // Future 尚未销毁，不能因为一次 poll 异常就授权其他进程回收资源。
    assert_eq!(runtime.check_local(&owner), Liveness::Alive);
    drop(future);
    assert_eq!(
        runtime.info(&owner).unwrap().state,
        ExecutionState::Panicked
    );
}
