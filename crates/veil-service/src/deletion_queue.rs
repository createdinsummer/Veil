//! 已提交删除的物理回收队列：不访问数据库，不持有容器密码。

use std::{
    path::PathBuf,
    sync::Mutex,
    thread::{self, JoinHandle},
    time::Duration,
};
use veil_core::queue::{BatchPolicy, BoundedQueue, ListenControl, Sender};

const QUEUE_CAPACITY: usize = 4096;
const BATCH_SIZE: usize = 256;
const BATCH_INTERVAL: Duration = Duration::from_millis(50);

/// 用户删除已提交后才入队；溢出项留给低优先级垃圾回收处理。
pub(crate) struct PhysicalDeletionQueue {
    sender: Sender<PathBuf>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Default for PhysicalDeletionQueue {
    fn default() -> Self {
        let (sender, receiver) = BoundedQueue::new(QUEUE_CAPACITY).split();
        let worker = thread::spawn(move || {
            let _ = receiver.listen_batches(
                BatchPolicy {
                    count: BATCH_SIZE,
                    interval: BATCH_INTERVAL,
                },
                |_| 1,
                |state| Ok::<_, ()>(ListenControl::new(BATCH_SIZE, state.closed)),
                |paths| {
                    for path in paths {
                        // 元数据已提交，失败的物理回收可由垃圾回收兜底。
                        let _ = std::fs::remove_file(path);
                    }
                    Ok(())
                },
            );
        });
        Self {
            sender,
            worker: Mutex::new(Some(worker)),
        }
    }
}

impl PhysicalDeletionQueue {
    /// 非阻塞投递；队列满时仅保留可容纳的前缀，剩余项交由垃圾回收兜底。
    pub(crate) fn enqueue(&self, paths: Vec<PathBuf>) {
        let _ = self.sender.try_send_batch_partial(paths);
    }
}

impl Drop for PhysicalDeletionQueue {
    fn drop(&mut self) {
        self.sender.close();
        if let Ok(worker) = self.worker.get_mut() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
}
