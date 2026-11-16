//! 当前文件传输任务的进度；查询不依赖会话锁。
//!
//! # 业务定位
//!
//! 本模块是"进行中任务"的进度上报通道：导入/导出/打包等长任务在工作线程里
//! 更新进度快照，UI 线程随时读取展示。查询只通过 `Arc<Mutex<Snapshot>>` 读
//! 一份共享快照，不依赖业务会话锁；仍可能等待进度快照自身的互斥锁。
//!
//! # 核心机制
//!
//! - `Progress` 是共享句柄：内部快照（Mutex 保护）+ 取消标志（原子布尔）。
//! - 线程本地 `CURRENT` 记录"当前工作线程绑定的任务"，让 `start`/`advance`
//!   等全局便捷函数找到应更新的任务，避免每个调用点都传句柄。
//! - `Scope` 用 RAII 控制任务生命周期：离开作用域自动复位线程绑定，若任务
//!   处于未完成阶段则标记 failed。
//! - 取消只允许发生在导入/导出任务的中途阶段；一旦进入 finalizing 就不再接受，
//!   防止提交半途被取消。
//!
//! 核心清单：Snapshot/Progress 保存与共享展示状态，Scope/Attachment 管理线程绑定；
//! start/stage 与文件计数函数推进展示，request_cancel/check_cancelled 协调请求和检查点，copy 系列包装字节流。
//! 依赖：Serde 输出客户端快照，线程局部存储找到当前任务，Mutex/原子量协调共享状态，标准 Read/Write 处理流；不提交业务数据库。
use serde::Serialize;
use std::{
    cell::RefCell,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

/// 一次文件传输任务的进度快照，供 UI 展示与轮询。
///
/// 业务定位：把所有"进度数字"集中成一份可克隆的只读快照；UI 每次轮询拿到
/// 的是独立副本；复制期间需要短暂持有快照锁，可能与工作线程互相等待。
#[derive(Clone, Default, Serialize)]
pub struct Snapshot {
    /// 当前操作类型（如 "import"、"export"），决定取消等策略是否适用。
    pub operation: &'static str,
    /// 当前阶段（preparing/processing/verifying/finalizing/completed/failed/cancelled）。
    pub phase: &'static str,
    /// 本阶段已处理的字节数。
    pub processed_bytes: u64,
    /// 本阶段总字节数；None 表示未知（扫描未完成前允许持续增长）。
    pub total_bytes: Option<u64>,
    /// 已完成（处理完）的文件数。
    pub completed_files: u64,
    /// 已加密完成、尚未提交的文件数（等待入队提交）。
    pub pending_files: u64,
    /// 因已存在等原因跳过的文件数。
    pub skipped_files: u64,
    /// 已发现的总文件数（扫描中允许增长）。
    pub total_files: u64,
    /// 文件扫描是否已完成。
    pub scan_complete: bool,
    /// 已做重复/冲突检查的文件数。
    pub checked_files: u64,
    /// 当前等待处理的文件数。
    pub waiting_files: u64,
    /// 正在处理的文件名（展示"正在导入 xxx"）。
    pub current_file: Option<String>,
    /// 是否已请求取消（不代表任务已停止）。
    pub cancel_requested: bool,
    /// 并行工作线程数，展示并发规模。
    pub parallel_workers: u64,
}

/// 任务的共享进度句柄：内部快照（锁保护）+ 原子取消标志。
///
/// 业务定位：可以廉价 `Clone` 并跨线程传递；工作线程更新、UI 线程读取互不
/// 依赖同一把业务锁；访问快照时仍需等待内部互斥锁。
#[derive(Clone, Default)]
pub struct Progress(Arc<Mutex<Snapshot>>, Arc<AtomicBool>);

// 线程本地表：记录"当前线程正在执行哪个任务"，让全局便捷函数找到更新目标。
// 每个工作线程绑定自己的任务，线程之间互不干扰。
thread_local! { static CURRENT: RefCell<Option<Progress>> = const { RefCell::new(None) }; }

/// 返回当前线程绑定的任务句柄（未绑定则为 None）。
/// 入参：无。
/// 返回值：当前线程已绑定任务时返回 Some(Progress) 克隆，否则 None。
/// 异常：不返回 Result；线程局部存储不可访问或 RefCell 借用冲突可 panic。
/// 注意事项：不自动查找其他线程任务；克隆只是共享状态句柄，不复制快照。
pub fn current() -> Option<Progress> {
    CURRENT.with(|current| current.borrow().clone())
}
/// 计算文件传输的并行工作线程数。
///
/// 功能：按可用 CPU 核心数、上限 4、以及文件总数三者取最小，避免并行度过大。
///
/// 入参：`files` 是待处理文件总数。
///
/// 返回值：文件数为零时返回 0，否则返回 1～4；获取不到系统并发度时以 2 为基数，
/// 再受文件数限制。
/// 错误：不向调用方返回错误；无法获取系统并发度时使用回退值。
/// 注意：返回 0 表示没有文件，调用方若要启动至少一个线程需自行处理。
pub fn worker_count(files: usize) -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(2)
        .min(4)
        .min(files)
}

