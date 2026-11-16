//! 通用队列，提供普通有界队列 BoundedQueue 和普通无界队列 UnboundedQueue。
//!
//! 两种队列都支持多生产者、多消费者，以及单条操作、批量操作。
//! 不覆盖已有消息，每次入队的消息只能被一个消费者取走一次，不支持消息去重。
//! 不保证多个生产者并发投递时的入队先后，也不要求多个消费者按顺序处理。
//! 支持监听推送：由队列内部按数量或时间阈值调用注册的消费回调。
//! 内部操作通过同一把锁串行执行；关闭后仍可取出剩余消息，取消只影响本次调用。
//!
//! # 业务定位
//!
//! 这是 Veil 的任务队列基础设施：导入/导出等长任务把"待处理的工作单元"（如
//! 一批待加密文件）投进队列，多个工作线程作为消费者并发取出处理。队列在
//! 这里的业务角色是**任务调度与并发消费的通道**：
//! - 削峰/缓冲：生产者（扫描/准备阶段）与消费者（处理阶段）速度不一致时，
//!   队列吸收瞬时堆积，让两边解耦、各按各的节奏走。
//! - 异步解耦：入队只表示"任务已提交"，不等处理完成；入队成功≠处理成功。
//! - 并发消费：每次入队的消息最多由一个消费者取走；队列不重投、不去重，
//!   也不保证消费者取出后一定成功处理。相同内容的多次入队仍是多条消息。
//!
//! # 与业务的关系
//!
//! 队列本身是通用的（消息类型 T 不限定），不关心消息内容；"每条消息是一条
//! 业务任务还是包含多个业务条目"由调用方通过权重（weight）和批次策略表达。
//! `Receiver::listen_batches` 提供"攒够一批再回调"的监听模式，供需要按批
//! 落盘/提交的消费方使用。
//!
//! # 并发语义
//!
//! - 所有内部操作经同一把锁串行执行，`len`/`closed` 等状态与底层 mpsc 通道
//!   保持一致。
//! - 关闭（close）后：不再接受新消息，但已入队的剩余消息仍可取出（排空语义）。
//! - 最后一个生产端丢弃 → 队列关闭；最后一个消费端丢弃 → 队列关闭并释放
//!   未被消费的消息（在锁外析构，防止消息析构再访问队列造成死锁）。
//! - 取消（cancelled）只影响本次等待调用，不关闭队列、不丢弃已入队消息。
use std::{
    fmt,
    sync::{Arc, Condvar, Mutex, MutexGuard, mpsc},
    time::{Duration, Instant},
};

// 取消条件由调用者提供，定期检查，避免要求取消方持有队列句柄。
// 含义：等待中的调用每 10ms 醒来检查一次取消条件/超时，兼顾响应速度与开销。
const CANCEL_INTERVAL: Duration = Duration::from_millis(10);

/// 入队失败原因；每种失败都带回原消息，保证消息所有权不丢失。
///
/// 业务定位：发送端拿回消息后可决定重试、暂存或放弃，不会凭空丢任务。
#[derive(Debug, PartialEq, Eq)]
pub enum SendError<T> {
    /// 队列已满（有界队列容量不足）。
    Full(T),
    /// 整批消息数量超过队列容量，永远放不下。
    TooLarge(T),
    /// 队列已关闭（所有生产端已丢弃或显式关闭）。
    Closed(T),
    /// 等待期间被调用方的取消条件中止。
    Cancelled(T),
    /// 等待超过时限，消息未入队。
    Timeout(T),
}
impl<T> SendError<T> {
    /// 取出被退回的消息，供调用方回收任务。
    /// 入参：self 为持有原消息的 `SendError<T>`，按值取得所有权。
    /// 返回值：返回失败时未被接受的原始 T，供调用方重试或释放。
    /// 异常：不返回错误。
    /// 注意事项：队列不会自动重投；重投是否安全由业务方决定。
    pub fn into_inner(self) -> T {
        match self {
            Self::Full(value)
            | Self::TooLarge(value)
            | Self::Closed(value)
            | Self::Cancelled(value)
            | Self::Timeout(value) => value,
        }
    }
}
impl<T> fmt::Display for SendError<T> {
    /// 把发送失败原因格式化为用户可读文本；原消息仍由 SendError 保留。
    /// 入参：self: &`SendError<T>` 为投递失败；formatter 为标准库格式化输出器。
    /// 返回值：文案写出成功为 Ok(())，输出端失败为 fmt::Error。
    /// 异常：不访问队列，不重试投递。
    /// 注意事项：不会展示原消息内容，消息所有权仍由 SendError 保留，可通过 into_inner 取回。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 各失败原因的用户可读文案。
        formatter.write_str(match self {
            Self::Full(_) => "队列已满",
            Self::TooLarge(_) => "批次大小超过队列容量",
            Self::Closed(_) => "队列已关闭",
            Self::Cancelled(_) => "队列等待已取消",
            Self::Timeout(_) => "队列等待超时",
        })
    }
}
impl<T: fmt::Debug> std::error::Error for SendError<T> {}

/// 出队失败原因（消息不涉及所有权，无需携带值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvError {
    /// 队列当前为空。
    Empty,
    /// 队列已关闭且已无剩余消息可取。
    Closed,
    /// 等待期间被取消条件中止。
    Cancelled,
    /// 等待超过时限。
    Timeout,
}
impl fmt::Display for RecvError {
    /// 功能：把消息领取失败原因显示为可读文案。
    /// 入参：self 为 RecvError；formatter 为标准库格式化输出器。
    /// 返回值：成功写出队空、关闭、取消或超时原因时返回 Ok(())。
    /// 异常：输出器失败返回 fmt::Error。
    /// 注意事项：只格式化状态，不改变队列或已排队的消息。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 各失败原因的用户可读文案。
        formatter.write_str(match self {
            Self::Empty => "队列为空",
            Self::Closed => "队列已关闭",
            Self::Cancelled => "队列等待已取消",
            Self::Timeout => "队列等待超时",
        })
    }
}
impl std::error::Error for RecvError {}

/// 队列推送批次的数量阈值和时间阈值；计时从本批首条消息开始。
///
/// 业务定位：监听模式（listen_batches）里定义"攒多大一批才回调消费方"：
/// 累计 weight 达到 count，或从首条消息起经过 interval 时间，就推送当前批。
#[derive(Clone, Copy, Debug)]
pub struct BatchPolicy {
    /// 数量阈值：本批累计权重达到该值时推送。
    pub count: usize,
    /// 时间阈值：从本批首次取到消息开始计时，到期且消费方允许推送时触发；不是严格时延保证。
    pub interval: Duration,
}
/// 队列提供给监听器的调度状态。
///
/// 业务定位：每轮调度前把"队列是否已关闭、当前尾批积压了多少权重"告诉
/// 监听器的 on_tick 回调，供其决定背压（暂停接收）或收尾。
#[derive(Clone, Copy, Debug)]
pub struct ListenState {
    /// 队列是否已关闭（关闭后把尾批推送完就要结束）。
    pub closed: bool,
    /// 当前尾批已累计的权重（未推送的积压量）。
    pub pending_weight: usize,
}
/// 监听器提供背压和结束条件，不负责取消息或判断定时阈值。
///
/// 业务定位：监听回调每轮返回一份"调度指令"：本批还能接收多少权重、最多
/// 取几条、是否立即推送尾批、推送后是否结束。队列据此控制节奏，实现
/// 消费方背压（下游慢了就少给）。
pub struct ListenControl {
    /// 本监听器的可用批次权重，0 表示暂停接收和推送。
    /// 业务含义：消费方处理能力指标；0 暂停接收和推送，正值参与计算本轮调度阈值。
    pub capacity: usize,
    /// 每轮最多取出的消息数；一条消息可以包含多个业务条目。
    /// 业务含义：单轮消费的消息条数上限，防止一轮取走过多任务。
    pub max_items: usize,
    /// 提前推送当前尾批。
    /// 业务含义：消费方希望立即拿到已积压的消息，不等数量/时间阈值。
    pub flush: bool,
    /// 请求结束监听；尾批存在时仍须 capacity > 0 才能推送并完成。
    /// 业务含义：消费方宣布收尾；只处理本监听器已取出的尾批，不承诺排空共享队列。
    pub finish: bool,
}
impl ListenControl {
    /// 默认控制：容量与条数同值；队列已关闭时默认收尾。
    /// 入参：capacity: usize 为本轮可接收的业务权重预算；closed: bool 为是否已经观察到队列关闭。
    /// 返回值：ListenControl，max_items 与 capacity 相同，flush/finish 跟随 closed。
    /// 异常：不返回错误，允许 capacity=0。
    /// 注意事项：capacity=0 且还有尾批时会暂停推送，即使 finish=true 也要等待调度方给出正容量。
    pub fn new(capacity: usize, closed: bool) -> Self {
        Self {
            capacity,
            max_items: capacity,
            // 队列已关闭：默认把尾批推完并结束监听。
            flush: closed,
            finish: closed,
        }
    }
}

impl<T> Receiver<T> {
    /// 注册回调并在当前调度线程持续监听，直到关闭、取消或回调失败。
    /// 消息到达时由入队唤醒；队列内部累计批次，任一阈值达到后调用 on_batch。
    /// on_tick 用于业务背压、取消和结束判断；空闲接收等待片段不超过 10ms，
    /// 但慢回调与系统线程调度仍可能延迟下一次检查。关闭后须由 on_tick 给出合适的收尾指令。
    /// 所有回调均在队列互斥锁之外执行；每个监听器串行执行自己的回调。
    /// 多个监听器共享同一队列时，消息仅投递给其中一个监听器。
    /// count 和每条消息的 weight 必须大于零；失败后不自动重投已取走的消息。
    ///
    /// 功能：把"攒批推送"的调度循环封装在队列侧。业务线程提供权重计算与两个业务回调：
    /// weight 计算消息条目数，on_tick 每轮返回背压/收尾指令，on_batch 消费一批消息。适用于"攒够一批
    /// 再落盘/提交"的消费场景，队列侧负责按数量/时间阈值触发推送。
    ///
    /// 入参：`policy` 是批次阈值；`weight` 计算每条消息的业务权重（如消息内
    /// 包含的条目数）；`on_tick` 每轮被调用并返回调度指令；`on_batch` 消费
    /// 一个批次的全部消息。
    ///
    /// 返回值：正常收尾返回 `Ok(())`；任一回调返回 Err 时监听立即结束并
    /// 传播该错误，已取走的消息不重投。
    ///
    /// 注意事项：回调都在队列锁外执行，可以安全地访问/操作同一队列；
    /// 多个监听器共享队列时每条消息只被其中一个取走（不广播）。
    /// 异常：policy.count 为零或 weight 返回零时 assert! 触发 panic；weight/on_tick/on_batch 的 panic 向外传播；
    /// on_tick/on_batch 返回 E 时直接以 Err(E) 结束，已领取消息不会自动重投。
    /// 注意事项：self: &`Receiver<T>` 为本监听器消费端，policy: BatchPolicy 为条目阈值和时间阈值；
    /// weight: Fn(&T)->usize 计算每条消息对应多少业务条目；on_tick: FnMut(ListenState)->`Result<ListenControl,E>` 提供调度指令；
    /// on_batch: FnMut(`Vec<T>`)->`Result<(),E>` 获得整批所有权，回调执行时间也计入实际响应延迟。
    pub fn listen_batches<E>(
        &self,
        policy: BatchPolicy,
        weight: impl Fn(&T) -> usize,
        mut on_tick: impl FnMut(ListenState) -> Result<ListenControl, E>,
        mut on_batch: impl FnMut(Vec<T>) -> Result<(), E>,
    ) -> Result<(), E> {
        assert!(policy.count > 0, "批量数量阈值必须大于零");
        // pending：当前尾批已取走但尚未推送的消息集合。
        let mut pending = Vec::new();
        // pending_weight：尾批累计的业务权重（达到阈值就推送）。
        let mut pending_weight = 0usize;
        // deadline：尾批首条消息入批时定的推送截止时间；None 表示尾批为空。
        let mut deadline = None;
        // closed：队列是否已关闭（关闭后只把尾批推完就结束）。
        let mut closed = false;
        loop {
            // 每轮先问消费方：当前状态下的背压/收尾指令。
            let control = on_tick(ListenState {
                closed,
                pending_weight,
            })?;
            // 消费方要求收尾且尾批为空：正常结束。
            if control.finish && pending.is_empty() {
                return Ok(());
            }
            // 本批的推送上限：数量阈值与消费方容量取较小者。
            let limit = policy.count.min(control.capacity);
            // 尾批非空且满足推送条件（权重够了 / 要求立即推 / 要求收尾 / 时间到了）：
            // 把整批交给 on_batch 并复位批次状态。
            if control.capacity > 0
                && !pending.is_empty()
                && (pending_weight >= limit
                    || control.flush
                    || control.finish
                    || deadline.is_some_and(|at| Instant::now() >= at))
            {
                // take 移走尾批（避免克隆），回调在锁外消费。
                on_batch(std::mem::take(&mut pending))?;
                pending_weight = 0;
                deadline = None;
                continue;
            }
            // 计算本次等待时长：距推送截止时间与取消检查间隔取较小者。
            let wait = deadline
                .map(|at| {
                    at.saturating_duration_since(Instant::now())
                        .min(CANCEL_INTERVAL)
                })
                .unwrap_or(CANCEL_INTERVAL);
            // 已关闭、消费方暂停接收、权重已满或条数上限为 0：本轮不再取消息，
            // 短暂休眠后回到循环开头，让 on_tick 有机会更新指令。
            if closed || control.capacity == 0 || pending_weight >= limit || control.max_items == 0
            {
                std::thread::sleep(if wait.is_zero() {
                    CANCEL_INTERVAL
                } else {
                    wait
                });
                continue;
            }
            // 正常取消息：一次最多取 (剩余权重预算 与 max_items 的较小者) 条。
            match self.recv_batch_timeout((limit - pending_weight).min(control.max_items), wait) {
                Ok(items) => {
                    // 尾批刚有第一条消息：从此刻起算时间阈值（计时从本批首条开始）。
                    if pending.is_empty() {
                        deadline = Some(Instant::now() + policy.interval);
                    }
                    // 逐条计算业务权重并并入尾批；取出条数按剩余预算估计，
            // 单条权重大于 1 时累计权重可能超过 limit，预算并不是拆分消息的硬上限。
                    for item in items {
                        let size = weight(&item);
                        assert!(size > 0, "消息权重必须大于零");
                        pending_weight = pending_weight.saturating_add(size);
                        pending.push(item);
                    }
                }
                // 队列关闭：置位后继续循环，把尾批推完再结束。
                Err(RecvError::Closed) => closed = true,
                // 超时/空：本轮没取到消息，回到循环开头重试。
                Err(RecvError::Timeout | RecvError::Empty) => {}
                // 本方法没传取消条件，不会出现 Cancelled。
                Err(RecvError::Cancelled) => unreachable!("未提供取消条件"),
            }
        }
    }
}

/// 底层通道的生产端：统一有界（sync_channel）与无界（channel）两种 mpsc 通道。
///
/// 业务定位：内部适配层，让上层队列逻辑不区分通道类型。
enum ChannelSender<T> {
    /// 有界通道生产端（容量满时 try_send 返回 Full）。
    Bounded(mpsc::SyncSender<T>),
    /// 无界通道生产端（不因容量失败，只可能因接收端消失而断开）。
    Unbounded(mpsc::Sender<T>),
}
impl<T> ChannelSender<T> {
    /// 功能：为内部队列统一有界与无界标准通道的单条投递结果。
    /// 入参：self: &`ChannelSender<T>` 为底层通道适配器；value: T 为一条消息，成功后所有权归通道。
    /// 返回值：Ok(()) 表示底层接受消息；失败返回标准库 `mpsc::TrySendError<T>` 并保留原消息。
    /// 异常：有界容量不足为 Full，底层接收端消失为 Disconnected；无界不会因容量不足返回 Full。
    /// 注意事项：这里不取得上层状态锁、不更新 len 或唤醒条件变量，调用者必须在自己的完整队列协议内协调这些步骤。
    fn try_send(&self, value: T) -> Result<(), mpsc::TrySendError<T>> {
        match self {
            // 有界：满时返回 Full。
            Self::Bounded(sender) => sender.try_send(value),
            // 无界：send 不会因容量失败，只有接收端消失才失败 → 映射为 Disconnected。
            Self::Unbounded(sender) => sender
                .send(value)
                .map_err(|error| mpsc::TrySendError::Disconnected(error.0)),
        }
    }
}

