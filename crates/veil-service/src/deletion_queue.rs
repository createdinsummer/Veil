//! 已提交删除的物理回收队列：不访问数据库，不持有容器密码。
//!
//! 业务上的「删除文件」分为两步：先由写事务删除容器库（veil.db）里的
//! 目录项/版本引用（数据库是权威来源），提交后才把失去引用的密文路径
//! 投递到这里排队回收。本队列不打开数据库、不校验引用，因此也不会被
//! 容器密码或资源租约卡住，但文件系统删除仍可阻塞；删除失败或队列溢出项需由另行调用的垃圾回收扫描兜底。
//! 所属模块：传输提交后的密文回收。核心类型 PhysicalDeletionQueue 管理发送端与消费者线程；
//! default 启动后台批处理，enqueue 投递已解除引用的路径，drop 关闭队列并等待消费者。
//! 依赖：veil-core 通用有界队列、标准库线程及文件删除。目标是把物理删除移出业务事务，
//! 缓冲突发清理请求；它不是持久化任务队列，进程崩溃后不能依靠队列恢复未完成消息。

use std::{
    path::PathBuf,
    sync::Mutex,
    thread::{self, JoinHandle},
    time::Duration,
};
use veil_core::queue::{BatchPolicy, BoundedQueue, ListenControl, Sender};

// 队列最多排队 4096 个待删密文路径，超过则只接收能容纳的前缀。
const QUEUE_CAPACITY: usize = 4096;
// 每批最多从队列取出 256 个路径一起删除。
const BATCH_SIZE: usize = 256;
// 从本批首条消息开始计时 50 毫秒，数量不足时也可触发；实际执行还受线程调度影响。
const BATCH_INTERVAL: Duration = Duration::from_millis(50);

/// 用户删除已提交后才入队；溢出项留给低优先级垃圾回收处理。
///
/// 内部结构：一个线程安全的有界队列发送端（业务线程投递用）＋一个后台
/// 删除线程句柄（析构时回收）。
/// 使用场景：业务事务已提交且密文不再被引用后异步投递；每条消息是一条物理路径。
/// 本类型不确认路径是否仍被引用，也不把删除成功与否反馈给用户操作。
pub(crate) struct PhysicalDeletionQueue {
    // 发送端：业务线程把已提交删除的密文文件路径投递到这里。
    sender: Sender<PathBuf>,
    // 后台消费者句柄；包装为互斥状态，析构独占取出后等待线程结束。
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Default for PhysicalDeletionQueue {
    /// 功能：建立有界回收通道，并启动专门删除无引用密文的后台消费者。
    /// 入参：无。
    /// 返回值：已启动消费者的队列；最多缓存 4096 条路径，批次条数为 256，时间阈值为 50 毫秒。
    /// 异常：线程创建失败可能 panic；消费阶段 remove_file 失败被忽略，不终止整批。
    /// 注意事项：消费者不持有数据库或密码，发送方必须先完成引用删除并确认事务提交。
    fn default() -> Self {
        // 拆分有界队列：发送端留在结构体里，接收端只归后台线程使用。
        let (sender, receiver) = BoundedQueue::new(QUEUE_CAPACITY).split();
        // 启动专职删除线程：攒批消费路径并逐个删除，不占用业务线程。
        let worker = thread::spawn(move || {
            let _ = receiver.listen_batches(
                BatchPolicy {
                    count: BATCH_SIZE,
                    interval: BATCH_INTERVAL,
                },
                |_| 1,
                |state| Ok::<_, ()>(ListenControl::new(BATCH_SIZE, state.closed)),
                |paths| {
                    // 每条路径对应一份已提交删除、可在文件系统中回收的密文。
                    for path in paths {
                        // 发送方承诺元数据已提交；失败只忽略，不重试、不反馈，后续另行触发垃圾扫描才可能回收。
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
    /// 非阻塞投递已解除引用的密文路径；队列满时仅保留可容纳的前缀。
    /// 入参：`self: &PhysicalDeletionQueue` 为共享发送端；`paths: Vec<PathBuf>` 为待回收密文路径，
    /// 每项对应一个失去引用的物理文件，允许空列表；所有权随投递转交队列。
    /// 返回：无；未能入队的路径不会反馈给调用方，后续依赖垃圾回收扫描。
    /// 异常：没有错误返回；队列满、关闭或物理删除失败均不会通过此接口反馈。
    /// 注意：调用方须先提交删除引用的数据库事务；本队列不会再次检查引用。
    /// 方法无需等待磁盘删除完成，但队列内部仍需短暂取得互斥锁，并非无锁操作。
    pub(crate) fn enqueue(&self, paths: Vec<PathBuf>) {
        // 尽量全部投递；投不进去的部分丢弃，由低优先级清理器稍后扫描回收。
        let _ = self.sender.try_send_batch_partial(paths);
    }
}

impl Drop for PhysicalDeletionQueue {
    /// 功能：停止接收回收请求，并等待消费者处理完已排队的密文路径。
    /// 入参：`self: &mut PhysicalDeletionQueue` 为正在析构的队列。
    /// 返回值：无。
    /// 异常：消费者 panic 的 join 结果被忽略；worker 互斥锁中毒时跳过显式 join。
    /// 注意事项：正常路径会阻塞等待后台删除结束；锁中毒路径不能承诺析构前线程已经退出。
    fn drop(&mut self) {
        // 关闭发送端，通知接收端消费完剩余批次后正常收尾。
        self.sender.close();
        if let Ok(worker) = self.worker.get_mut() {
            if let Some(worker) = worker.take() {
                // 等待删除线程处理完队列中已投递的路径，避免留下半截工作。
                let _ = worker.join();
            }
        }
    }
}