/// 任务生命周期作用域：进入时绑定线程本地任务，离开时恢复并收尾。
///
/// 业务定位：RAII（资源获取即初始化）守卫。`begin` 时接管当前线程的
/// `CURRENT`，`drop` 时把绑定恢复为上一个任务；若任务仍处于中间阶段
/// 说明异常退出，阶段标记为 failed。
pub struct Scope {
    /// 进入作用域前的线程绑定（可能为 None），离开时恢复。
    previous: Option<Progress>,
    /// 本作用域激活的任务句柄。
    active: Progress,
}
/// 将已有任务绑定到工作线程，不改变任务状态或所有权。
///
/// 业务定位：把"别的线程创建好的任务"绑定到当前线程（如线程池调度），
/// 使全局便捷函数能在本线程内更新它；drop 时恢复之前的绑定。
pub struct Attachment(Option<Progress>);
impl Drop for Attachment {
    /// 功能：结束工作线程对既有进度任务的临时绑定，恢复此前线程任务。
    /// 入参：self: &mut Attachment，保存进入前的 `Option<Progress>`。
    /// 返回值：无，将保存的旧绑定取回 CURRENT。
    /// 异常：TLS 或 RefCell 借用冲突可能 panic，不返回 Result。
    /// 注意事项：不结束任务或改阶段，只恢复本线程绑定；守卫须按作用域嵌套顺序在原绑定线程释放。
    fn drop(&mut self) {
        // 离开时把线程绑定恢复为绑定前的值（take 取出并放回）。
        CURRENT.with(|current| *current.borrow_mut() = self.0.take());
    }
}
impl Drop for Scope {
    /// 功能：结束主进度作用域时补记未正常完成的任务失败，并恢复原线程绑定。
    /// 入参：self: &mut Scope，持有活动 Progress 及上一个线程绑定。
    /// 返回值：无；已知中间阶段改为 failed，其他阶段保持原值，再释放快照锁并恢复 CURRENT。
    /// 异常：快照锁中毒时恢复状态；TLS 或 RefCell 借用冲突可能 panic，不返回 Result。
    /// 注意事项：只按阶段字符串判断，不查询数据库结果；不释放业务租约或删除文件，已标记 completed/cancelled 的任务不会再次改为失败。
    fn drop(&mut self) {
        // 收尾前先读一次快照：任务是否在正常结束前就离开了作用域。
        let mut snapshot = self.active.0.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(
            snapshot.phase,
            "preparing"
                | "processing"
                | "verifying"
                | "snapshot"
                | "packing"
                | "unpacking"
                | "finalizing"
        ) {
            // 处于中间阶段就离开作用域 = 未走 finish 流程（异常/提前退出），标记失败。
            snapshot.phase = "failed";
        }
        // 先释放快照锁，再恢复线程绑定，避免锁与线程局部状态交叉。
        drop(snapshot);
        CURRENT.with(|current| *current.borrow_mut() = self.previous.take());
    }
}
impl Progress {
    /// 把本任务绑定到当前线程，返回恢复守卫。
    ///
    /// 功能：工作线程（如线程池中的执行者）通过本方法接管一个既有任务，
    /// 使后续全局进度函数更新到该任务；返回的 `Attachment` 在 drop 时恢复原绑定。
    /// 入参：self: &Progress 为要在当前线程更新的任务句柄。
    /// 返回值：Attachment 恢复守卫，持有上一个线程绑定。
    /// 异常：线程局部存储访问或 RefCell 借用冲突可能 panic，不返回 Result。
    /// 注意事项：只绑定当前线程，不重置状态；守卫按嵌套顺序释放，不应借此结束任务。
    pub fn attach(&self) -> Attachment {
        Attachment(CURRENT.with(|current| current.replace(Some(self.clone()))))
    }
    /// 读取当前进度快照（独立副本）。
    ///
    /// 功能：UI 轮询入口；持锁拷贝后释放锁，期间其他快照访问可能短暂等待。
    /// 入参：self: &Progress 为待查询任务。
    /// 返回值：Snapshot 独立副本，字段描述同一把锁保护下读到的状态。
    /// 异常：锁中毒时恢复内部状态，不返回错误。
    /// 注意事项：仍可能短暂等待快照锁；返回后工作线程可能继续更新，副本不是完成通知。
    pub fn snapshot(&self) -> Snapshot {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    /// 开始一次新任务，返回生命周期作用域。
    ///
    /// 功能：重置取消标志、把快照初始化为 preparing 阶段，并接管当前线程绑定。
    ///
    /// 入参：`operation` 是任务类型（如 "import"），写入快照供策略判断。
    ///
    /// 返回值：作用域守卫；离开作用域会自动收尾（见 [`Scope`]）。
    ///
    /// 注意事项：同一线程上嵌套任务时，内层结束会恢复外层绑定。
    /// 异常：不返回 Result；线程局部存储借用冲突可能 panic。
    /// 注意事项：operation: &'static str 允许任何静态字符串，但 request_cancel 只认可 import/export；
    /// 同一 Progress 被并发 begin 会重置共享任务状态，调用方应避免复用仍活动的任务句柄。
    pub fn begin(&self, operation: &'static str) -> Scope {
        // 新任务开始：清除上一任务的取消标志（Release 保证后续更新可见）。
        self.1.store(false, Ordering::Release);
        // 快照整体重置，只保留操作类型与 preparing 阶段。
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Snapshot {
            operation,
            phase: "preparing",
            ..Snapshot::default()
        };
        Scope {
            previous: CURRENT.with(|current| current.replace(Some(self.clone()))),
            active: self.clone(),
        }
    }
    /// 结束任务并把阶段置为 completed / failed。
    ///
    /// 入参：`success` 为真表示正常完成，否则标记失败。
    /// 返回值：无；success: bool 只决定 completed 或 failed 阶段，不清空统计和取消标志。
    /// 异常：锁中毒时恢复快照，不返回错误。
    /// 注意事项：这里只更新展示状态，不等待工作线程、提交数据库或释放其他资源。
    pub fn finish(&self, success: bool) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).phase =
            if success { "completed" } else { "failed" };
    }
    /// 只接受仍可安全中止的文件导入或文件导出任务。
    ///
    /// 功能：UI 请求取消；只有 import/export 类型、且仍处于可中止阶段
    /// （preparing/processing/verifying）时接受。
    ///
    /// 返回值：接受取消返回 `true`；类型不符或已进入 finalizing 等不可中止
    /// 阶段返回 `false`。
    ///
    /// 注意事项：接受取消只是置位标志，实际停止由工作线程在检查点执行；
    /// 已进入提交（finalizing）阶段的任务不能中途取消，避免提交半截。
    /// 入参：self: &Progress 为需要请求取消的当前任务。
    /// 异常：不返回 Result；锁中毒时恢复快照继续检查。
    /// 注意事项：接受后设置快照 cancel_requested 和原子标志；重复请求仍可能返回 true，不表示工作已停止。
    pub fn request_cancel(&self) -> bool {
        let mut snapshot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        // 只允许导入/导出，且阶段仍在最终提交之前。
        if !matches!(snapshot.operation, "import" | "export")
            || !matches!(snapshot.phase, "preparing" | "processing" | "verifying")
        {
            return false;
        }
        // 快照里记录"已请求取消"（UI 可见），并置位原子标志（工作线程检查）。
        snapshot.cancel_requested = true;
        self.1.store(true, Ordering::Release);
        true
    }
    /// 取出可跨线程共享的取消标志（供复制循环等低层组件检查）。
    /// 入参：self: &Progress 为要观察的任务。
    /// 返回值：`Arc<AtomicBool>`，与任务共享的取消标志。
    /// 异常：克隆句柄不返回业务错误。
    /// 注意事项：建议消费者只读取；直接改写此标志不会同步 Snapshot.cancel_requested，而 check_cancelled 读取的是快照字段。
    pub fn cancellation_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.1)
    }
    /// 把阶段标记为 cancelled（任务因取消而结束）。
    /// 入参：self: &Progress 为已完成取消收尾的任务。
    /// 返回值：无；只把 phase 改为 cancelled。
    /// 异常：锁中毒时恢复状态，不返回错误。
    /// 注意事项：不主动终止线程、不清理文件，也不设置取消标志；应由业务确认已经停止后调用。
    pub fn finish_cancelled(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).phase = "cancelled";
    }
}
/// 在当前线程绑定的任务上执行一次快照更新；未绑定任务时静默忽略。
///
/// 功能：所有全局便捷函数（start/advance/...）共用的更新通道。
/// 入参：f: impl FnOnce(&mut Snapshot) 为更新当前线程任务快照的闭包。
/// 返回值：无；没有绑定时不执行闭包。
/// 异常：闭包 panic 向外传播且可使快照锁中毒；已有中毒锁恢复状态继续使用。
/// 注意事项：在快照互斥锁内执行 f，闭包不得递归查询或更新同一任务，否则可能等待自己持有的锁。
fn update(f: impl FnOnce(&mut Snapshot)) {
    CURRENT.with(|current| {
        if let Some(progress) = current.borrow().as_ref() {
            // 拿锁期间执行更新闭包；中毒锁（panic 遗留）按可恢复处理。
            f(&mut progress.0.lock().unwrap_or_else(|e| e.into_inner()));
        }
    });
}
/// 开始处理阶段：设置总字节数并进入 processing。
///
/// 入参：`total` 是本次处理的总字节数；None 表示未知。
/// 返回值：无；只设置 total_bytes 和 processing 阶段，不自动清零已有已处理字节。
/// 异常：不返回错误，沿用 update 的线程绑定与锁访问约束。
/// 注意事项：total: `Option<u64>` 的 None 表示总量未知；无当前线程任务时无操作。
pub fn start(total: Option<u64>) {
    update(|p| {
        p.total_bytes = total;
        p.phase = "processing";
    });
}
/// 切换到独立阶段；百分比仅表示当前阶段的字节处理进度。
///
/// 功能：进入新阶段（如 verifying/finalizing）时重置字节与文件计数，
/// 让进度条按阶段重新计算。
///
/// 入参：`phase` 是新阶段名；`total` 是该阶段总字节数。
/// 返回值：无；阶段名与总量更新，并清零字节数及部分文件调度计数。
/// 异常：不返回错误，沿用 update 约束。
/// 注意事项：phase: &'static str 是静态阶段名；total: `Option<u64>` 是本阶段总量；
/// scan_complete、checked_files、waiting_files 和取消标记不会在此清零，不能当作整任务重新初始化。
pub fn stage(phase: &'static str, total: Option<u64>) {
    update(|p| {
        p.phase = phase;
        p.total_bytes = total;
        // 阶段切换后旧的累计进度清零，百分比只反映本阶段。
        p.processed_bytes = 0;
        p.current_file = None;
        p.completed_files = 0;
        p.pending_files = 0;
        p.skipped_files = 0;
        p.total_files = 0;
        p.parallel_workers = 0;
    });
}