/// 队列的共享状态：底层通道、当前长度、关闭标志与两端存活计数。
///
/// 业务定位：整把锁保护的全部可变状态。len 与底层通道条数严格同步，
/// closed 决定"能否入队/是否排空结束"，senders/receivers 用于判断
/// "最后一个生产者/消费者离开"的隐式关闭时机。
struct State<T> {
    /// 底层通道生产端（始终存在）。
    sender: ChannelSender<T>,
    /// 底层通道接收端；最后一个消费端离开时 take 走并释放剩余消息。
    receiver: Option<mpsc::Receiver<T>>,
    /// 当前在队消息数（与底层通道条数保持一致）。
    len: usize,
    /// 是否已关闭：不再接受新消息；已入队的仍可取出。
    closed: bool,
    /// 存活的生产端句柄数（含原 Sender 与所有克隆）。
    senders: usize,
    /// 存活的消费端句柄数。
    receivers: usize,
}
/// 所有生产/消费端共享的队列核心。
///
/// 业务定位：Arc 包裹后跨线程共享；一把状态锁 + 两个条件变量：
/// readable 通知"有消息可取了"，writable 通知"有容量可入了"。
struct Shared<T> {
    /// 受互斥锁保护的可变状态。
    state: Mutex<State<T>>,
    /// 条件变量：入队后唤醒等待取消息的消费者。
    readable: Condvar,
    /// 条件变量：出队后唤醒等待容量的生产者。
    writable: Condvar,
    /// 容量上限；None 表示无界。
    capacity: Option<usize>,
}
impl<T> State<T> {
    // 调用方持有同一把锁，并提前确认容量；外部无法访问底层通道。
    /// 在持锁状态下把整批消息写进底层通道并同步 len。
    ///
    /// 前置条件：调用方已持有状态锁并确认容量足够。
    /// 入参：self: &mut `State<T>` 为已持锁状态；values 为逐条产生 T 消息的迭代器，每项是一条投递任务。
    /// 返回值：无；按迭代顺序投递全部消息，并逐项增加 len。
    /// 异常：实际通道与容量账本不一致时 unreachable! 触发 panic，迭代器自身的 panic 不在此捕获。
    /// 注意事项：本函数不重新检查容量或关闭状态，调用者必须在同一锁内预先完成校验。
    fn enqueue_batch(&mut self, values: impl IntoIterator<Item = T>) {
        for value in values {
            // 容量已提前确认，这里失败说明容量账本与通道不一致（程序错误）。
            if self.sender.try_send(value).is_err() {
                unreachable!("队列容量与底层通道不一致");
            }
            self.len += 1;
        }
    }
    /// 在持锁状态下取出最多 max 条消息（FIFO 前缀）。
    /// 入参：self: &mut `State<T>` 为已持锁状态；max: usize 为最多取出的消息条数。
    /// 返回值：`Vec<T>` 为 min(len,max) 条已领取消息，保持 FIFO 顺序并扣减 len。
    /// 异常：接收端不存在或通道实际消息数与 len 不一致时 panic。
    /// 注意事项：max=0 返回空集合；返回的消息所有权已离开通道，不会自动重新入队。
    fn dequeue_batch(&mut self, max: usize) -> Vec<T> {
        let count = self.len.min(max);
        let mut values = Vec::with_capacity(count);
        // 逐条从底层通道取出，保持先进先出顺序（Vec[0] 最先出队）。
        for _ in 0..count {
            values.push(
                self.receiver
                    .as_ref()
                    .expect("存活的消费端必须持有底层通道")
                    .try_recv()
                    .unwrap_or_else(|_| unreachable!("队列长度与底层通道不一致")),
            );
        }
        self.len -= count;
        values
    }
}
impl<T> Shared<T> {
    /// 获取状态锁；锁被 panic 毒化时按可恢复处理。
    /// 入参：self: &`Shared<T>` 为当前队列的共享内核。
    /// 返回值：MutexGuard<`State<T>`> 独占状态守卫，持有期间其余入队/出队等待该锁。
    /// 异常：锁中毒时取回内部状态继续操作，不返回 Result。
    /// 注意事项：只恢复访问资格，不证明 panic 前的任意业务副作用已经恢复；不要在锁内调用外部业务回调。
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
    /// 在条件变量上等待（可选限时/可取消）。
    ///
    /// 入参：`condition` 是等待的条件变量；`state` 是已持有的锁；
    /// `remaining` 是剩余时限（None=无限）；`cancellable` 为真时把单次等待
    /// 切成 ≤10ms 的小段，便于周期性检查取消条件。
    /// 返回值：无；等待期间释放状态锁，醒来时重新取锁并立即释放，外层循环随后自行重读状态。
    /// 异常：条件变量或状态锁中毒时恢复内部状态，不返回等待结果。
    /// 注意事项：condition 与 state 必须属于此 Shared 的同一状态锁；唤醒可能不是业务条件满足，
    /// 不能把被唤醒直接当作可读或可写，需外层循环重查；本方法自身不调用取消回调。
    fn wait(
        &self,
        condition: &Condvar,
        state: MutexGuard<'_, State<T>>,
        remaining: Option<Duration>,
        cancellable: bool,
    ) {
        // 可取消等待：单次最多等 CANCEL_INTERVAL，到点醒来重新检查取消条件。
        let wait = if cancellable {
            Some(remaining.unwrap_or(CANCEL_INTERVAL).min(CANCEL_INTERVAL))
        } else {
            remaining
        };
        match wait {
            Some(wait) => drop(
                condition
                    .wait_timeout(state, wait)
                    .unwrap_or_else(|error| error.into_inner()),
            ),
            None => drop(
                condition
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner()),
            ),
        }
    }
    /// 关闭队列：置关闭标志并唤醒全部等待者。
    ///
    /// 功能：被唤醒的生产者发现 closed 后立即失败；消费者仍可取剩余消息，
    /// 取空后才返回 Closed。
    /// 入参：self 为当前队列或任一端句柄。
    /// 异常：不返回业务错误。
    /// 注意事项：可重复调用；不会等消费者完成，也不会丢弃尚可领取的队内消息。
    fn close(&self) {
        self.lock().closed = true;
        // 唤醒所有等待者，让它们重新检查状态。
        self.readable.notify_all();
        self.writable.notify_all();
    }
}

/// 队列扩展接口，只约定消息存取与生命周期，不限定底层存储或调度实现。
/// 当前仅提供普通有界队列、普通无界队列；后续类型可独立实现本接口。
/// 入队成功不代表处理成功，不覆盖消息，也不按消息内容去重。
///
/// 业务定位：让业务层只依赖"任务队列"这个抽象，不关心底层是有界还是
/// 无界；便于按场景替换实现（如未来的持久化队列）。
pub trait Queue<T>: Send + Sync {
    /// None 表示无容量上限。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：返回容量上限；有界包装返回 usize，通用端/接口以 Some 表示有界、None 表示无界。
    /// 异常：不返回业务错误。
    /// 注意事项：容量是消息条数，不是消息内业务条目数，也不是字节上限。
    fn capacity(&self) -> Option<usize>;
    /// 当前在队消息数。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：返回当前还在通道中的消息条数，不含已被消费者取走但尚未处理完成的任务。
    /// 异常：不返回业务错误。
    /// 注意事项：只是锁内读取的瞬时快照；并发投递或领取后立即可能改变。
    fn len(&self) -> usize;
    /// 队列是否为空。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：当前在队消息数为零返回 true，否则 false。
    /// 异常：不返回业务错误。
    /// 注意事项：true 不等于业务任务全部完成，取走但未完成的消息不再计数。
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// 队列是否已关闭（不再接收新消息；剩余消息仍可取）。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：不再接受新消息时返回 true；仍可能存在尚未消费的消息。
    /// 异常：不返回业务错误。
    /// 注意事项：读取是瞬时快照；关闭与排空是两个不同阶段。
    fn is_closed(&self) -> bool;
    /// 关闭队列。
    /// 入参：self 为当前队列或任一端句柄。
    /// 返回值：无；同一共享队列所有句柄观察到关闭，并唤醒等待读写的线程。
    /// 异常：不返回业务错误。
    /// 注意事项：可重复调用；不会等消费者完成，也不会丢弃尚可领取的队内消息。
    fn close(&self);
    /// 阻塞投递一条消息（有界队列满时等待容量）。
    /// 入参：self 为生产端或队列；value: T 为一条任务消息，所有权转移给队列。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed。
    /// 注意事项：无时限版本可能一直等待容量；取消/超时不会关闭共享队列。取得锁或回调耗时不保证被等待预算硬性截断。入队成功仅表示任务已排队，不表示处理成功。
    fn send(&self, value: T) -> Result<(), SendError<T>>;
    /// 阻塞投递整批消息。
    /// 入参：self 为生产端或队列；values: `Vec<T>` 为按投递顺序排列的任务列表，每项为一个 T 消息，允许为空。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed；整批大于固定容量返回 TooLarge。
    /// 注意事项：无时限版本可能一直等待容量；取消/超时不会关闭共享队列。空列表直接成功，不检查关闭、取消或容量。取得锁或回调耗时不保证被等待预算硬性截断。入队成功仅表示任务已排队，不表示处理成功。
    fn send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>>;
    /// 阻塞取出下一条消息（空时等待）。
    /// 入参：self 为消费端或队列。
    /// 返回值：成功返回消息 T，其所有权交给本消费者。
    /// 异常：队空且关闭返回 Closed。
    /// 注意事项：关闭后仍能排空已有消息；并发消费者竞争领取，不会广播。成功领取后业务失败不会由队列自动重投。
    fn recv(&self) -> Result<T, RecvError>;
    /// 阻塞取出最多 max 条消息（有首项即返回，不凑满批次）。
    /// 入参：self 为消费端或队列。max: usize 为本次最多领取的消息条数，0 表示不领取。
    /// 异常：队空且关闭返回 Closed。
    /// 注意事项：关闭后仍能排空已有消息；并发消费者竞争领取，不会广播。max=0 先行成功，不检查取消或关闭。成功领取后业务失败不会由队列自动重投。
    fn recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError>;
    /// 非阻塞投递一条消息。
    /// 入参：self 为生产端或队列；value: T 为一条任务消息，所有权转移给队列。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed；没有空位返回 Full。
    /// 注意事项：非阻塞指不等待容量，仍需取得队列互斥锁。入队成功仅表示任务已排队，不表示处理成功。
    fn try_send(&self, value: T) -> Result<(), SendError<T>>;
    /// 非阻塞投递整批消息（放不下整批退回）。
    /// 入参：self 为生产端或队列；values: `Vec<T>` 为按投递顺序排列的任务列表，每项为一个 T 消息，允许为空。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed；整批超过容量返回 TooLarge、剩余空位不足返回 Full。
    /// 注意事项：非阻塞指不等待容量，仍需取得队列互斥锁。空列表直接成功，不检查关闭、取消或容量。入队成功仅表示任务已排队，不表示处理成功。
    fn try_send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>>;
    /// 非阻塞取出下一条消息。
    /// 入参：self 为消费端或队列。
    /// 返回值：成功返回消息 T，其所有权交给本消费者。
    /// 异常：队空且关闭返回 Closed；队空且未关闭返回 Empty。
    /// 注意事项：非阻塞只是不等待新消息，仍需取得互斥锁。成功领取后业务失败不会由队列自动重投。
    fn try_recv(&self) -> Result<T, RecvError>;
    /// 非阻塞取出最多 max 条消息。
    /// 入参：self 为消费端或队列。max: usize 为本次最多领取的消息条数，0 表示不领取。
    /// 返回值：成功返回按队内顺序领取的 `Vec<T>`，`Vec[0]` 最早；最多 max 条，不等待凑满批次；max=0 返回空集合。
    /// 异常：队空且关闭返回 Closed；队空且未关闭返回 Empty。
    /// 注意事项：非阻塞只是不等待新消息，仍需取得互斥锁。max=0 先行成功，不检查取消或关闭。成功领取后业务失败不会由队列自动重投。
    fn try_recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError>;
}

/// 普通有界队列。
///
/// 1. 默认在队列满时等待可用容量，在队列空时等待消息。
/// 2. 支持多个生产者、多个消费者，以及批量入队、批量取出。
/// 3. 不覆盖已有消息，每次入队的消息只能被一个消费者取走一次，不支持消息去重。
/// 4. 不保证多个生产者并发投递时的入队先后，也不要求多个消费者按顺序处理。
///
/// 业务定位：适合需要背压的任务通道——消费端处理不过来时，生产端入队会
/// 等待，天然限制内存占用与积压上限。
pub struct BoundedQueue<T> {
    /// 生产端句柄。
    sender: Sender<T>,
    /// 消费端句柄。
    receiver: Receiver<T>,
}

/// 普通无界队列。
///
/// 1. 不设置容量上限，入队不因容量而等待，在队列空时等待消息。
/// 2. 支持多个生产者、多个消费者，以及批量入队、批量取出。
/// 3. 不覆盖已有消息，每次入队的消息只能被一个消费者取走一次，不支持消息去重。
/// 4. 不保证多个生产者并发投递时的入队先后，也不要求多个消费者按顺序处理。
///
/// 业务定位：适合生产速度天然受限、或需要"永不满"的任务通道（如扫描线程
/// 边发现边投递）；代价是积压无上限，滥用会吃内存。
pub struct UnboundedQueue<T> {
    /// 生产端句柄。
    sender: Sender<T>,
    /// 消费端句柄。
    receiver: Receiver<T>,
}

/// 可克隆的生产端；capacity 返回 None 表示没有容量上限。
///
/// 业务定位：克隆分发给多个生产者线程；最后一个克隆（含原句柄）丢弃时
/// 队列自动关闭。
pub struct Sender<T>(Arc<Shared<T>>);
/// 可克隆的消费端；capacity 返回 None 表示没有容量上限。
///
/// 业务定位：克隆分发给多个消费者线程；最后一个克隆丢弃时队列自动关闭，
/// 未被消费的消息在锁外析构释放。
pub struct Receiver<T>(Arc<Shared<T>>);

/// 创建通道与共享状态，返回成对的生产端/消费端。
///
/// 入参：`capacity` 为 Some 时有界（sync_channel），None 时无界（channel）。
/// 返回值：(`Sender<T>`, `Receiver<T>`) 为共享同一状态的初始两端，存活计数均为 1。
/// 异常：不返回 Result；底层通道与内存分配由标准库管理。
/// 注意事项：capacity: `Option<usize>` 在有界生产入口中须大于零；无界不限制消息条数但仍占内存。
fn channel<T>(capacity: Option<usize>) -> (Sender<T>, Receiver<T>) {
    let (sender, receiver) = match capacity {
        Some(capacity) => {
            // 有界：容量满时入队等待。
            let (sender, receiver) = mpsc::sync_channel(capacity);
            (ChannelSender::Bounded(sender), receiver)
        }
        None => {
            // 无界：入队不因容量失败。
            let (sender, receiver) = mpsc::channel();
            (ChannelSender::Unbounded(sender), receiver)
        }
    };
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            sender,
            receiver: Some(receiver),
            len: 0,
            closed: false,
            // 初始各有一个句柄。
            senders: 1,
            receivers: 1,
        }),
        readable: Condvar::new(),
        writable: Condvar::new(),
        capacity,
    });
    (Sender(Arc::clone(&shared)), Receiver(shared))
}

impl<T> BoundedQueue<T> {
    /// 容量必须大于零；不提供容量为零的同步交接模式。
    ///
    /// 功能：创建给定容量的有界队列。
    ///
    /// 入参：`capacity` 是最大在队消息数（必须 > 0）。
    ///
    /// 异常：capacity 为 0 时 panic。
    /// 返回值：全新空的 `BoundedQueue<T>`，初始各持有一个生产/消费句柄。
    /// 注意事项：capacity: usize 是通道中消息条数的上限，不限制消息内部对象大小或消费者已领取任务数量。
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "队列容量必须大于零");
        let (sender, receiver) = channel(Some(capacity));
        Self { sender, receiver }
    }
    /// 默认等待可用容量，不丢弃、不覆盖已有消息。
    ///
    /// 功能：投递一条任务消息；队列满时阻塞等待消费者腾出容量。
    ///
    /// 错误：关闭返回 Closed、取消返回 Cancelled、超时（对应 *_timeout 变体）
    /// 返回 Timeout，均带回原消息。
    /// 入参：self 为生产端或队列；value: T 为一条任务消息，所有权转移给队列。
    /// 注意事项：无时限版本可能一直等待容量；取消/超时不会关闭共享队列。取得锁或回调耗时不保证被等待预算硬性截断。入队成功仅表示任务已排队，不表示处理成功。
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        self.sender.send(value)
    }
    /// 等待足够容量后投递整批消息；批次大小不能超过固定容量。
    ///
    /// 功能：整批原子入队：等待到容量足以容纳整批才放入，放不下任何一项时
    /// 整批退回，不会出现半批入队。
    /// 入参：self 为生产端或队列；values: `Vec<T>` 为按投递顺序排列的任务列表，每项为一个 T 消息，允许为空。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed；整批大于固定容量返回 TooLarge。
    /// 注意事项：无时限版本可能一直等待容量；取消/超时不会关闭共享队列。空列表直接成功，不检查关闭、取消或容量。取得锁或回调耗时不保证被等待预算硬性截断。入队成功仅表示任务已排队，不表示处理成功。
    pub fn send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        self.sender.send_batch(values)
    }
    /// 阻塞取出下一条消息（空时等待）。
    /// 入参：self 为消费端或队列。
    /// 返回值：成功返回消息 T，其所有权交给本消费者。
    /// 异常：队空且关闭返回 Closed。
    /// 注意事项：关闭后仍能排空已有消息；并发消费者竞争领取，不会广播。成功领取后业务失败不会由队列自动重投。
    pub fn recv(&self) -> Result<T, RecvError> {
        self.receiver.recv()
    }
    /// 阻塞取出最多 max 条消息；首项可用即返回，不等待凑满。
    /// 入参：self 为消费端或队列。max: usize 为本次最多领取的消息条数，0 表示不领取。
    /// 异常：队空且关闭返回 Closed。
    /// 注意事项：关闭后仍能排空已有消息；并发消费者竞争领取，不会广播。max=0 先行成功，不检查取消或关闭。成功领取后业务失败不会由队列自动重投。
    pub fn recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError> {
        self.receiver.recv_batch(max)
    }
    /// 非阻塞投递一条消息；满时立即返回 Full。
    /// 入参：self 为生产端或队列；value: T 为一条任务消息，所有权转移给队列。
    /// 异常：队列关闭返回 Closed；没有空位返回 Full。
    /// 注意事项：非阻塞指不等待容量，仍需取得队列互斥锁。入队成功仅表示任务已排队，不表示处理成功。
    pub fn try_send(&self, value: T) -> Result<(), SendError<T>> {
        self.sender.try_send(value)
    }
    /// 非阻塞取出下一条消息；空时立即返回 Empty。
    /// 入参：self 为消费端或队列。
    /// 异常：队空且关闭返回 Closed；队空且未关闭返回 Empty。
    /// 注意事项：非阻塞只是不等待新消息，仍需取得互斥锁。成功领取后业务失败不会由队列自动重投。
    pub fn try_recv(&self) -> Result<T, RecvError> {
        self.receiver.try_recv()
    }
    /// 整批放入，`Vec[0]` 最先入队；容量不足时整批退回。
    ///
    /// 功能：非阻塞整批入队；没有足够空位时整批退回（Full），队列内容不变。
    /// 入参：self 为生产端或队列；values: `Vec<T>` 为按投递顺序排列的任务列表，每项为一个 T 消息，允许为空。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed；整批超过容量返回 TooLarge、剩余空位不足返回 Full。
    /// 注意事项：非阻塞指不等待容量，仍需取得队列互斥锁。空列表直接成功，不检查关闭、取消或容量。入队成功仅表示任务已排队，不表示处理成功。
    pub fn try_send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        self.sender.try_send_batch(values)
    }
    /// 只放入能容纳的前缀，按原顺序返回尚未入队的后缀。
    ///
    /// 功能：非阻塞部分入队：能放多少放多少（保持顺序），放不下的后缀交还
    /// 调用方决定重试或暂存。
    /// 入参：self 为生产端或队列；values: `Vec<T>` 为按投递顺序排列的任务列表，每项为一个 T 消息，允许为空。
    /// 异常：队列关闭返回 Closed；没有空位返回 Full。
    /// 注意事项：非阻塞指不等待容量，仍需取得队列互斥锁。空列表直接成功，不检查关闭、取消或容量。入队成功仅表示任务已排队，不表示处理成功。
    pub fn try_send_batch_partial(&self, values: Vec<T>) -> Result<Vec<T>, SendError<Vec<T>>> {
        self.sender.try_send_batch_partial(values)
    }
    /// 按 FIFO 返回最多 max 项，`Vec[0]` 最先出队。
    ///
    /// 功能：非阻塞批量取出，保持先进先出顺序。
    /// 入参：self 为消费端或队列。max: usize 为本次最多领取的消息条数，0 表示不领取。
    /// 异常：队空且关闭返回 Closed；队空且未关闭返回 Empty。
    /// 注意事项：非阻塞只是不等待新消息，仍需取得互斥锁。max=0 先行成功，不检查取消或关闭。成功领取后业务失败不会由队列自动重投。
    pub fn try_recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError> {
        self.receiver.try_recv_batch(max)
    }
    /// 拆分成生产端与消费端，分发给不同线程。
    /// 入参：self 为队列本体，按值转移所有权。
    /// 返回值：返回 (`Sender<T>`, `Receiver<T>`)，用于把投递与消费分发给不同线程。
    /// 异常：不返回业务错误。
    /// 注意事项：不复制消息，不新建另一条队列；后续状态由两个端共享。
    pub fn split(self) -> (Sender<T>, Receiver<T>) {
        (self.sender, self.receiver)
    }
    /// 返回固定容量。
    /// 入参：self 为当前队列或端句柄。
    /// 异常：不返回业务错误。
    /// 注意事项：容量是消息条数，不是消息内业务条目数，也不是字节上限。
    pub fn capacity(&self) -> usize {
        self.sender.capacity().expect("有界队列必须设置容量")
    }
    /// 当前在队消息数。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：返回当前还在通道中的消息条数，不含已被消费者取走但尚未处理完成的任务。
    /// 异常：不返回业务错误。
    /// 注意事项：只是锁内读取的瞬时快照；并发投递或领取后立即可能改变。
    pub fn len(&self) -> usize {
        self.sender.len()
    }
    /// 队列是否为空。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：当前在队消息数为零返回 true，否则 false。
    /// 异常：不返回业务错误。
    /// 注意事项：true 不等于业务任务全部完成，取走但未完成的消息不再计数。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// 队列是否已关闭。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：不再接受新消息时返回 true；仍可能存在尚未消费的消息。
    /// 异常：不返回业务错误。
    /// 注意事项：读取是瞬时快照；关闭与排空是两个不同阶段。
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
    /// 关闭队列：不再接收新消息，剩余消息仍可取出。
    /// 入参：self 为当前队列或任一端句柄。
    /// 返回值：无；同一共享队列所有句柄观察到关闭，并唤醒等待读写的线程。
    /// 异常：不返回业务错误。
    /// 注意事项：可重复调用；不会等消费者完成，也不会丢弃尚可领取的队内消息。
    pub fn close(&self) {
        self.sender.close();
    }
}
impl<T> Default for UnboundedQueue<T> {
    /// 按 Default 约定创建无界队列，初始不含消息且未关闭。
    /// 入参：无。
    /// 返回值：全新空的 `UnboundedQueue<T>`，包含一对共享通道的生产/消费句柄。
    /// 异常：不返回业务错误。
    /// 注意事项：无容量背压，生产长期快于消费会累积占用内存。
    fn default() -> Self {
        Self::new()
    }
}
impl<T> UnboundedQueue<T> {
    /// 创建无容量上限的队列。
    /// 入参：无。
    /// 返回值：全新空的 `UnboundedQueue<T>`，没有消息条数容量上限。
    /// 异常：不返回业务错误。
    /// 注意事项：依赖可用内存，仍需短暂竞争状态锁；无界不代表无限存储能力。
    pub fn new() -> Self {
        let (sender, receiver) = channel(None);
        Self { sender, receiver }
    }
    /// 投递一条消息（不因容量等待；队列关闭时失败）。
    /// 入参：self 为生产端或队列；value: T 为一条任务消息，所有权转移给队列。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed。
    /// 注意事项：无时限版本可能一直等待容量；取消/超时不会关闭共享队列。取得锁或回调耗时不保证被等待预算硬性截断。入队成功仅表示任务已排队，不表示处理成功。
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        self.sender.send(value)
    }
    /// 投递整批消息（不因容量等待；队列关闭时失败）。
    /// 入参：self 为生产端或队列；values: `Vec<T>` 为按投递顺序排列的任务列表，每项为一个 T 消息，允许为空。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed；整批大于固定容量返回 TooLarge。
    /// 注意事项：无时限版本可能一直等待容量；取消/超时不会关闭共享队列。空列表直接成功，不检查关闭、取消或容量。取得锁或回调耗时不保证被等待预算硬性截断。入队成功仅表示任务已排队，不表示处理成功。
    pub fn send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        self.sender.send_batch(values)
    }
    /// 阻塞取出下一条消息（空时等待）。
    /// 入参：self 为消费端或队列。
    /// 返回值：成功返回消息 T，其所有权交给本消费者。
    /// 异常：队空且关闭返回 Closed。
    /// 注意事项：关闭后仍能排空已有消息；并发消费者竞争领取，不会广播。成功领取后业务失败不会由队列自动重投。
    pub fn recv(&self) -> Result<T, RecvError> {
        self.receiver.recv()
    }
    /// 阻塞取出最多 max 条消息；首项可用即返回，不等待凑满。
    /// 入参：self 为消费端或队列。max: usize 为本次最多领取的消息条数，0 表示不领取。
    /// 异常：队空且关闭返回 Closed。
    /// 注意事项：关闭后仍能排空已有消息；并发消费者竞争领取，不会广播。max=0 先行成功，不检查取消或关闭。成功领取后业务失败不会由队列自动重投。
    pub fn recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError> {
        self.receiver.recv_batch(max)
    }
    /// 非阻塞投递一条消息。
    /// 入参：self 为生产端或队列；value: T 为一条任务消息，所有权转移给队列。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed；没有空位返回 Full。
    /// 注意事项：非阻塞指不等待容量，仍需取得队列互斥锁。入队成功仅表示任务已排队，不表示处理成功。
    pub fn try_send(&self, value: T) -> Result<(), SendError<T>> {
        self.sender.try_send(value)
    }
    /// 非阻塞取出下一条消息。
    /// 入参：self 为消费端或队列。
    /// 返回值：成功返回消息 T，其所有权交给本消费者。
    /// 异常：队空且关闭返回 Closed；队空且未关闭返回 Empty。
    /// 注意事项：非阻塞只是不等待新消息，仍需取得互斥锁。成功领取后业务失败不会由队列自动重投。
    pub fn try_recv(&self) -> Result<T, RecvError> {
        self.receiver.try_recv()
    }
    /// 非阻塞投递整批消息。
    /// 入参：self 为生产端或队列；values: `Vec<T>` 为按投递顺序排列的任务列表，每项为一个 T 消息，允许为空。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed；整批超过容量返回 TooLarge、剩余空位不足返回 Full。
    /// 注意事项：非阻塞指不等待容量，仍需取得队列互斥锁。空列表直接成功，不检查关闭、取消或容量。入队成功仅表示任务已排队，不表示处理成功。
    pub fn try_send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        self.sender.try_send_batch(values)
    }
    /// 无界队列可放入整个批次，成功时返回空 Vec。
    ///
    /// 功能：与有界队列接口对齐；无界容量足够，正常时整个批次入队并返回
    /// 空后缀。
    /// 入参：self 为生产端或队列；values: `Vec<T>` 为按投递顺序排列的任务列表，每项为一个 T 消息，允许为空。
    /// 异常：队列关闭返回 Closed；没有空位返回 Full。
    /// 注意事项：非阻塞指不等待容量，仍需取得队列互斥锁。空列表直接成功，不检查关闭、取消或容量。入队成功仅表示任务已排队，不表示处理成功。
    pub fn try_send_batch_partial(&self, values: Vec<T>) -> Result<Vec<T>, SendError<Vec<T>>> {
        self.sender.try_send_batch_partial(values)
    }
    /// 非阻塞批量取出最多 max 条（FIFO 前缀）。
    /// 入参：self 为消费端或队列。max: usize 为本次最多领取的消息条数，0 表示不领取。
    /// 返回值：成功返回按队内顺序领取的 `Vec<T>`，`Vec[0]` 最早；最多 max 条，不等待凑满批次；max=0 返回空集合。
    /// 异常：队空且关闭返回 Closed；队空且未关闭返回 Empty。
    /// 注意事项：非阻塞只是不等待新消息，仍需取得互斥锁。max=0 先行成功，不检查取消或关闭。成功领取后业务失败不会由队列自动重投。
    pub fn try_recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError> {
        self.receiver.try_recv_batch(max)
    }
    /// 拆分成生产端与消费端。
    /// 入参：self 为队列本体，按值转移所有权。
    /// 返回值：返回 (`Sender<T>`, `Receiver<T>`)，用于把投递与消费分发给不同线程。
    /// 异常：不返回业务错误。
    /// 注意事项：不复制消息，不新建另一条队列；后续状态由两个端共享。
    pub fn split(self) -> (Sender<T>, Receiver<T>) {
        (self.sender, self.receiver)
    }
    /// 无界队列恒为 None。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：返回容量上限；有界包装返回 usize，通用端/接口以 Some 表示有界、None 表示无界。
    /// 异常：不返回业务错误。
    /// 注意事项：容量是消息条数，不是消息内业务条目数，也不是字节上限。
    pub fn capacity(&self) -> Option<usize> {
        None
    }
    /// 当前在队消息数。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：返回当前还在通道中的消息条数，不含已被消费者取走但尚未处理完成的任务。
    /// 异常：不返回业务错误。
    /// 注意事项：只是锁内读取的瞬时快照；并发投递或领取后立即可能改变。
    pub fn len(&self) -> usize {
        self.sender.len()
    }
    /// 队列是否为空。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：当前在队消息数为零返回 true，否则 false。
    /// 异常：不返回业务错误。
    /// 注意事项：true 不等于业务任务全部完成，取走但未完成的消息不再计数。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// 队列是否已关闭。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：不再接受新消息时返回 true；仍可能存在尚未消费的消息。
    /// 异常：不返回业务错误。
    /// 注意事项：读取是瞬时快照；关闭与排空是两个不同阶段。
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
    /// 关闭队列。
    /// 入参：self 为当前队列或任一端句柄。
    /// 返回值：无；同一共享队列所有句柄观察到关闭，并唤醒等待读写的线程。
    /// 异常：不返回业务错误。
    /// 注意事项：可重复调用；不会等消费者完成，也不会丢弃尚可领取的队内消息。
    pub fn close(&self) {
        self.sender.close();
    }
}