/// 带进度累计的字节流复制。
///
/// 功能：把 reader 内容复制到 writer，同时按每 1 MiB 粒度上报已处理字节。
///
/// 入参：`reader` 是明文/密文输入流；`writer` 是目标流。
///
/// 返回值：复制总字节数；IO 失败时返回 Err。
///
/// 注意事项：进度更新只绑定当前线程的任务（见 `update`）。
/// 异常：输入读取或底层写入失败返回 io::Error；返回错误前仍会上报已实际写入的尾部字节。
/// 注意事项：reader/writer 为实现 Read/Write 的可变借用；此复制路径只统计进度，不主动检查取消，不代表写入已持久化。
pub fn copy(
    reader: &mut impl std::io::Read,
    writer: &mut impl std::io::Write,
) -> std::io::Result<u64> {
    // 包装目标流的计数写入器：累计写入量并分批上报进度。
    /// 进度复制的输出包装：借用真实 writer，pending 累计尚未上报的实际写入字节。
    /// 每达到 1 MiB 批量更新进度，减少频繁争用快照锁，失败时尾部已写量由外层补报。
    struct Counted<'a, W> {
        // 真正的目标流。
        writer: &'a mut W,
        // 自上次上报以来累积的字节数（攒够 1 MiB 才上报一次）。
        pending: u64,
    }
    impl<W: std::io::Write> std::io::Write for Counted<'_, W> {
        /// 真实写入后累计字节数，达到上报阈值才更新共享进度以减少锁竞争。
        /// 功能：按实际输出量累计复制进度，避免每次短写都更新共享状态。
        /// 入参：self 为 Counted 计数写入器；bytes 为本次待输出字节切片。
        /// 返回值：底层实际写入字节数，同时把数量加入 pending。
        /// 异常：原样传播底层 writer.write 的 io::Error。
        /// 注意事项：累计至少 1 MiB 时一次性上报；不限制单次写入大小，也不在此直接检查取消。
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            // 先真实写入，再按实际写入字节数累计（部分写入也计入）。
            let written = self.writer.write(bytes)?;
            self.pending += written as u64;
            // 攒够 1 MiB 才上报一次，降低锁竞争与 UI 刷新频率。
            if self.pending >= 1024 * 1024 {
                advance(self.pending);
                self.pending = 0;
            }
            Ok(written)
        }
        /// 将刷新请求转交下游并保留其 I/O 错误；是否持久落盘取决于下游实现。
        /// 功能：把复制写入器的缓冲刷新请求交给实际输出端。
        /// 入参：self 为包装底层 writer 的 Counted。
        /// 返回值：底层刷新成功返回 Ok(())。
        /// 异常：原样传播 writer.flush 的 io::Error。
        /// 注意事项：这里不检查取消，也不额外执行文件持久化同步。
        fn flush(&mut self) -> std::io::Result<()> {
            self.writer.flush()
        }
    }
    let mut counted = Counted { writer, pending: 0 };
    // 标准库整段复制，走上面的计数写入器。
    let result = std::io::copy(reader, &mut counted);
    // 无论复制成功或失败，都上报尚未累计的已写入字节；失败时不代表总量已完成。
    advance(counted.pending);
    result
}

/// 内部错误类型：表示复制因取消请求而中止。
#[derive(Debug)]
struct CopyCancelled;
impl std::fmt::Display for CopyCancelled {
    /// 功能：把用户取消复制的内部标记显示为可读原因。
    /// 入参：self 为 CopyCancelled 标记；f 为标准库格式化输出器。
    /// 返回值：写出“操作已中止”成功返回 Ok(())。
    /// 异常：输出器写入失败返回 fmt::Error。
    /// 注意事项：只格式化原因，不修改进度状态，不再次发起取消。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("操作已中止")
    }
}
impl std::error::Error for CopyCancelled {}

/// 判断一个 IO 错误是否由取消复制触发。
///
/// 功能：让调用方区分"IO 故障"与"用户取消"，前者需报错，后者静默收尾。
/// 入参：error: &io::Error 为复制失败原因。
/// 返回值：直接底层来源类型为 CopyCancelled 时为 true，其余为 false。
/// 异常：只检查类型，不返回错误。
/// 注意事项：不递归扫描任意多层错误链；额外包装后不保证还能识别。
pub fn is_cancelled_io(error: &std::io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|source| source.is::<CopyCancelled>())
}

/// 不累计进度：用于已经计过加密字节数的密文发布复制。
///
/// 功能：与 [`copy`] 类似，但不累计字节进度（密文发布阶段字节已在加密时计过），
/// 改为每 1 MiB 检查一次取消标志；取消时返回可识别的错误。
///
/// 入参：`reader` 是密文输入流；`writer` 是发布目标流。
///
/// 返回值：复制总字节数；取消返回包装了 `CopyCancelled` 的 IO 错误。
/// 异常：取消返回带 CopyCancelled 的 io::Error，输入/输出错误原样传播。
/// 注意事项：reader/writer 是 Read/Write 的可变借用；检查在开始、每累计 1 MiB 后和成功收尾时执行，
/// 阻塞 I/O 不能被此检查强行中断；失败不回滚已输出字节。
pub fn copy_cancellable(
    reader: &mut impl std::io::Read,
    writer: &mut impl std::io::Write,
) -> std::io::Result<u64> {
    // 包装目标流的可取消写入器：控制检查节奏，防止每块都查。
    /// 可取消复制的输出包装：借用真实 writer，since_check 记录距取消检查点的已写量。
    /// 每次限制写入到下一个 1 MiB 边界，只负责响应取消，不重复累计加密阶段已上报的进度。
    struct Cancellable<'a, W> {
        // 真正的目标流。
        writer: &'a mut W,
        // 距上次取消检查已写入的字节数（每 1 MiB 查一次）。
        since_check: usize,
    }
    impl<W: std::io::Write> std::io::Write for Cancellable<'_, W> {
        /// 功能：在流式复制写入过程中定期响应用户取消，避免长文件无法停止。
        /// 入参：self 为包装业务写入器的 Cancellable；bytes 为本次待复制字节切片。
        /// 返回值：返回本次实际写入字节数，最多写到下一个 1 MiB 检查点。
        /// 异常：已取消时返回内含 CopyCancelled 的 I/O 错误；底层写入错误原样传播。
        /// 注意事项：检查在累计达到 1 MiB 后的下一次写入前进行；调用者仍须正确处理短写。
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            // 距上次检查已满 1 MiB：先查取消标志再继续写。
            if self.since_check >= 1024 * 1024 {
                if check_cancelled().is_err() {
                    // 已取消：中止本次复制，返回带原因的 IO 错误。
                    return Err(std::io::Error::other(CopyCancelled));
                }
                self.since_check = 0;
            }
            // 本次最多写到凑满 1 MiB 检查点为止，避免越过检查粒度。
            let limit = (1024 * 1024 - self.since_check).min(bytes.len());
            let written = self.writer.write(&bytes[..limit])?;
            self.since_check += written;
            Ok(written)
        }
        /// 功能：转交复制目标的缓冲刷新请求。
        /// 入参：self 为 Cancellable 写入包装器。
        /// 返回值：底层刷新成功时返回 Ok(())。
        /// 异常：原样传播 writer.flush 的 io::Error。
        /// 注意事项：不检查取消，不额外请求文件或目录持久化。
        fn flush(&mut self) -> std::io::Result<()> {
            self.writer.flush()
        }
    }
    // 复制开始前先查一次，任务已取消则直接失败。
    if check_cancelled().is_err() {
        return Err(std::io::Error::other(CopyCancelled));
    }
    let result = std::io::copy(
        reader,
        &mut Cancellable {
            writer,
            since_check: 0,
        },
    );
    // 复制正常结束但期间出现过取消请求（不足 1 MiB 的尾部没触发检查）：
    // 收尾再查一次，保证取消不丢失。
    if result.is_ok() && check_cancelled().is_err() {
        return Err(std::io::Error::other(CopyCancelled));
    }
    result
}
/// 设置总文件数（事先已知时一次性设置）。
/// 入参：total: usize 为事先确定的任务文件总数。
/// 返回值：无；更新当前线程绑定任务的对应快照字段。
/// 异常：不返回 Result；快照锁中毒按可恢复处理，计数的普通加法仍受整数边界约束。
/// 注意事项：未绑定任务时无操作；统计只用于展示，不代替数据库或队列中的实际业务状态。
pub fn set_file_count(total: usize) {
    update(|p| p.total_files = total as u64);
}
/// 遍历线程逐个发现文件；总数和总字节数允许持续增长。
///
/// 功能：扫描阶段每发现一个文件就累加计数；已知字节总量随扫描推进增长，
/// 未知或发生整数溢出时保持为未知。
///
/// 入参：`size` 是新发现文件的字节数。
/// 入参：size: u64 为新发现文件的明文字节数，每调用一次代表一个文件。
/// 返回值：无；更新当前线程绑定任务的对应快照字段。
/// 异常：不返回 Result；快照锁中毒按可恢复处理，计数的普通加法仍受整数边界约束。
/// 注意事项：未绑定任务时无操作；统计只用于展示，不代替数据库或队列中的实际业务状态。已有总字节为 None 时仍保持未知；文件数的普通加法在极端溢出时可能 panic 或回绕。
pub fn discovered_file(size: u64) {
    update(|p| {
        p.total_files += 1;
        // total_bytes 为 Some 时尝试累加；未知或加法溢出都以 None 表示。
        p.total_bytes = p.total_bytes.and_then(|total| total.checked_add(size));
    });
}
/// 标记文件扫描完成（扫描结束后 UI 可显示确定总量）。
/// 入参：无；调用方已经完成当前任务的文件扫描。
/// 返回值：无；更新当前线程绑定任务的对应快照字段。
/// 异常：不返回 Result；快照锁中毒按可恢复处理，计数的普通加法仍受整数边界约束。
/// 注意事项：未绑定任务时无操作；统计只用于展示，不代替数据库或队列中的实际业务状态。
pub fn scan_finished() {
    update(|p| p.scan_complete = true);
}
/// 完成一个文件的重复/冲突检查，计数加一。
/// 入参：无；每次调用代表一条文件数据已完成重复/冲突检查。
/// 返回值：无；更新当前线程绑定任务的对应快照字段。
/// 异常：不返回 Result；快照锁中毒按可恢复处理，计数的普通加法仍受整数边界约束。
/// 注意事项：未绑定任务时无操作；统计只用于展示，不代替数据库或队列中的实际业务状态。
pub fn file_checked() {
    update(|p| p.checked_files += 1);
}
/// 设置当前等待处理的文件数（队列积压量展示）。
/// 入参：count: usize 为当前待处理文件数，替换旧值而非累加。
/// 返回值：无；更新当前线程绑定任务的对应快照字段。
/// 异常：不返回 Result；快照锁中毒按可恢复处理，计数的普通加法仍受整数边界约束。
/// 注意事项：未绑定任务时无操作；统计只用于展示，不代替数据库或队列中的实际业务状态。
pub fn set_waiting_files(count: usize) {
    update(|p| p.waiting_files = count as u64);
}