/// 有界队列实现统一 Queue 接口。
impl<T: Send> Queue<T> for BoundedQueue<T> {
    /// 返回队列的容量上限；有界队列为固定数值，无界队列返回 None。
    /// 入参：self 为当前队列或端句柄。
    /// 异常：不返回业务错误。
    /// 注意事项：容量是消息条数，不是消息内业务条目数，也不是字节上限。
    fn capacity(&self) -> Option<usize> {
        self.sender.capacity()
    }
    /// 返回当前仍在队列中、尚未被消费者取走的消息数。
    /// 入参：self 为当前队列或端句柄。
    /// 异常：不返回业务错误。
    /// 注意事项：只是锁内读取的瞬时快照；并发投递或领取后立即可能改变。
    fn len(&self) -> usize {
        self.sender.len()
    }
    /// 检查是否已关闭；关闭后不再接收新消息，但队列可能仍有待消费消息。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：不再接受新消息时返回 true；仍可能存在尚未消费的消息。
    /// 异常：不返回业务错误。
    /// 注意事项：读取是瞬时快照；关闭与排空是两个不同阶段。
    fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
    /// 关闭队列生产端；已入队消息仍可由消费者排空。
    /// 入参：self 为当前队列或任一端句柄。
    /// 返回值：无；同一共享队列所有句柄观察到关闭，并唤醒等待读写的线程。
    /// 异常：不返回业务错误。
    /// 注意事项：可重复调用；不会等消费者完成，也不会丢弃尚可领取的队内消息。
    fn close(&self) {
        self.sender.close();
    }
    /// 阻塞投递一条业务消息，直到有界队列腾出空间或队列关闭。
    /// 入参：self 为生产端或队列；value: T 为一条任务消息，所有权转移给队列。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed。
    /// 注意事项：无时限版本可能一直等待容量；取消/超时不会关闭共享队列。取得锁或回调耗时不保证被等待预算硬性截断。入队成功仅表示任务已排队，不表示处理成功。
    fn send(&self, value: T) -> Result<(), SendError<T>> {
        self.sender.send(value)
    }
    /// 阻塞投递一组消息；失败时整批退回调用方，不丢失任务所有权。
    /// 入参：self 为生产端或队列；values: `Vec<T>` 为按投递顺序排列的任务列表，每项为一个 T 消息，允许为空。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed；整批大于固定容量返回 TooLarge。
    /// 注意事项：无时限版本可能一直等待容量；取消/超时不会关闭共享队列。空列表直接成功，不检查关闭、取消或容量。取得锁或回调耗时不保证被等待预算硬性截断。入队成功仅表示任务已排队，不表示处理成功。
    fn send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        self.sender.send_batch(values)
    }
    /// 阻塞等待一条消息；收到的消息只交给当前消费者一次。
    /// 入参：self 为消费端或队列。
    /// 返回值：成功返回消息 T，其所有权交给本消费者。
    /// 异常：队空且关闭返回 Closed。
    /// 注意事项：关闭后仍能排空已有消息；并发消费者竞争领取，不会广播。成功领取后业务失败不会由队列自动重投。
    fn recv(&self) -> Result<T, RecvError> {
        self.receiver.recv()
    }
    /// 最多取出 max 条消息；首条可用后即返回，不等待凑满整个批次。
    /// 入参：self 为消费端或队列。max: usize 为本次最多领取的消息条数，0 表示不领取。
    /// 异常：队空且关闭返回 Closed。
    /// 注意事项：关闭后仍能排空已有消息；并发消费者竞争领取，不会广播。max=0 先行成功，不检查取消或关闭。成功领取后业务失败不会由队列自动重投。
    fn recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError> {
        self.receiver.recv_batch(max)
    }
    /// 非阻塞投递一条消息；容量不足或队列关闭时立即退回。
    /// 入参：self 为生产端或队列；value: T 为一条任务消息，所有权转移给队列。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed；没有空位返回 Full。
    /// 注意事项：非阻塞指不等待容量，仍需取得队列互斥锁。入队成功仅表示任务已排队，不表示处理成功。
    fn try_send(&self, value: T) -> Result<(), SendError<T>> {
        self.sender.try_send(value)
    }
    /// 非阻塞整批投递；容量不足时整批不入队。
    /// 入参：self 为生产端或队列；values: `Vec<T>` 为按投递顺序排列的任务列表，每项为一个 T 消息，允许为空。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed；整批超过容量返回 TooLarge、剩余空位不足返回 Full。
    /// 注意事项：非阻塞指不等待容量，仍需取得队列互斥锁。空列表直接成功，不检查关闭、取消或容量。入队成功仅表示任务已排队，不表示处理成功。
    fn try_send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        self.sender.try_send_batch(values)
    }
    /// 非阻塞取出一条消息；队列为空或已关闭时立即返回对应状态。
    /// 入参：self 为消费端或队列。
    /// 异常：队空且关闭返回 Closed；队空且未关闭返回 Empty。
    /// 注意事项：非阻塞只是不等待新消息，仍需取得互斥锁。成功领取后业务失败不会由队列自动重投。
    fn try_recv(&self) -> Result<T, RecvError> {
        self.receiver.try_recv()
    }
    /// 非阻塞按 FIFO 顺序取出最多 max 条消息。
    /// 入参：self 为消费端或队列。max: usize 为本次最多领取的消息条数，0 表示不领取。
    /// 返回值：成功返回按队内顺序领取的 `Vec<T>`，`Vec[0]` 最早；最多 max 条，不等待凑满批次；max=0 返回空集合。
    /// 异常：队空且关闭返回 Closed；队空且未关闭返回 Empty。
    /// 注意事项：非阻塞只是不等待新消息，仍需取得互斥锁。max=0 先行成功，不检查取消或关闭。成功领取后业务失败不会由队列自动重投。
    fn try_recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError> {
        self.receiver.try_recv_batch(max)
    }
}

/// 无界队列实现统一 Queue 接口。
impl<T: Send> Queue<T> for UnboundedQueue<T> {
    /// 返回队列的容量上限；有界队列为固定数值，无界队列返回 None。
    /// 入参：self 为当前队列或端句柄。
    /// 异常：不返回业务错误。
    /// 注意事项：容量是消息条数，不是消息内业务条目数，也不是字节上限。
    fn capacity(&self) -> Option<usize> {
        self.sender.capacity()
    }
    /// 返回当前仍在队列中、尚未被消费者取走的消息数。
    /// 入参：self 为当前队列或端句柄。
    /// 异常：不返回业务错误。
    /// 注意事项：只是锁内读取的瞬时快照；并发投递或领取后立即可能改变。
    fn len(&self) -> usize {
        self.sender.len()
    }
    /// 检查是否已关闭；关闭后不再接收新消息，但队列可能仍有待消费消息。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：不再接受新消息时返回 true；仍可能存在尚未消费的消息。
    /// 异常：不返回业务错误。
    /// 注意事项：读取是瞬时快照；关闭与排空是两个不同阶段。
    fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
    /// 关闭队列生产端；已入队消息仍可由消费者排空。
    /// 入参：self 为当前队列或任一端句柄。
    /// 返回值：无；同一共享队列所有句柄观察到关闭，并唤醒等待读写的线程。
    /// 异常：不返回业务错误。
    /// 注意事项：可重复调用；不会等消费者完成，也不会丢弃尚可领取的队内消息。
    fn close(&self) {
        self.sender.close();
    }
    /// 阻塞投递一条业务消息，直到有界队列腾出空间或队列关闭。
    /// 入参：self 为生产端或队列；value: T 为一条任务消息，所有权转移给队列。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed。
    /// 注意事项：无时限版本可能一直等待容量；取消/超时不会关闭共享队列。取得锁或回调耗时不保证被等待预算硬性截断。入队成功仅表示任务已排队，不表示处理成功。
    fn send(&self, value: T) -> Result<(), SendError<T>> {
        self.sender.send(value)
    }
    /// 阻塞投递一组消息；失败时整批退回调用方，不丢失任务所有权。
    /// 入参：self 为生产端或队列；values: `Vec<T>` 为按投递顺序排列的任务列表，每项为一个 T 消息，允许为空。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed；整批大于固定容量返回 TooLarge。
    /// 注意事项：无时限版本可能一直等待容量；取消/超时不会关闭共享队列。空列表直接成功，不检查关闭、取消或容量。取得锁或回调耗时不保证被等待预算硬性截断。入队成功仅表示任务已排队，不表示处理成功。
    fn send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        self.sender.send_batch(values)
    }
    /// 阻塞等待一条消息；收到的消息只交给当前消费者一次。
    /// 入参：self 为消费端或队列。
    /// 返回值：成功返回消息 T，其所有权交给本消费者。
    /// 异常：队空且关闭返回 Closed。
    /// 注意事项：关闭后仍能排空已有消息；并发消费者竞争领取，不会广播。成功领取后业务失败不会由队列自动重投。
    fn recv(&self) -> Result<T, RecvError> {
        self.receiver.recv()
    }
    /// 最多取出 max 条消息；首条可用后即返回，不等待凑满整个批次。
    /// 入参：self 为消费端或队列。max: usize 为本次最多领取的消息条数，0 表示不领取。
    /// 异常：队空且关闭返回 Closed。
    /// 注意事项：关闭后仍能排空已有消息；并发消费者竞争领取，不会广播。max=0 先行成功，不检查取消或关闭。成功领取后业务失败不会由队列自动重投。
    fn recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError> {
        self.receiver.recv_batch(max)
    }
    /// 非阻塞投递一条消息；容量不足或队列关闭时立即退回。
    /// 入参：self 为生产端或队列；value: T 为一条任务消息，所有权转移给队列。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed；没有空位返回 Full。
    /// 注意事项：非阻塞指不等待容量，仍需取得队列互斥锁。入队成功仅表示任务已排队，不表示处理成功。
    fn try_send(&self, value: T) -> Result<(), SendError<T>> {
        self.sender.try_send(value)
    }
    /// 非阻塞整批投递；容量不足时整批不入队。
    /// 入参：self 为生产端或队列；values: `Vec<T>` 为按投递顺序排列的任务列表，每项为一个 T 消息，允许为空。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed；整批超过容量返回 TooLarge、剩余空位不足返回 Full。
    /// 注意事项：非阻塞指不等待容量，仍需取得队列互斥锁。空列表直接成功，不检查关闭、取消或容量。入队成功仅表示任务已排队，不表示处理成功。
    fn try_send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        self.sender.try_send_batch(values)
    }
    /// 非阻塞取出一条消息；队列为空或已关闭时立即返回对应状态。
    /// 入参：self 为消费端或队列。
    /// 异常：队空且关闭返回 Closed；队空且未关闭返回 Empty。
    /// 注意事项：非阻塞只是不等待新消息，仍需取得互斥锁。成功领取后业务失败不会由队列自动重投。
    fn try_recv(&self) -> Result<T, RecvError> {
        self.receiver.try_recv()
    }
    /// 非阻塞按 FIFO 顺序取出最多 max 条消息。
    /// 入参：self 为消费端或队列。max: usize 为本次最多领取的消息条数，0 表示不领取。
    /// 返回值：成功返回按队内顺序领取的 `Vec<T>`，`Vec[0]` 最早；最多 max 条，不等待凑满批次；max=0 返回空集合。
    /// 异常：队空且关闭返回 Closed；队空且未关闭返回 Empty。
    /// 注意事项：非阻塞只是不等待新消息，仍需取得互斥锁。max=0 先行成功，不检查取消或关闭。成功领取后业务失败不会由队列自动重投。
    fn try_recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError> {
        self.receiver.try_recv_batch(max)
    }
}

/// 克隆生产端：把新句柄计入存活数，供"最后一个生产者关闭队列"判断。
impl<T> Clone for Sender<T> {
    /// 功能：新增同一队列的生产端句柄，让多个业务线程共同投递任务。
    /// 入参：self 为 `Sender<T>`，共享同一 `Shared<T>`。
    /// 返回值：共享队列的另一个 Sender；生产端存活计数加一。
    /// 异常：互斥锁中毒时恢复内部状态，不返回错误。
    /// 注意事项：不复制消息；全部生产端释放后队列才自动关闭。
    fn clone(&self) -> Self {
        self.0.lock().senders += 1;
        Self(Arc::clone(&self.0))
    }
}
/// 克隆消费端：把新句柄计入存活数，供"最后一个消费者释放剩余消息"判断。
impl<T> Clone for Receiver<T> {
    /// 功能：新增同一队列的消费端，让多个工作线程竞争领取任务。
    /// 入参：self 为 `Receiver<T>`，共享同一 `Shared<T>`。
    /// 返回值：共享队列的另一个 Receiver；消费端存活计数加一。
    /// 异常：互斥锁中毒时恢复内部状态，不返回错误。
    /// 注意事项：不广播消息；最后一个消费端释放后关闭并释放未消费内容。
    fn clone(&self) -> Self {
        self.0.lock().receivers += 1;
        Self(Arc::clone(&self.0))
    }
}
/// 生产端丢弃：存活数减一；全部生产端离开时隐式关闭队列。
///
/// 业务含义：生产者任务全部结束时，队列自动进入关闭状态，等待中的消费者
/// 排空剩余消息后得到 Closed。
impl<T> Drop for Sender<T> {
    /// 生产端句柄释放时递减生产者数量；最后一个生产者离开会关闭队列。
    /// 入参：self: &mut `Sender<T>` 为即将析构的生产端。
    /// 返回值：无；递减存活计数，最后一端释放时设置 closed，随后唤醒等待读消息的消费者。
    /// 异常：锁中毒时恢复内部状态，不返回 Result。
    /// 注意事项：队内消息仍可由存活消费者领取；析构不等待业务处理完成。
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.senders -= 1;
        if state.senders == 0 {
            state.closed = true;
        }
        drop(state);
        // 唤醒消费者：让它们看到关闭标志并决定何时收尾。
        self.0.readable.notify_all();
    }
}
/// 消费端丢弃：存活数减一；最后一个消费者离开时关闭队列并释放剩余消息。
impl<T> Drop for Receiver<T> {
    /// 消费端句柄释放时递减消费者数量；最后一个消费者离开会关闭并释放未读消息。
    /// 入参：self: &mut `Receiver<T>` 为即将析构的消费端。
    /// 返回值：无；最后一端释放时关闭通道并移走底层接收器，剩余消息在状态锁外析构。
    /// 异常：消息 T 自身的析构可能 panic，本队列不在此捕获。
    /// 注意事项：在锁外释放消息避免其析构重新访问队列时死锁；生产者等待会被唤醒并观察关闭。
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.receivers -= 1;
        // 最后一个消费端：没有消费者了，队列无存在意义——
        // 关闭队列、清零计数，并把底层接收端（连同未取消息）拿走。
        let abandoned = if state.receivers == 0 {
            state.closed = true;
            state.len = 0;
            state.receiver.take()
        } else {
            None
        };
        drop(state);
        // 唤醒等待中的生产者：让它们看到关闭标志并失败返回。
        self.0.writable.notify_all();
        // 在锁外直接释放底层接收端及其消息，避免为清空无界队列另建同样大的集合。
        // 任务的析构可能访问队列，不能在内部互斥锁中执行。
        drop(abandoned);
    }
}