/// 设置并行工作线程数。
/// 入参：count: usize 为当前工作线程数，零表示未启动或已退出。
/// 返回值：无；更新当前线程绑定任务的对应快照字段。
/// 异常：不返回 Result；快照锁中毒按可恢复处理，计数的普通加法仍受整数边界约束。
/// 注意事项：未绑定任务时无操作；统计只用于展示，不代替数据库或队列中的实际业务状态。
pub fn set_parallel_workers(count: usize) {
    update(|p| p.parallel_workers = count as u64);
}
/// 开始处理一个新文件，记录当前文件名。
///
/// 入参：`name` 是正在处理的文件展示名（如 "正在导入 photo.png"）。
/// 入参：name: &str 为当前文件展示名称，复制存入快照。
/// 返回值：无；更新当前线程绑定任务的对应快照字段。
/// 异常：不返回 Result；快照锁中毒按可恢复处理，计数的普通加法仍受整数边界约束。
/// 注意事项：未绑定任务时无操作；统计只用于展示，不代替数据库或队列中的实际业务状态。并行任务会覆盖同一 current_file，表示最后更新者，不是所有活动文件列表。
pub fn begin_file(name: &str) {
    update(|p| {
        p.phase = "processing";
        p.current_file = Some(name.to_owned());
    });
}
/// 一个文件处理完成；`skipped` 为真表示该文件被跳过（如已存在）。
/// 入参：skipped: bool 为本文件是否按业务规则跳过。
/// 返回值：无；更新当前线程绑定任务的对应快照字段。
/// 异常：不返回 Result；快照锁中毒按可恢复处理，计数的普通加法仍受整数边界约束。
/// 注意事项：未绑定任务时无操作；统计只用于展示，不代替数据库或队列中的实际业务状态。跳过文件同样计入 completed_files，调用方不能对同一文件重复完成计数。
pub fn file_done(skipped: bool) {
    update(|p| {
        p.completed_files += 1;
        if skipped {
            // 跳过的文件单独计数，UI 可展示"跳过 N 个"。
            p.skipped_files += 1;
        }
    });
}
/// 记录加密完成、尚未提交的文件。
///
/// 功能：文件密文已生成但还未提交数据库时 +1；提交（出队）后由
/// [`file_dequeued`] 减回，展示"待提交"积压量。
/// 入参：无；每次调用代表一份已完成加密、等候提交的文件。
/// 返回值：无；更新当前线程绑定任务的对应快照字段。
/// 异常：不返回 Result；快照锁中毒按可恢复处理，计数的普通加法仍受整数边界约束。
/// 注意事项：未绑定任务时无操作；统计只用于展示，不代替数据库或队列中的实际业务状态。
pub fn file_queued() {
    update(|p| p.pending_files += 1);
}
/// 一个待提交文件完成提交出队，计数减一（不出现负数）。
/// 入参：无；每次调用代表一份不再等待提交的文件。
/// 返回值：无；更新当前线程绑定任务的对应快照字段。
/// 异常：不返回 Result；快照锁中毒按可恢复处理，计数的普通加法仍受整数边界约束。
/// 注意事项：未绑定任务时无操作；统计只用于展示，不代替数据库或队列中的实际业务状态。
pub fn file_dequeued() {
    update(|p| p.pending_files = p.pending_files.saturating_sub(1));
}
/// 切换到校验阶段（密文认证/一致性检查）。
/// 入参：无；只切换当前阶段，不执行任何实际认证。
/// 返回值：无；更新当前线程绑定任务的对应快照字段。
/// 异常：不返回 Result；快照锁中毒按可恢复处理，计数的普通加法仍受整数边界约束。
/// 注意事项：未绑定任务时无操作；统计只用于展示，不代替数据库或队列中的实际业务状态。
pub fn verifying() {
    update(|p| p.phase = "verifying");
}
/// 累加已处理字节数（饱和加法，溢出时停在最大值）。
/// 入参：bytes: u64 为本次实际处理的新增字节数，不是累计总数。
/// 返回值：无；更新当前线程绑定任务的对应快照字段。
/// 异常：不返回 Result；快照锁中毒按可恢复处理，计数的普通加法仍受整数边界约束。
/// 注意事项：未绑定任务时无操作；统计只用于展示，不代替数据库或队列中的实际业务状态。
pub fn advance(bytes: u64) {
    update(|p| p.processed_bytes = p.processed_bytes.saturating_add(bytes));
}
/// 检查当前任务是否已请求取消。
///
/// 返回值：已取消返回 `Err(VeilError::Cancelled)`，否则 `Ok(())`。
///
/// 注意事项：供低层复制循环等检查点调用；未绑定任务时视为未取消。
/// 入参：无；从当前线程绑定的 Progress 读取快照。
/// 异常：已请求取消返回 VeilError::Cancelled；没有绑定或标志未置位为 Ok(())。
/// 注意事项：读取快照需要互斥锁，不能在持有同一快照锁的闭包中递归调用。
pub fn check_cancelled() -> crate::error::Result<()> {
    let cancelled = CURRENT.with(|current| {
        current
            .borrow()
            .as_ref()
            .is_some_and(|p| p.snapshot().cancel_requested)
    });
    if cancelled {
        Err(crate::error::VeilError::Cancelled)
    } else {
        Ok(())
    }
}
/// 进入最终提交阶段（此后不再接受取消）。
/// 入参：无；调用方即将进入不可中途取消的提交阶段。
/// 返回值：无；更新当前线程绑定任务的对应快照字段。
/// 异常：不返回 Result；快照锁中毒按可恢复处理，计数的普通加法仍受整数边界约束。
/// 注意事项：未绑定任务时无操作；统计只用于展示，不代替数据库或队列中的实际业务状态。不会撤销先前已经接受的取消标记，调用方仍应遵守提交前检查顺序。
pub fn finalizing() {
    update(|p| p.phase = "finalizing");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// 验证文件扫描持续发现新文件时总数会上升，但已完成数量不会被重置。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn discovery_increases_totals_without_resetting_completed_progress() {
        let progress = Progress::default();
        let _scope = progress.begin("import");
        start(Some(0));
        discovered_file(10);
        file_done(false);
        assert_eq!(
            (
                progress.snapshot().completed_files,
                progress.snapshot().total_files
            ),
            (1, 1)
        );
        discovered_file(20);
        let snapshot = progress.snapshot();
        assert_eq!(
            (
                snapshot.completed_files,
                snapshot.total_files,
                snapshot.total_bytes
            ),
            (1, 2, Some(30))
        );
        assert!(!snapshot.scan_complete);
        scan_finished();
        assert!(progress.snapshot().scan_complete);
    }

    #[test]
    /// 验证文件传输进入最终提交阶段后不再接受取消，避免结果发布被中途破坏。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn cancellation_is_only_accepted_before_finalizing_a_file_transfer() {
        let progress = Progress::default();
        assert!(!progress.request_cancel());
        {
            let _scope = progress.begin("export");
            start(Some(1));
            assert!(progress.request_cancel());
            finalizing();
            assert!(!progress.request_cancel());
        }
        {
            let _scope = progress.begin("import");
            start(Some(1));
            finalizing();
            assert!(!progress.request_cancel());
        }
    }

    #[test]
    /// 验证复制到暂存目标时观察到取消信号会中止复制并清理未完成内容。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn cancellation_interrupts_a_staged_copy() {
        /// 取消复制测试替身：模拟输出成功后立即请求取消，验证复制器在检查点收尾。
        /// progress 为被测任务句柄，written 为模拟输出总量，不持有磁盘文件。
        struct StopWriter {
            /// 接收模拟用户取消请求的进度句柄。
            progress: Progress,
            /// 模拟目标已接受字节数，用于确认没有无限继续复制。
            written: usize,
        }
        impl std::io::Write for StopWriter {
            /// 功能：在复制取消测试中模拟用户看见首批输出后请求停止。
            /// 入参：self 为测试写入器，保存进度句柄和累计量；bytes: &[u8] 为此次模拟输出内容。
            /// 返回值：Ok(bytes.len()) 并累计 written，同时调用 request_cancel。
            /// 异常：此替身不主动返回 I/O 错误，也不校验取消请求的布尔受理结果。
            /// 注意事项：只用于测试，不真正落盘；实际复制是否响应取消由外层复制循环的检查点决定。
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.written += bytes.len();
                self.progress.request_cancel();
                Ok(bytes.len())
            }
            /// 功能：为测试目标满足 Write 刷新接口，不执行真实磁盘刷新。
            /// 入参：self 为没有写缓冲的测试写入器。
            /// 返回值：始终 Ok(())。
            /// 异常：没有主动失败分支。
            /// 注意事项：空刷新只属于该测试替身，不能推断真实文件输出的持久化或刷新语义。
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let progress = Progress::default();
        let _scope = progress.begin("import");
        start(Some(2 * 1024 * 1024));
        let mut reader = std::io::Cursor::new(vec![1; 2 * 1024 * 1024]);
        let mut writer = StopWriter {
            progress,
            written: 0,
        };
        let error = copy_cancellable(&mut reader, &mut writer).unwrap_err();
        assert!(is_cancelled_io(&error));
        assert!(writer.written < 2 * 1024 * 1024);
    }

    #[test]
    /// 验证任务进度快照可安全地从另一个线程读取。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn progress_can_be_read_from_another_thread() {
        let progress = Progress::default();
        let worker = progress.clone();
        let (ready, observe) = std::sync::mpsc::channel();
        let (resume, wait) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _scope = worker.begin("import");
            start(Some(10));
            advance(4);
            ready.send(()).unwrap();
            wait.recv().unwrap();
            advance(6);
            finalizing();
            worker.finish(true);
        });
        observe.recv().unwrap();
        let snapshot = progress.snapshot();
        assert_eq!(
            (
                snapshot.phase,
                snapshot.processed_bytes,
                snapshot.total_bytes
            ),
            ("processing", 4, Some(10))
        );
        resume.send(()).unwrap();
        thread.join().unwrap();
        let snapshot = progress.snapshot();
        assert_eq!(
            (snapshot.phase, snapshot.processed_bytes),
            ("completed", 10)
        );
    }
}