impl<T> Sender<T> {
    /// 容量上限（None 表示无界）。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：返回容量上限；有界包装返回 usize，通用端/接口以 Some 表示有界、None 表示无界。
    /// 异常：不返回业务错误。
    /// 注意事项：容量是消息条数，不是消息内业务条目数，也不是字节上限。
    pub fn capacity(&self) -> Option<usize> {
        self.0.capacity
    }
    /// 当前在队消息数。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：返回当前还在通道中的消息条数，不含已被消费者取走但尚未处理完成的任务。
    /// 异常：不返回业务错误。
    /// 注意事项：只是锁内读取的瞬时快照；并发投递或领取后立即可能改变。
    pub fn len(&self) -> usize {
        self.0.lock().len
    }
    /// 队列是否为空。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：当前在队消息数为零返回 true，否则 false。
    /// 异常：不返回业务错误。
    /// 注意事项：true 不等于业务任务全部完成，取走但未完成的消息不再计数。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// 队列是否已关闭。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：不再接受新消息时返回 true；仍可能存在尚未消费的消息。
    /// 异常：不返回业务错误。
    /// 注意事项：读取是瞬时快照；关闭与排空是两个不同阶段。
    pub fn is_closed(&self) -> bool {
        self.0.lock().closed
    }
    /// 关闭队列：生产者不再能投递，消费者可排空剩余消息。
    /// 入参：self 为当前队列或任一端句柄。
    /// 返回值：无；同一共享队列所有句柄观察到关闭，并唤醒等待读写的线程。
    /// 异常：不返回业务错误。
    /// 注意事项：可重复调用；不会等消费者完成，也不会丢弃尚可领取的队内消息。
    pub fn close(&self) {
        self.0.close();
    }
    /// 非阻塞投递一条消息。
    ///
    /// 功能：加锁检查关闭标志后 try_send；成功时更新 len 并唤醒一个消费者。
    ///
    /// 错误：关闭返回 Closed；有界队列满返回 Full；均带回原消息。
    /// 入参：self 为生产端或队列；value: T 为一条任务消息，所有权转移给队列。
    /// 注意事项：非阻塞指不等待容量，仍需取得队列互斥锁。入队成功仅表示任务已排队，不表示处理成功。
    pub fn try_send(&self, value: T) -> Result<(), SendError<T>> {
        let mut state = self.0.lock();
        // 已关闭：不再接受任何新消息。
        if state.closed {
            return Err(SendError::Closed(value));
        }
        match state.sender.try_send(value) {
            Ok(()) => {
                // 入队成功：同步长度账本。
                state.len += 1;
                // 先放锁再唤醒，避免消费者醒来后立刻又卡在锁上。
                drop(state);
                // 唤醒一个等待取消息的消费者即可。
                self.0.readable.notify_one();
                Ok(())
            }
            // 有界队列满：原样退回消息。
            Err(mpsc::TrySendError::Full(value)) => Err(SendError::Full(value)),
            // 接收端已消失（等于关闭）：退回消息。
            Err(mpsc::TrySendError::Disconnected(value)) => Err(SendError::Closed(value)),
        }
    }
    /// 在一次加锁内整批入队。`Vec[0]` 最先入队，其他生产者不能插入批次内部。
    /// 空 Vec 是无操作；有界队列可能返回 TooLarge 或 Full，无界队列不检查容量。
    ///
    /// 功能：整批原子入队：空批次无操作；批次超容量 TooLarge；容量不足 Full
    /// （整批退回，队列内容不变）；成功时一次锁内完成，其他生产者插不进队。
    /// 入参：self 为生产端或队列；values: `Vec<T>` 为按投递顺序排列的任务列表，每项为一个 T 消息，允许为空。
    /// 异常：队列关闭返回 Closed；整批超过容量返回 TooLarge、剩余空位不足返回 Full。
    /// 注意事项：非阻塞指不等待容量，仍需取得队列互斥锁。空列表直接成功，不检查关闭、取消或容量。入队成功仅表示任务已排队，不表示处理成功。
    pub fn try_send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        // 空批次：无操作成功。
        if values.is_empty() {
            return Ok(());
        }
        // 批次数量超过固定容量：永远放不下，提前失败。
        if self
            .capacity()
            .is_some_and(|capacity| values.len() > capacity)
        {
            return Err(SendError::TooLarge(values));
        }
        let mut state = self.0.lock();
        if state.closed {
            return Err(SendError::Closed(values));
        }
        // 剩余空位不足以容纳整批：整批退回。
        if self
            .capacity()
            .is_some_and(|capacity| values.len() > capacity - state.len)
        {
            return Err(SendError::Full(values));
        }
        // 锁内连续入队：批次内部不被其他生产者插入。
        state.enqueue_batch(values);
        drop(state);
        // 批量唤醒消费者（多个消费者可并行取）。
        self.0.readable.notify_all();
        Ok(())
    }
    /// 在一次加锁内放入有序前缀，返回有序后缀；没有空位时整批返回 Full。
    /// 成功放入的前缀不会被其他生产者插入，但后缀再次提交是另一次独立操作。
    ///
    /// 功能：非阻塞部分入队：按顺序放入能容纳的前缀，返回放不下的后缀，
    /// 由调用方决定重试/暂存；没有空位时整批 Full。
    /// 入参：self 为生产端或队列；values: `Vec<T>` 为按投递顺序排列的任务列表，每项为一个 T 消息，允许为空。
    /// 异常：队列关闭返回 Closed；没有空位返回 Full。
    /// 注意事项：非阻塞指不等待容量，仍需取得队列互斥锁。空列表直接成功，不检查关闭、取消或容量。入队成功仅表示任务已排队，不表示处理成功。
    pub fn try_send_batch_partial(&self, mut values: Vec<T>) -> Result<Vec<T>, SendError<Vec<T>>> {
        // 空批次：原样返回空后缀。
        if values.is_empty() {
            return Ok(values);
        }
        let mut state = self.0.lock();
        if state.closed {
            return Err(SendError::Closed(values));
        }
        // 可放入条数 = 批次长度与剩余空位（无界时即批次长度）的较小者。
        let count = self.capacity().map_or(values.len(), |capacity| {
            values.len().min(capacity - state.len)
        });
        // 一个空位都没有：整批退回 Full。
        if count == 0 {
            return Err(SendError::Full(values));
        }
        // 原地移走前缀，返回原 Vec 中的有序后缀，不额外分配整份剩余集合。
        state.enqueue_batch(values.drain(..count));
        drop(state);
        self.0.readable.notify_all();
        Ok(values)
    }
    /// 整批放入：有界队列等待足够容量，无界队列不等待容量。
    /// 取消、关闭或超时均不放入任何一项。
    ///
    /// 功能：阻塞整批入队（原子语义：要么整批进，要么一项不进）。
    /// 入参：self 为生产端或队列；values: `Vec<T>` 为按投递顺序排列的任务列表，每项为一个 T 消息，允许为空。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed；整批大于固定容量返回 TooLarge。
    /// 注意事项：无时限版本可能一直等待容量；取消/超时不会关闭共享队列。空列表直接成功，不检查关闭、取消或容量。取得锁或回调耗时不保证被等待预算硬性截断。入队成功仅表示任务已排队，不表示处理成功。
    pub fn send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        self.wait_send_batch(values, None, None)
    }
    /// 带取消条件的阻塞整批入队；取消条件返回 true 时中止等待并退回整批。
    /// 入参：self 为生产端或队列；values: `Vec<T>` 为按投递顺序排列的任务列表，每项为一个 T 消息，允许为空。cancelled: impl Fn() -> bool 为取消检查，返回 true 表示本次投递应停止。
    /// 异常：队列关闭返回 Closed；整批大于固定容量返回 TooLarge；取消检查为真返回 Cancelled；回调 panic 向外传播。
    /// 注意事项：无时限版本可能一直等待容量；取消/超时不会关闭共享队列。空列表直接成功，不检查关闭、取消或容量。取得锁或回调耗时不保证被等待预算硬性截断。入队成功仅表示任务已排队，不表示处理成功。
    pub fn send_batch_with(
        &self,
        values: Vec<T>,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), SendError<Vec<T>>> {
        self.wait_send_batch(values, None, Some(&cancelled))
    }
    /// 带超时的阻塞整批入队；超时未放入整批退回。
    /// 入参：self 为生产端或队列；values: `Vec<T>` 为按投递顺序排列的任务列表，每项为一个 T 消息，允许为空。timeout: Duration 为容量等待预算，零值仍可在当前有空位时成功。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed；整批大于固定容量返回 TooLarge；等待容量到期返回 Timeout。
    /// 注意事项：无时限版本可能一直等待容量；取消/超时不会关闭共享队列。空列表直接成功，不检查关闭、取消或容量。取得锁或回调耗时不保证被等待预算硬性截断。入队成功仅表示任务已排队，不表示处理成功。
    pub fn send_batch_timeout(
        &self,
        values: Vec<T>,
        timeout: Duration,
    ) -> Result<(), SendError<Vec<T>>> {
        self.wait_send_batch(values, Some(timeout), None)
    }
    /// 同时带超时与取消条件的阻塞整批入队。
    /// 入参：self 为生产端或队列；values: `Vec<T>` 为按投递顺序排列的任务列表，每项为一个 T 消息，允许为空。timeout: Duration 为容量等待预算，零值仍可在当前有空位时成功。cancelled: impl Fn() -> bool 为取消检查，返回 true 表示本次投递应停止。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed；整批大于固定容量返回 TooLarge；取消检查为真返回 Cancelled；回调 panic 向外传播；等待容量到期返回 Timeout。
    /// 注意事项：无时限版本可能一直等待容量；取消/超时不会关闭共享队列。空列表直接成功，不检查关闭、取消或容量。取得锁或回调耗时不保证被等待预算硬性截断。入队成功仅表示任务已排队，不表示处理成功。
    pub fn send_batch_timeout_with(
        &self,
        values: Vec<T>,
        timeout: Duration,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), SendError<Vec<T>>> {
        self.wait_send_batch(values, Some(timeout), Some(&cancelled))
    }
    /// 功能：统一驱动整批投递的容量等待，协调生产速度与消费者处理能力。
    /// 入参：self 为共享队列的生产端；values: `Vec<T>` 为整批待投递消息，元素顺序决定批内顺序；timeout: `Option<Duration>` 为等待预算，None 无限等待；cancelled: `Option<&dyn Fn()->bool>` 为可选取消检查。
    /// 返回值：Ok(()) 表示全部消息已一次加锁连续投递。
    /// 异常：关闭、取消、容量等待到期分别返回 Closed/Cancelled/Timeout，失败均带回原消息，整批大于容量返回 TooLarge。取消回调 panic 向外传播。
    /// 注意事项：循环每次在锁外查取消，再持状态锁检查业务条件；等待释放锁，醒来重新检查，不依赖一次唤醒即成功。空批次先成功，不查取消或关闭。有容量/消息时先尝试成功，再判断预算，故零预算也可能立即成功；等待预算不硬中断锁竞争和回调。
    fn wait_send_batch(
        &self,
        values: Vec<T>,
        timeout: Option<Duration>,
        cancelled: Option<&dyn Fn() -> bool>,
    ) -> Result<(), SendError<Vec<T>>> {
        if values.is_empty() {
            return Ok(());
        }
        // 批次超容量：永远放不下，立即失败。
        if self
            .capacity()
            .is_some_and(|capacity| values.len() > capacity)
        {
            return Err(SendError::TooLarge(values));
        }
        let started = Instant::now();
        loop {
            // 取消检查在加锁前执行：取消函数可能访问队列，避免在锁内回调。
            if cancelled.is_some_and(|check| check()) {
                return Err(SendError::Cancelled(values));
            }
            let mut state = self.0.lock();
            if state.closed {
                return Err(SendError::Closed(values));
            }
            // 空位够整批（无界恒够）：一次锁内整批入队。
            if self
                .capacity()
                .is_none_or(|capacity| values.len() <= capacity - state.len)
            {
                state.enqueue_batch(values);
                drop(state);
                self.0.readable.notify_all();
                return Ok(());
            }
            // 容量不够：计算剩余时限；已到零直接超时。
            let remaining = timeout.map(|timeout| timeout.saturating_sub(started.elapsed()));
            if remaining == Some(Duration::ZERO) {
                return Err(SendError::Timeout(values));
            }
            // 在 writable 上等待消费者腾出容量。
            self.0
                .wait(&self.0.writable, state, remaining, cancelled.is_some());
        }
    }
    /// 阻塞投递一条消息：有界队列满时等待可用容量。
    /// 入参：self 为生产端或队列；value: T 为一条任务消息，所有权转移给队列。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed。
    /// 注意事项：无时限版本可能一直等待容量；取消/超时不会关闭共享队列。取得锁或回调耗时不保证被等待预算硬性截断。入队成功仅表示任务已排队，不表示处理成功。
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        self.wait_send(value, None, None)
    }
    /// 带取消条件的阻塞投递；取消条件返回 true 时中止并退回消息。
    /// 入参：self 为生产端或队列；value: T 为一条任务消息，所有权转移给队列。cancelled: impl Fn() -> bool 为取消检查，返回 true 表示本次投递应停止。
    /// 异常：队列关闭返回 Closed；取消检查为真返回 Cancelled；回调 panic 向外传播。
    /// 注意事项：无时限版本可能一直等待容量；取消/超时不会关闭共享队列。取得锁或回调耗时不保证被等待预算硬性截断。入队成功仅表示任务已排队，不表示处理成功。
    pub fn send_with(&self, value: T, cancelled: impl Fn() -> bool) -> Result<(), SendError<T>> {
        self.wait_send(value, None, Some(&cancelled))
    }
    /// 带超时的阻塞投递；超时未入队退回消息。
    /// 入参：self 为生产端或队列；value: T 为一条任务消息，所有权转移给队列。timeout: Duration 为容量等待预算，零值仍可在当前有空位时成功。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed；等待容量到期返回 Timeout。
    /// 注意事项：无时限版本可能一直等待容量；取消/超时不会关闭共享队列。取得锁或回调耗时不保证被等待预算硬性截断。入队成功仅表示任务已排队，不表示处理成功。
    pub fn send_timeout(&self, value: T, timeout: Duration) -> Result<(), SendError<T>> {
        self.wait_send(value, Some(timeout), None)
    }
    /// 同时带超时与取消条件的阻塞投递。
    /// 入参：self 为生产端或队列；value: T 为一条任务消息，所有权转移给队列。timeout: Duration 为容量等待预算，零值仍可在当前有空位时成功。cancelled: impl Fn() -> bool 为取消检查，返回 true 表示本次投递应停止。
    /// 返回值：成功返回 Ok(())；未投递消息通过 SendError 原样退回。
    /// 异常：队列关闭返回 Closed；取消检查为真返回 Cancelled；回调 panic 向外传播；等待容量到期返回 Timeout。
    /// 注意事项：无时限版本可能一直等待容量；取消/超时不会关闭共享队列。取得锁或回调耗时不保证被等待预算硬性截断。入队成功仅表示任务已排队，不表示处理成功。
    pub fn send_timeout_with(
        &self,
        value: T,
        timeout: Duration,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), SendError<T>> {
        self.wait_send(value, Some(timeout), Some(&cancelled))
    }
    /// 功能：统一驱动单条投递的容量等待，协调生产速度与消费者处理能力。
    /// 入参：self 为共享队列的生产端；value: T 为待投递单条消息；timeout: `Option<Duration>` 为等待预算，None 无限等待；cancelled: `Option<&dyn Fn()->bool>` 为可选取消检查。
    /// 返回值：Ok(()) 表示单条消息已入队。
    /// 异常：关闭、取消、容量等待到期分别返回 Closed/Cancelled/Timeout，失败均带回原消息。取消回调 panic 向外传播。
    /// 注意事项：循环每次在锁外查取消，再持状态锁检查业务条件；等待释放锁，醒来重新检查，不依赖一次唤醒即成功。有容量/消息时先尝试成功，再判断预算，故零预算也可能立即成功；等待预算不硬中断锁竞争和回调。
    fn wait_send(
        &self,
        mut value: T,
        timeout: Option<Duration>,
        cancelled: Option<&dyn Fn() -> bool>,
    ) -> Result<(), SendError<T>> {
        let started = Instant::now();
        loop {
            // 调用者的取消函数可能访问队列，必须在加锁之前调用。
            if cancelled.is_some_and(|check| check()) {
                return Err(SendError::Cancelled(value));
            }
            let mut state = self.0.lock();
            if state.closed {
                return Err(SendError::Closed(value));
            }
            match state.sender.try_send(value) {
                Ok(()) => {
                    state.len += 1;
                    drop(state);
                    self.0.readable.notify_one();
                    return Ok(());
                }
                // 满：拿回消息，进入等待。
                Err(mpsc::TrySendError::Full(returned)) => value = returned,
                // 接收端消失：视为关闭。
                Err(mpsc::TrySendError::Disconnected(returned)) => {
                    return Err(SendError::Closed(returned));
                }
            }
            let remaining = timeout.map(|timeout| timeout.saturating_sub(started.elapsed()));
            if remaining == Some(Duration::ZERO) {
                return Err(SendError::Timeout(value));
            }
            self.0
                .wait(&self.0.writable, state, remaining, cancelled.is_some());
        }
    }
}
impl<T> Receiver<T> {
    /// 容量上限（None 表示无界）。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：返回容量上限；有界包装返回 usize，通用端/接口以 Some 表示有界、None 表示无界。
    /// 异常：不返回业务错误。
    /// 注意事项：容量是消息条数，不是消息内业务条目数，也不是字节上限。
    pub fn capacity(&self) -> Option<usize> {
        self.0.capacity
    }
    /// 当前在队消息数。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：返回当前还在通道中的消息条数，不含已被消费者取走但尚未处理完成的任务。
    /// 异常：不返回业务错误。
    /// 注意事项：只是锁内读取的瞬时快照；并发投递或领取后立即可能改变。
    pub fn len(&self) -> usize {
        self.0.lock().len
    }
    /// 队列是否为空。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：当前在队消息数为零返回 true，否则 false。
    /// 异常：不返回业务错误。
    /// 注意事项：true 不等于业务任务全部完成，取走但未完成的消息不再计数。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// 队列是否已关闭。
    /// 入参：self 为当前队列或端句柄。
    /// 返回值：不再接受新消息时返回 true；仍可能存在尚未消费的消息。
    /// 异常：不返回业务错误。
    /// 注意事项：读取是瞬时快照；关闭与排空是两个不同阶段。
    pub fn is_closed(&self) -> bool {
        self.0.lock().closed
    }
    /// 关闭队列。
    /// 入参：self 为当前队列或任一端句柄。
    /// 返回值：无；同一共享队列所有句柄观察到关闭，并唤醒等待读写的线程。
    /// 异常：不返回业务错误。
    /// 注意事项：可重复调用；不会等消费者完成，也不会丢弃尚可领取的队内消息。
    pub fn close(&self) {
        self.0.close();
    }
    /// 非阻塞取出下一条消息。
    ///
    /// 功能：加锁后 try_recv；成功时同步 len 并唤醒等待容量的生产者。
    ///
    /// 错误：空且未关闭返回 Empty；空且已关闭返回 Closed。
    /// 入参：self 为消费端或队列。
    /// 注意事项：非阻塞只是不等待新消息，仍需取得互斥锁。成功领取后业务失败不会由队列自动重投。
    pub fn try_recv(&self) -> Result<T, RecvError> {
        let mut state = self.0.lock();
        match state
            .receiver
            .as_ref()
            .expect("存活的消费端必须持有底层通道")
            .try_recv()
        {
            Ok(value) => {
                // 出队成功：同步长度账本。
                state.len -= 1;
                drop(state);
                // 唤醒等待容量的生产者（腾出了空位）。
                self.0.writable.notify_all();
                Ok(value)
            }
            // 取空：已关闭 → Closed（无剩余消息）；否则 Empty。
            Err(_) if state.closed => Err(RecvError::Closed),
            Err(_) => Err(RecvError::Empty),
        }
    }
    /// 一次加锁取出有序前缀，`Vec[0]` 最先出队。max=0 是无操作。
    ///
    /// 功能：非阻塞批量取出最多 max 条（FIFO）；max=0 返回空批次。
    ///
    /// 错误：队列为空且未关闭返回 Empty；为空且已关闭返回 Closed。
    /// 入参：self 为消费端或队列。max: usize 为本次最多领取的消息条数，0 表示不领取。
    /// 注意事项：非阻塞只是不等待新消息，仍需取得互斥锁。max=0 先行成功，不检查取消或关闭。成功领取后业务失败不会由队列自动重投。
    pub fn try_recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError> {
        // 取零条：无操作成功。
        if max == 0 {
            return Ok(Vec::new());
        }
        let mut state = self.0.lock();
        // 队空：区分"只是暂时空"与"关闭后排空结束"。
        if state.len == 0 {
            return Err(if state.closed {
                RecvError::Closed
            } else {
                RecvError::Empty
            });
        }
        let values = state.dequeue_batch(max);
        drop(state);
        self.0.writable.notify_all();
        Ok(values)
    }
    /// 等待首项可用，然后在一次加锁内最多取出 max 项，不等待凑满批次。
    ///
    /// 功能：阻塞批量取出：等到至少一条消息后一次性取走最多 max 条
    /// （有首项即返回，不等待批次凑满）。
    /// 入参：self 为消费端或队列。max: usize 为本次最多领取的消息条数，0 表示不领取。
    /// 异常：队空且关闭返回 Closed。
    /// 注意事项：关闭后仍能排空已有消息；并发消费者竞争领取，不会广播。max=0 先行成功，不检查取消或关闭。成功领取后业务失败不会由队列自动重投。
    pub fn recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError> {
        self.wait_recv_batch(max, None, None)
    }
    /// 带取消条件的阻塞批量取出。
    /// 入参：self 为消费端或队列。max: usize 为本次最多领取的消息条数，0 表示不领取。cancelled: impl Fn() -> bool 返回 true 时中止本次领取。
    /// 返回值：成功返回按队内顺序领取的 `Vec<T>`，`Vec[0]` 最早；最多 max 条，不等待凑满批次；max=0 返回空集合。
    /// 异常：队空且关闭返回 Closed；取消检查为真返回 Cancelled，回调 panic 向外传播。
    /// 注意事项：关闭后仍能排空已有消息；并发消费者竞争领取，不会广播。取消检查先于领取，已取消时即使有消息也会中止。max=0 先行成功，不检查取消或关闭。成功领取后业务失败不会由队列自动重投。
    pub fn recv_batch_with(
        &self,
        max: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Vec<T>, RecvError> {
        self.wait_recv_batch(max, None, Some(&cancelled))
    }
    /// 带超时的阻塞批量取出。
    /// 入参：self 为消费端或队列。max: usize 为本次最多领取的消息条数，0 表示不领取。timeout: Duration 为等待首条消息的预算，零值仍可取走当前可用消息。
    /// 返回值：成功返回按队内顺序领取的 `Vec<T>`，`Vec[0]` 最早；最多 max 条，不等待凑满批次；max=0 返回空集合。
    /// 异常：队空且关闭返回 Closed；队空等待到期返回 Timeout。
    /// 注意事项：关闭后仍能排空已有消息；并发消费者竞争领取，不会广播。max=0 先行成功，不检查取消或关闭。成功领取后业务失败不会由队列自动重投。
    pub fn recv_batch_timeout(&self, max: usize, timeout: Duration) -> Result<Vec<T>, RecvError> {
        self.wait_recv_batch(max, Some(timeout), None)
    }
    /// 同时带超时与取消条件的阻塞批量取出。
    /// 入参：self 为消费端或队列。max: usize 为本次最多领取的消息条数，0 表示不领取。timeout: Duration 为等待首条消息的预算，零值仍可取走当前可用消息。cancelled: impl Fn() -> bool 返回 true 时中止本次领取。
    /// 返回值：成功返回按队内顺序领取的 `Vec<T>`，`Vec[0]` 最早；最多 max 条，不等待凑满批次；max=0 返回空集合。
    /// 异常：队空且关闭返回 Closed；取消检查为真返回 Cancelled，回调 panic 向外传播；队空等待到期返回 Timeout。
    /// 注意事项：关闭后仍能排空已有消息；并发消费者竞争领取，不会广播。取消检查先于领取，已取消时即使有消息也会中止。max=0 先行成功，不检查取消或关闭。成功领取后业务失败不会由队列自动重投。
    pub fn recv_batch_timeout_with(
        &self,
        max: usize,
        timeout: Duration,
        cancelled: impl Fn() -> bool,
    ) -> Result<Vec<T>, RecvError> {
        self.wait_recv_batch(max, Some(timeout), Some(&cancelled))
    }
    /// 功能：统一驱动整批领取的可读等待，让消费者在空队列时休眠。
    /// 入参：self 为共享队列的消费端；max: usize 为最多领取条数；timeout: `Option<Duration>` 为等待预算，None 无限等待；cancelled: `Option<&dyn Fn()->bool>` 为可选取消检查。
    /// 返回值：Ok(`Vec<T>`) 为本次可领取的有序前缀，最多 max 条，不等凑满。
    /// 异常：取消、队空且关闭、等待首项到期分别为 Cancelled/Closed/Timeout。取消回调 panic 向外传播。
    /// 注意事项：循环每次在锁外查取消，再持状态锁检查业务条件；等待释放锁，醒来重新检查，不依赖一次唤醒即成功。max=0 先返回空列表，不查取消或关闭。有容量/消息时先尝试成功，再判断预算，故零预算也可能立即成功；等待预算不硬中断锁竞争和回调。
    fn wait_recv_batch(
        &self,
        max: usize,
        timeout: Option<Duration>,
        cancelled: Option<&dyn Fn() -> bool>,
    ) -> Result<Vec<T>, RecvError> {
        if max == 0 {
            return Ok(Vec::new());
        }
        let started = Instant::now();
        loop {
            // 取消检查在加锁前执行（取消函数可能访问队列）。
            if cancelled.is_some_and(|check| check()) {
                return Err(RecvError::Cancelled);
            }
            let mut state = self.0.lock();
            // 有消息：一次锁内取走最多 max 条。
            if state.len > 0 {
                let values = state.dequeue_batch(max);
                drop(state);
                self.0.writable.notify_all();
                return Ok(values);
            }
            // 队空且已关闭：排空结束信号。
            if state.closed {
                return Err(RecvError::Closed);
            }
            let remaining = timeout.map(|timeout| timeout.saturating_sub(started.elapsed()));
            if remaining == Some(Duration::ZERO) {
                return Err(RecvError::Timeout);
            }
            // 在 readable 上等待新消息入队。
            self.0
                .wait(&self.0.readable, state, remaining, cancelled.is_some());
        }
    }
    /// 阻塞取出下一条消息（空时等待）。
    /// 入参：self 为消费端或队列。
    /// 返回值：成功返回消息 T，其所有权交给本消费者。
    /// 异常：队空且关闭返回 Closed。
    /// 注意事项：关闭后仍能排空已有消息；并发消费者竞争领取，不会广播。成功领取后业务失败不会由队列自动重投。
    pub fn recv(&self) -> Result<T, RecvError> {
        self.wait_recv(None, None)
    }
    /// 带取消条件的阻塞取出。
    /// 入参：self 为消费端或队列。cancelled: impl Fn() -> bool 返回 true 时中止本次领取。
    /// 返回值：成功返回消息 T，其所有权交给本消费者。
    /// 异常：队空且关闭返回 Closed；取消检查为真返回 Cancelled，回调 panic 向外传播。
    /// 注意事项：关闭后仍能排空已有消息；并发消费者竞争领取，不会广播。取消检查先于领取，已取消时即使有消息也会中止。成功领取后业务失败不会由队列自动重投。
    pub fn recv_with(&self, cancelled: impl Fn() -> bool) -> Result<T, RecvError> {
        self.wait_recv(None, Some(&cancelled))
    }
    /// 带超时的阻塞取出。
    /// 入参：self 为消费端或队列。timeout: Duration 为等待首条消息的预算，零值仍可取走当前可用消息。
    /// 返回值：成功返回消息 T，其所有权交给本消费者。
    /// 异常：队空且关闭返回 Closed；队空等待到期返回 Timeout。
    /// 注意事项：关闭后仍能排空已有消息；并发消费者竞争领取，不会广播。成功领取后业务失败不会由队列自动重投。
    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvError> {
        self.wait_recv(Some(timeout), None)
    }
    /// 同时带超时与取消条件的阻塞取出。
    /// 入参：self 为消费端或队列。timeout: Duration 为等待首条消息的预算，零值仍可取走当前可用消息。cancelled: impl Fn() -> bool 返回 true 时中止本次领取。
    /// 返回值：成功返回消息 T，其所有权交给本消费者。
    /// 异常：队空且关闭返回 Closed；取消检查为真返回 Cancelled，回调 panic 向外传播；队空等待到期返回 Timeout。
    /// 注意事项：关闭后仍能排空已有消息；并发消费者竞争领取，不会广播。取消检查先于领取，已取消时即使有消息也会中止。成功领取后业务失败不会由队列自动重投。
    pub fn recv_timeout_with(
        &self,
        timeout: Duration,
        cancelled: impl Fn() -> bool,
    ) -> Result<T, RecvError> {
        self.wait_recv(Some(timeout), Some(&cancelled))
    }
    /// 功能：统一驱动单条领取的可读等待，让消费者在空队列时休眠。
    /// 入参：self 为共享队列的消费端；timeout: `Option<Duration>` 为等待预算，None 无限等待；cancelled: `Option<&dyn Fn()->bool>` 为可选取消检查。
    /// 返回值：Ok(T) 为领取到的一条消息。
    /// 异常：取消、队空且关闭、等待首项到期分别为 Cancelled/Closed/Timeout。取消回调 panic 向外传播。
    /// 注意事项：循环每次在锁外查取消，再持状态锁检查业务条件；等待释放锁，醒来重新检查，不依赖一次唤醒即成功。有容量/消息时先尝试成功，再判断预算，故零预算也可能立即成功；等待预算不硬中断锁竞争和回调。
    fn wait_recv(
        &self,
        timeout: Option<Duration>,
        cancelled: Option<&dyn Fn() -> bool>,
    ) -> Result<T, RecvError> {
        let started = Instant::now();
        loop {
            if cancelled.is_some_and(|check| check()) {
                return Err(RecvError::Cancelled);
            }
            let mut state = self.0.lock();
            match state
                .receiver
                .as_ref()
                .expect("存活的消费端必须持有底层通道")
                .try_recv()
            {
                Ok(value) => {
                    state.len -= 1;
                    drop(state);
                    self.0.writable.notify_all();
                    return Ok(value);
                }
                // 取空且已关闭：排空结束。
                Err(_) if state.closed => return Err(RecvError::Closed),
                // 取空未关闭：进入等待。
                Err(_) => {}
            }
            let remaining = timeout.map(|timeout| timeout.saturating_sub(started.elapsed()));
            if remaining == Some(Duration::ZERO) {
                return Err(RecvError::Timeout);
            }
            self.0
                .wait(&self.0.readable, state, remaining, cancelled.is_some());
        }
    }
}

/// 队列核心行为的单元测试：接口一致性、FIFO、容量等待、关闭排空、
/// 批量原子性、取消与超时、多生产者/多消费者并发等。
#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::HashSet,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        thread,
    };

    /// 验证有界与无界队列都满足统一 Queue 接口的存取语义。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn queue_extension_interface_supports_both_current_implementations() {
        let queues: Vec<Box<dyn Queue<u8>>> = vec![
            Box::new(BoundedQueue::new(4)),
            Box::new(UnboundedQueue::new()),
        ];
        for (queue, capacity) in queues.into_iter().zip([Some(4), None]) {
            assert_eq!(queue.capacity(), capacity);
            queue.send(7).unwrap();
            queue.send_batch(vec![7, 2]).unwrap();
            queue.try_send(8).unwrap();
            assert_eq!(queue.len(), 4);
            let mut values = queue.try_recv_batch(2).unwrap();
            values.push(queue.recv().unwrap());
            values.extend(queue.recv_batch(2).unwrap());
            values.sort_unstable();
            assert_eq!(values, vec![2, 7, 7, 8]);
            queue.try_send_batch(vec![4, 5]).unwrap();
            assert!(queue.try_recv().is_ok());
            assert!(queue.try_recv().is_ok());
            assert!(queue.is_empty());
            queue.close();
            assert!(queue.is_closed());
            assert_eq!(queue.send(9), Err(SendError::Closed(9)));
            assert_eq!(queue.recv(), Err(RecvError::Closed));
        }
    }

    #[test]
    /// 验证无界队列能完整接收超过固定容量的批次。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn unbounded_queue_accepts_large_batches_without_a_capacity_limit() {
        let queue = UnboundedQueue::new();
        assert_eq!(queue.capacity(), None);
        queue.send_batch((0..20_000).collect()).unwrap();
        queue.send(7).unwrap();
        queue.try_send(7).unwrap();
        assert_eq!(queue.len(), 20_002);
        assert_eq!(
            queue.try_recv_batch(20_000).unwrap(),
            (0..20_000).collect::<Vec<_>>()
        );
        // 主动投递相同内容仍是两条消息，不会自动合并。
        assert_eq!(queue.recv_batch(10).unwrap(), vec![7, 7]);
        assert!(queue.is_empty());
        let (sender, receiver) = queue.split();
        assert_eq!(sender.capacity(), None);
        assert_eq!(receiver.capacity(), None);
        assert_eq!(sender.try_send_batch_partial(vec![9, 2, 5]), Ok(vec![]));
        assert_eq!(receiver.recv_batch(3).unwrap(), vec![9, 2, 5]);
        assert!(sender.send_timeout(1, Duration::ZERO).is_ok());
        assert!(
            sender
                .send_batch_timeout(vec![2, 3], Duration::ZERO)
                .is_ok()
        );
        assert_eq!(receiver.recv_batch(3).unwrap(), vec![1, 2, 3]);
        assert!(UnboundedQueue::<u8>::default().is_empty());
    }

    #[test]
    /// 验证阻塞消费者收到首条消息后立即返回，不会为了凑满批次继续等待。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn unbounded_consumers_wait_for_messages_and_do_not_wait_to_fill_a_batch() {
        let (sender, receiver) = UnboundedQueue::new().split();
        let (done, observed) = mpsc::channel();
        let worker = thread::spawn(move || done.send(receiver.recv_batch(10)).unwrap());
        assert!(observed.recv_timeout(Duration::from_millis(20)).is_err());
        sender.send_batch(vec![5, 8]).unwrap();
        assert_eq!(
            observed.recv_timeout(Duration::from_secs(1)).unwrap(),
            Ok(vec![5, 8])
        );
        worker.join().unwrap();
        assert!(sender.is_closed());
    }

    #[test]
    /// 验证无界队列仍遵守等待超时和调用方取消条件。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn unbounded_waits_preserve_timeout_and_cancellation_semantics() {
        let (sender, receiver) = UnboundedQueue::new().split();
        assert_eq!(receiver.try_recv(), Err(RecvError::Empty));
        assert_eq!(
            receiver.recv_timeout(Duration::from_millis(5)),
            Err(RecvError::Timeout)
        );
        assert_eq!(
            receiver.recv_batch_timeout(4, Duration::from_millis(5)),
            Err(RecvError::Timeout)
        );
        assert_eq!(sender.send_with(1, || true), Err(SendError::Cancelled(1)));
        assert_eq!(
            sender.send_batch_with(vec![1, 2], || true),
            Err(SendError::Cancelled(vec![1, 2]))
        );
        assert!(receiver.is_empty());
        sender.send_batch(vec![3, 4]).unwrap();
        assert_eq!(
            receiver.recv_batch_with(4, || true),
            Err(RecvError::Cancelled)
        );
        assert_eq!(receiver.len(), 2);
        assert_eq!(receiver.recv_batch(4).unwrap(), vec![3, 4]);
        let cancelled = Arc::new(AtomicBool::new(false));
        let token = Arc::clone(&cancelled);
        let consumer = receiver.clone();
        let (done, observed) = mpsc::channel();
        let worker = thread::spawn(move || {
            done.send(consumer.recv_with(|| token.load(Ordering::Acquire)))
                .unwrap()
        });
        assert!(observed.recv_timeout(Duration::from_millis(20)).is_err());
        cancelled.store(true, Ordering::Release);
        assert_eq!(
            observed.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(RecvError::Cancelled)
        );
        worker.join().unwrap();
        assert!(!sender.is_closed());
    }

    #[test]
    /// 验证关闭后剩余消息可排空，最后一个生产者释放时队列自动关闭。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn unbounded_close_drains_messages_and_last_producer_closes_the_queue() {
        let queue = UnboundedQueue::new();
        queue.send_batch(vec![1, 2, 3]).unwrap();
        queue.close();
        assert!(queue.is_closed());
        assert_eq!(queue.send(4), Err(SendError::Closed(4)));
        assert_eq!(
            queue.try_send_batch(vec![4, 5]),
            Err(SendError::Closed(vec![4, 5]))
        );
        assert_eq!(queue.recv_batch(2).unwrap(), vec![1, 2]);
        assert_eq!(queue.recv().unwrap(), 3);
        assert_eq!(queue.recv_batch(2), Err(RecvError::Closed));
        let (sender, receiver) = UnboundedQueue::new().split();
        let other = sender.clone();
        sender.send(7).unwrap();
        drop(sender);
        assert!(!receiver.is_closed());
        drop(other);
        assert!(receiver.is_closed());
        assert_eq!(receiver.recv(), Ok(7));
        assert_eq!(receiver.recv(), Err(RecvError::Closed));
        let (sender, receiver) = UnboundedQueue::<u8>::new().split();
        let worker = thread::spawn(move || receiver.recv());
        sender.close();
        assert_eq!(worker.join().unwrap(), Err(RecvError::Closed));
    }

    #[test]
    /// 验证最后一个消费者离开后，未读消息资源会被释放。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn unbounded_last_consumer_releases_unread_resources() {
        /// 析构计数测试消息；元组中的 Arc 指向所有消息共享的释放次数，
        /// 用于验证最后消费者离开或监听收尾时，未处理资源也被逐一释放。
        struct Resource(Arc<AtomicUsize>);
        impl Drop for Resource {
            /// 功能：在无界队列释放测试中确认每个未读资源最终被销毁。
            /// 入参：self 为持有共享原子计数器的测试资源，每个实例代表一条待释放消息。
            /// 返回值：无，计数器 Relaxed 加一。
            /// 异常：不执行外部回调或 I/O，没有业务错误返回。
            /// 注意事项：仅验证析构数量；测试同步后读取结果，不用此计数定义消息处理成功。
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let dropped = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = UnboundedQueue::new().split();
        assert!(
            sender
                .send_batch((0..1000).map(|_| Resource(Arc::clone(&dropped))).collect())
                .is_ok()
        );
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
        drop(receiver);
        assert_eq!(dropped.load(Ordering::Relaxed), 1000);
        assert_eq!(sender.len(), 0);
        assert!(sender.is_closed());
    }

    #[test]
    /// 验证多个生产者和消费者并发使用无界队列时，每条消息恰好被一个消费者取走。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn unbounded_multiple_producers_and_consumers_take_each_message_once() {
        let (sender, receiver) = UnboundedQueue::new().split();
        let start = Arc::new(std::sync::Barrier::new(8));
        let values = thread::scope(|scope| {
            for producer in 0..4 {
                let sender = sender.clone();
                let start = Arc::clone(&start);
                scope.spawn(move || {
                    start.wait();
                    for batch in 0..100 {
                        sender
                            .send_batch(
                                (0..5)
                                    .map(|offset| producer * 500 + batch * 5 + offset)
                                    .collect(),
                            )
                            .unwrap();
                    }
                });
            }
            drop(sender);
            let mut consumers = Vec::new();
            for _ in 0..4 {
                let receiver = receiver.clone();
                let start = Arc::clone(&start);
                consumers.push(scope.spawn(move || {
                    start.wait();
                    let mut values = Vec::new();
                    while let Ok(batch) = receiver.recv_batch(11) {
                        values.extend(batch);
                    }
                    values
                }));
            }
            drop(receiver);
            consumers
                .into_iter()
                .flat_map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(values.len(), 2000);
        assert_eq!(
            values.into_iter().collect::<HashSet<_>>(),
            (0..2000).collect()
        );
    }

    #[test]
    /// 验证有界队列满时普通发送会等待空位，不覆盖队列中已有消息。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn ordinary_queue_defaults_to_waiting_without_overwriting_messages() {
        let queue = Arc::new(BoundedQueue::new(2));
        queue.send_batch(vec![1, 2]).unwrap();
        let producer = Arc::clone(&queue);
        let (done, observed) = mpsc::channel();
        let worker = thread::spawn(move || {
            producer.send(3).unwrap();
            done.send(()).unwrap();
        });
        assert!(observed.recv_timeout(Duration::from_millis(20)).is_err());
        let mut values = vec![queue.recv().unwrap()];
        observed.recv_timeout(Duration::from_secs(1)).unwrap();
        queue.close();
        values.extend(queue.recv_batch(10).unwrap());
        values.sort_unstable();
        assert_eq!(values, vec![1, 2, 3]);
        assert_eq!(queue.recv(), Err(RecvError::Closed));
        worker.join().unwrap();
    }

    #[test]
    /// 验证释放一个空位可唤醒单条发送者，但不满足容量的整批仍继续等待。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn a_free_slot_can_wake_single_producers_while_larger_batches_still_wait() {
        let (sender, receiver) = BoundedQueue::new(2).split();
        sender.send_batch(vec![0, 1]).unwrap();
        let large = sender.clone();
        let (large_done, large_observed) = mpsc::channel();
        let large_worker = thread::spawn(move || {
            large.send_batch(vec![2, 3]).unwrap();
            large_done.send(()).unwrap();
        });
        let (small_done, small_observed) = mpsc::channel();
        let small_worker = thread::spawn(move || {
            sender.send(4).unwrap();
            small_done.send(()).unwrap();
        });
        assert!(
            small_observed
                .recv_timeout(Duration::from_millis(20))
                .is_err()
        );
        let mut values = vec![receiver.recv().unwrap()];
        small_observed.recv_timeout(Duration::from_secs(1)).unwrap();
        values.extend(receiver.recv_batch(2).unwrap());
        large_observed.recv_timeout(Duration::from_secs(1)).unwrap();
        values.extend(receiver.recv_batch(2).unwrap());
        values.sort_unstable();
        assert_eq!(values, vec![0, 1, 2, 3, 4]);
        large_worker.join().unwrap();
        small_worker.join().unwrap();
    }

    #[test]
    /// 验证整批消息按输入索引顺序入队，并保持先进先出。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn batch_index_zero_is_first_in_and_first_out() {
        let (sender, receiver) = BoundedQueue::new(8).split();
        sender.send(0).unwrap();
        sender.send_batch(vec![70, 10, 50]).unwrap();
        sender.send(90).unwrap();
        assert_eq!(receiver.try_recv_batch(3).unwrap(), vec![0, 70, 10]);
        assert_eq!(receiver.recv().unwrap(), 50);
        assert_eq!(
            receiver.recv_batch_timeout(8, Duration::ZERO).unwrap(),
            vec![90]
        );
        assert_eq!(receiver.try_recv_batch(8), Err(RecvError::Empty));
    }

    #[test]
    /// 验证部分入队返回未放入的有序后缀，之后可再次完整投递。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn partial_batch_returns_an_ordered_suffix() {
        let queue = BoundedQueue::new(4);
        queue.try_send(0).unwrap();
        let remaining = queue.try_send_batch_partial(vec![1, 2, 3, 4, 5]).unwrap();
        assert_eq!(remaining, vec![4, 5]);
        assert_eq!(queue.len(), 4);
        assert_eq!(
            queue.try_send_batch_partial(vec![6, 7]),
            Err(SendError::Full(vec![6, 7]))
        );
        assert_eq!(queue.try_recv_batch(10).unwrap(), vec![0, 1, 2, 3]);
        queue.try_send(9).unwrap();
        queue.try_send_batch(remaining).unwrap();
        assert_eq!(queue.try_recv_batch(10).unwrap(), vec![9, 4, 5]);
    }

    #[test]
    /// 验证整批失败时原批次全部退回，已在队列的消息不受影响。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn whole_batch_failure_preserves_every_item_and_existing_queue_contents() {
        let (sender, receiver) = BoundedQueue::new(2).split();
        sender.send(0).unwrap();
        assert_eq!(
            sender.try_send_batch(vec![1, 2]),
            Err(SendError::Full(vec![1, 2]))
        );
        assert_eq!(
            sender.send_batch(vec![1, 2, 3]),
            Err(SendError::TooLarge(vec![1, 2, 3]))
        );
        assert_eq!(
            sender.send_batch_timeout(vec![1, 2], Duration::from_millis(5)),
            Err(SendError::Timeout(vec![1, 2]))
        );
        assert_eq!(
            sender.send_batch_with(vec![1, 2], || true),
            Err(SendError::Cancelled(vec![1, 2]))
        );
        assert_eq!(receiver.len(), 1);
        assert_eq!(receiver.recv_batch(10).unwrap(), vec![0]);
    }

    #[test]
    /// 验证阻塞式整批发送会等待足够容量，而不是拆开批次逐条入队。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn whole_batch_waits_for_enough_capacity() {
        let (sender, receiver) = BoundedQueue::new(3).split();
        sender.send_batch(vec![0, 1]).unwrap();
        let (done, observed) = mpsc::channel();
        let producer = thread::spawn(move || {
            sender.send_batch(vec![2, 3]).unwrap();
            done.send(()).unwrap();
        });
        assert!(observed.recv_timeout(Duration::from_millis(20)).is_err());
        assert_eq!(receiver.recv().unwrap(), 0);
        observed.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(receiver.recv_batch(10).unwrap(), vec![1, 2, 3]);
        producer.join().unwrap();
    }

    #[test]
    /// 验证批量接收等待首条消息即可返回，不为凑满上限延迟消费。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn batch_receive_waits_for_first_item_without_waiting_to_fill_the_batch() {
        let (sender, receiver) = BoundedQueue::new(3).split();
        let worker = thread::spawn(move || receiver.recv_batch(10));
        sender.send_batch(vec![10, 11]).unwrap();
        assert_eq!(worker.join().unwrap().unwrap(), vec![10, 11]);
    }

    #[test]
    /// 验证批量等待同时遵循取消、关闭后排空和结束状态语义。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn batch_waits_support_cancellation_close_and_drain() {
        let (sender, receiver) = BoundedQueue::new(2).split();
        sender.send(0).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let token = Arc::clone(&cancelled);
        let producer = sender.clone();
        let (done, observed) = mpsc::channel();
        let worker = thread::spawn(move || {
            done.send(producer.send_batch_with(vec![1, 2], || token.load(Ordering::Acquire)))
                .unwrap()
        });
        assert!(observed.recv_timeout(Duration::from_millis(20)).is_err());
        cancelled.store(true, Ordering::Release);
        assert_eq!(
            observed.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(SendError::Cancelled(vec![1, 2]))
        );
        worker.join().unwrap();
        assert_eq!(
            receiver.recv_batch_with(2, || true),
            Err(RecvError::Cancelled)
        );
        sender.close();
        assert_eq!(
            sender.send_batch(vec![1, 2]),
            Err(SendError::Closed(vec![1, 2]))
        );
        assert_eq!(receiver.recv_batch(2).unwrap(), vec![0]);
        assert_eq!(receiver.recv_batch(2), Err(RecvError::Closed));
        let (sender, receiver) = BoundedQueue::<u8>::new(2).split();
        let token = Arc::new(AtomicBool::new(false));
        let check = Arc::clone(&token);
        let (done, observed) = mpsc::channel();
        let worker = thread::spawn(move || {
            done.send(receiver.recv_batch_with(2, || check.load(Ordering::Acquire)))
                .unwrap()
        });
        assert!(observed.recv_timeout(Duration::from_millis(20)).is_err());
        token.store(true, Ordering::Release);
        assert_eq!(
            observed.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(RecvError::Cancelled)
        );
        worker.join().unwrap();
        assert!(sender.is_closed());
    }

    #[test]
    /// 验证空批次与零条读取请求不改变队列状态。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn empty_batch_and_zero_receive_limit_are_no_ops() {
        let (sender, receiver) = BoundedQueue::<u8>::new(1).split();
        sender.close();
        assert_eq!(sender.send_batch_with(vec![], || true), Ok(()));
        assert_eq!(sender.try_send_batch_partial(vec![]), Ok(vec![]));
        assert_eq!(receiver.recv_batch_with(0, || true), Ok(vec![]));
        assert_eq!(receiver.try_recv_batch(0), Ok(vec![]));
    }

    #[test]
    /// 验证多个生产者并发提交时，一个原子批次内部不会与其他批次交错。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn concurrent_producers_cannot_interleave_inside_an_atomic_batch() {
        let (sender, receiver) = BoundedQueue::new(12).split();
        let values = thread::scope(|scope| {
            for producer in 0..4 {
                let sender = sender.clone();
                scope.spawn(move || {
                    for batch in 0..100 {
                        sender
                            .send_batch((0..3).map(|index| (producer, batch, index)).collect())
                            .unwrap();
                    }
                });
            }
            let singles = sender.clone();
            scope.spawn(move || {
                for batch in 0..100 {
                    singles.send((4, batch, 0)).unwrap();
                }
            });
            drop(sender);
            let mut all = Vec::new();
            while let Ok(batch) = receiver.recv_batch(5) {
                all.extend(batch);
            }
            all
        });
        let mut index = 0;
        while index < values.len() {
            let (producer, batch, offset) = values[index];
            assert_eq!(offset, 0);
            if producer == 4 {
                index += 1;
            } else {
                assert_eq!(
                    &values[index..index + 3],
                    &[
                        (producer, batch, 0),
                        (producer, batch, 1),
                        (producer, batch, 2)
                    ]
                );
                index += 3;
            }
        }
        assert_eq!(values.len(), 1300);
        assert_eq!(values.into_iter().collect::<HashSet<_>>().len(), 1300);
    }

    #[test]
    /// 验证多个消费者取到互不重叠的 FIFO 前缀，不会重复消费或乱序。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn concurrent_batch_consumers_return_ordered_disjoint_prefixes() {
        let (sender, receiver) = BoundedQueue::new(20).split();
        let batches = thread::scope(|scope| {
            scope.spawn(move || {
                for start in (0..2000).step_by(4) {
                    sender.send_batch((start..start + 4).collect()).unwrap();
                }
            });
            let mut consumers = Vec::new();
            for _ in 0..4 {
                let receiver = receiver.clone();
                consumers.push(scope.spawn(move || {
                    let mut batches = Vec::new();
                    while let Ok(batch) = receiver.recv_batch(7) {
                        batches.push(batch);
                    }
                    batches
                }));
            }
            drop(receiver);
            consumers
                .into_iter()
                .flat_map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        for batch in &batches {
            assert!(batch.windows(2).all(|pair| pair[0] + 1 == pair[1]));
        }
        let values = batches.into_iter().flatten().collect::<Vec<_>>();
        assert_eq!(values.len(), 2000);
        assert_eq!(
            values.into_iter().collect::<HashSet<_>>(),
            (0..2000).collect()
        );
    }

    #[test]
    /// 验证容量、FIFO 和零超时失败都保留消息所有权，不丢任务。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn capacity_fifo_and_zero_timeout_preserve_ownership() {
        let queue = BoundedQueue::new(2);
        assert_eq!(queue.capacity(), 2);
        let (sender, receiver) = queue.split();
        sender.send("first".to_owned()).unwrap();
        sender.send("second".to_owned()).unwrap();
        assert_eq!(sender.len(), 2);
        assert_eq!(
            sender.try_send("third".into()),
            Err(SendError::Full("third".into()))
        );
        assert_eq!(
            sender.send_timeout("timed".into(), Duration::ZERO),
            Err(SendError::Timeout("timed".into()))
        );
        assert_eq!(receiver.recv().unwrap(), "first");
        assert_eq!(receiver.recv().unwrap(), "second");
        assert!(receiver.is_empty());
        assert_eq!(receiver.try_recv(), Err(RecvError::Empty));
        assert_eq!(
            receiver.recv_timeout(Duration::ZERO),
            Err(RecvError::Timeout)
        );
    }

    #[test]
    /// 验证正等待时限到达后解除阻塞，未成功投递的消息仍交还调用方。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn positive_timeouts_release_waiters_without_losing_tasks() {
        let (sender, receiver) = BoundedQueue::new(1).split();
        sender.send(1).unwrap();
        assert_eq!(
            sender.send_timeout_with(2, Duration::from_millis(5), || sender.is_closed()),
            Err(SendError::Timeout(2))
        );
        assert_eq!(receiver.recv().unwrap(), 1);
        assert_eq!(
            receiver.recv_timeout_with(Duration::from_millis(5), || receiver.is_closed()),
            Err(RecvError::Timeout)
        );
        sender.send(3).unwrap();
        assert_eq!(receiver.recv().unwrap(), 3);
    }

    #[test]
    /// 验证队列满时生产者等待，消费者取走消息后生产者继续提交。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn full_queue_waits_until_a_consumer_releases_capacity() {
        let (sender, receiver) = BoundedQueue::new(1).split();
        sender.send(1).unwrap();
        let (done, observed) = mpsc::channel();
        let producer = thread::spawn(move || {
            sender.send(2).unwrap();
            done.send(()).unwrap();
        });
        assert!(observed.recv_timeout(Duration::from_millis(20)).is_err());
        assert_eq!(receiver.recv().unwrap(), 1);
        observed.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(receiver.recv().unwrap(), 2);
        assert_eq!(receiver.recv(), Err(RecvError::Closed));
        producer.join().unwrap();
    }

    #[test]
    /// 验证取消只结束本次等待，不关闭队列也不丢弃已有消息。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn cancelling_waits_does_not_close_the_queue_or_discard_pending_items() {
        let (sender, receiver) = BoundedQueue::new(1).split();
        sender.send(1).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let token = Arc::clone(&cancelled);
        let producer = sender.clone();
        let (done, observed) = mpsc::channel();
        let worker = thread::spawn(move || {
            done.send(producer.send_with(2, || token.load(Ordering::Acquire)))
                .unwrap()
        });
        assert!(observed.recv_timeout(Duration::from_millis(20)).is_err());
        cancelled.store(true, Ordering::Release);
        assert_eq!(
            observed.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(SendError::Cancelled(2))
        );
        worker.join().unwrap();
        assert_eq!(receiver.recv().unwrap(), 1);
        assert!(!sender.is_closed());
        let consumer = receiver.clone();
        let token = Arc::new(AtomicBool::new(false));
        let check = Arc::clone(&token);
        let (done, observed) = mpsc::channel();
        let worker = thread::spawn(move || {
            done.send(consumer.recv_with(|| check.load(Ordering::Acquire)))
                .unwrap()
        });
        assert!(observed.recv_timeout(Duration::from_millis(20)).is_err());
        token.store(true, Ordering::Release);
        assert_eq!(
            observed.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(RecvError::Cancelled)
        );
        worker.join().unwrap();
        sender.send(3).unwrap();
        assert_eq!(receiver.recv().unwrap(), 3);
    }

    #[test]
    /// 验证显式关闭会唤醒等待中的生产者，并允许消费者先取完余量。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn explicit_close_wakes_producers_and_drains_before_reporting_closed() {
        let (sender, receiver) = BoundedQueue::new(1).split();
        sender.send(1).unwrap();
        let producer = sender.clone();
        let (done, observed) = mpsc::channel();
        let worker = thread::spawn(move || done.send(producer.send(2)).unwrap());
        receiver.close();
        assert_eq!(
            observed.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(SendError::Closed(2))
        );
        assert!(sender.is_closed());
        assert_eq!(receiver.recv().unwrap(), 1);
        assert_eq!(receiver.recv(), Err(RecvError::Closed));
        worker.join().unwrap();
        let (sender, receiver) = BoundedQueue::<u8>::new(1).split();
        let worker = thread::spawn(move || receiver.recv());
        sender.close();
        assert_eq!(worker.join().unwrap(), Err(RecvError::Closed));
    }

    #[test]
    /// 验证最后一个消费者离开时，未读任务在队列锁外析构，避免回调访问队列造成死锁。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn last_receiver_releases_tasks_outside_the_queue_lock() {
        /// 队列锁重入测试消息：元组保存一次性析构回调，析构后取走并执行。
        /// 回调再次查询队列，以确认消息是在状态锁外释放，避免死锁。
        struct Task(Option<Box<dyn FnOnce() + Send>>);
        impl Drop for Task {
            /// 功能：执行测试消息携带的析构回调，验证队列丢弃未读消息时已释放内部状态锁。
            /// 入参：self 为包裹 `Option<Box<dyn FnOnce() + Send>>` 的测试消息，具体回调由用例设置。
            /// 返回值：无；take 取出回调并最多执行一次，已为空时无操作。
            /// 异常：回调的 panic 原样传播，队列测试不在此吞掉断言失败。
            /// 注意事项：回调会再次访问队列以探测锁位置，因此生产代码必须在锁外释放消息资源。
            fn drop(&mut self) {
                if let Some(action) = self.0.take() {
                    action();
                }
            }
        }
        let (sender, receiver) = BoundedQueue::new(1).split();
        let observer = sender.clone();
        let dropped = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&dropped);
        assert!(
            sender
                .send(Task(Some(Box::new(move || {
                    assert!(observer.is_closed());
                    assert_eq!(observer.len(), 0);
                    count.fetch_add(1, Ordering::Relaxed);
                }))))
                .is_ok()
        );
        drop(receiver);
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        assert!(sender.is_closed());
        assert!(matches!(
            sender.try_send(Task(None)),
            Err(SendError::Closed(_))
        ));
    }

    #[test]
    /// 验证克隆生产端与消费端共同工作时每条任务仍只交付一次。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn cloned_producers_and_consumers_deliver_each_task_once() {
        let (sender, receiver) = BoundedQueue::new(8).split();
        let all = thread::scope(|scope| {
            for producer in 0..4 {
                let sender = sender.clone();
                scope.spawn(move || {
                    for index in 0..500 {
                        sender.send(producer * 500 + index).unwrap();
                    }
                });
            }
            drop(sender);
            let mut consumers = Vec::new();
            for _ in 0..4 {
                let receiver = receiver.clone();
                consumers.push(scope.spawn(move || {
                    let mut values = Vec::new();
                    while let Ok(value) = receiver.recv() {
                        values.push(value);
                    }
                    values
                }));
            }
            drop(receiver);
            consumers
                .into_iter()
                .flat_map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(all.len(), 2000);
        assert_eq!(all.into_iter().collect::<HashSet<_>>(), (0..2000).collect());
    }

    #[test]
    /// 验证取消回调可以查询队列状态而不会因内部持锁产生自锁。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn cancellation_callbacks_can_inspect_the_queue() {
        let (sender, receiver) = BoundedQueue::new(1).split();
        assert!(sender.send_with(1, || sender.is_closed()).is_ok());
        assert_eq!(receiver.recv_with(|| receiver.is_closed()), Ok(1));
    }
}

/// 监听推送（listen_batches）的单元测试：数量阈值、时间阈值、关闭尾批、
/// 取消不调消费回调、多监听器每条消息只投一次。
#[cfg(test)]
mod listener_tests {
    use super::*;
    use std::{
        cell::Cell,
        sync::atomic::{AtomicUsize, Ordering},
    };
    /// 功能：为攒批监听测试提供可控制的数量和时间阈值。
    /// 入参：count: usize 为累计业务权重阈值；interval: Duration 为首项进入尾批后的等待阈值。
    /// 返回值：BatchPolicy，原样保存两个参数。
    /// 异常：构造不返回错误；count=0 会在真正调用 listen_batches 时断言失败。
    /// 注意事项：仅组装测试数据，不开始计时或监听，不约束回调执行和线程调度的实际延迟。
    fn policy(count: usize, interval: Duration) -> BatchPolicy {
        BatchPolicy { count, interval }
    }
    /// 验证数量阈值触发推送、回调可在锁外反向入队。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn queue_pushes_count_batch_across_capacity_and_callback_can_enqueue() {
        let (sender, receiver) = BoundedQueue::new(2).split();
        let delivered = Cell::new(false);
        std::thread::scope(|scope| {
            let producer = sender.clone();
            scope.spawn(move || {
                for value in 0..4 {
                    producer.send(value).unwrap();
                }
            });
            receiver
                .listen_batches(
                    policy(4, Duration::from_secs(5)),
                    |_| 1,
                    |state| {
                        let mut control = ListenControl::new(4, state.closed);
                        control.finish = delivered.get();
                        Ok::<_, ()>(control)
                    },
                    |batch| {
                        assert_eq!(batch, vec![0, 1, 2, 3]);
                        sender.send(99).unwrap();
                        delivered.set(true);
                        Ok(())
                    },
                )
                .unwrap();
        });
        assert_eq!(receiver.try_recv().unwrap(), 99);
    }
    /// 验证生产者仍存活时按时间阈值推送尾批。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn queue_pushes_on_time_while_producer_remains_open() {
        let (sender, receiver) = UnboundedQueue::new().split();
        sender.send(1).unwrap();
        let delivered = Cell::new(false);
        let start = Instant::now();
        receiver
            .listen_batches(
                policy(8, Duration::from_millis(20)),
                |_| 1,
                |state| {
                    assert!(!state.closed);
                    let mut control = ListenControl::new(8, false);
                    control.finish = delivered.get();
                    Ok::<_, ()>(control)
                },
                |batch| {
                    assert!(start.elapsed() >= Duration::from_millis(20));
                    assert_eq!(batch, vec![1]);
                    delivered.set(true);
                    Ok(())
                },
            )
            .unwrap();
    }
    /// 验证队列关闭时立即推送尾批，不等时间阈值。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn queue_closure_pushes_tail_without_waiting_for_time_threshold() {
        let (sender, receiver) = UnboundedQueue::new().split();
        sender.send_batch(vec![1, 2]).unwrap();
        drop(sender);
        let mut batches = Vec::new();
        receiver
            .listen_batches(
                policy(8, Duration::from_secs(60)),
                |_| 1,
                |state| Ok::<_, ()>(ListenControl::new(8, state.closed)),
                |batch| {
                    batches.push(batch);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(batches, vec![vec![1, 2]]);
    }
    /// 验证监听被取消时，已取走但未推送的消息随尾批析构释放，
    /// 且不再调用消费回调。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn cancelling_listener_drops_pending_resources_without_calling_consumer() {
        /// 析构计数测试消息；元组中的 Arc 指向所有消息共享的释放次数，
        /// 用于验证最后消费者离开或监听收尾时，未处理资源也被逐一释放。
        struct Resource(Arc<AtomicUsize>);
        impl Drop for Resource {
            /// 功能：在监听失败/取消测试中统计未交付尾批消息是否已被释放。
            /// 入参：self 为持有共享原子计数器的测试消息，一份实例对应一个待释放资源。
            /// 返回值：无；计数器用 SeqCst 加一。
            /// 异常：不返回业务错误或执行磁盘操作。
            /// 注意事项：计数用于验证资源所有权清理，并不意味着该消息已经被业务消费者成功处理。
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = UnboundedQueue::new().split();
        sender
            .send(Resource(dropped.clone()))
            .unwrap_or_else(|_| panic!("入队失败"));
        let result = receiver.listen_batches(
            policy(8, Duration::from_secs(60)),
            |_| 1,
            |state| {
                if state.pending_weight > 0 {
                    Err("cancelled")
                } else {
                    Ok(ListenControl::new(8, false))
                }
            },
            |_| panic!("取消后不能调用消费回调"),
        );
        assert_eq!(result, Err("cancelled"));
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }
    /// 验证多个监听器共享同一队列时，每条消息只投递给其中一个监听器。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn multiple_listeners_deliver_each_message_once() {
        let (sender, receiver) = BoundedQueue::new(2).split();
        let output = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let receiver = receiver.clone();
                let output = &output;
                scope.spawn(move || {
                    receiver
                        .listen_batches(
                            policy(3, Duration::from_millis(20)),
                            |_| 1,
                            |state| Ok::<_, ()>(ListenControl::new(3, state.closed)),
                            |batch| {
                                output.lock().unwrap().extend(batch);
                                Ok(())
                            },
                        )
                        .unwrap()
                });
            }
            for value in 0..21 {
                sender.send(value).unwrap();
            }
            drop(sender);
        });
        let mut output = output.into_inner().unwrap();
        output.sort();
        assert_eq!(output, (0..21).collect::<Vec<_>>());
    }
}
