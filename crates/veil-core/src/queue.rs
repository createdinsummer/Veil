//! 通用队列，提供普通有界队列 BoundedQueue 和普通无界队列 UnboundedQueue。
//!
//! 两种队列都支持多生产者、多消费者，以及单条操作、批量操作。
//! 不覆盖已有消息，每次入队的消息只能被一个消费者取走一次，不支持消息去重。
//! 不保证多个生产者并发投递时的入队先后，也不要求多个消费者按顺序处理。
//! 支持监听推送：由队列内部按数量或时间阈值调用注册的消费回调。
//! 内部操作通过同一把锁串行执行；关闭后仍可取出剩余消息，取消只影响本次调用。
use std::{
    fmt,
    sync::{Arc, Condvar, Mutex, MutexGuard, mpsc},
    time::{Duration, Instant},
};

// 取消条件由调用者提供，定期检查，避免要求取消方持有队列句柄。
const CANCEL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, PartialEq, Eq)]
pub enum SendError<T> {
    Full(T),
    TooLarge(T),
    Closed(T),
    Cancelled(T),
    Timeout(T),
}
impl<T> SendError<T> {
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
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvError {
    Empty,
    Closed,
    Cancelled,
    Timeout,
}
impl fmt::Display for RecvError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
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
#[derive(Clone, Copy, Debug)]
pub struct BatchPolicy {
    pub count: usize,
    pub interval: Duration,
}
/// 队列提供给监听器的调度状态。
#[derive(Clone, Copy, Debug)]
pub struct ListenState {
    pub closed: bool,
    pub pending_weight: usize,
}
/// 监听器提供背压和结束条件，不负责取消息或判断定时阈值。
pub struct ListenControl {
    /// 本监听器的可用批次权重，0 表示暂停接收和推送。
    pub capacity: usize,
    /// 每轮最多取出的消息数；一条消息可以包含多个业务条目。
    pub max_items: usize,
    /// 提前推送当前尾批。
    pub flush: bool,
    /// 尾批推送后结束监听。
    pub finish: bool,
}
impl ListenControl {
    pub fn new(capacity: usize, closed: bool) -> Self {
        Self {
            capacity,
            max_items: capacity,
            flush: closed,
            finish: closed,
        }
    }
}

impl<T> Receiver<T> {
    /// 注册回调并在当前调度线程持续监听，直到关闭、取消或回调失败。
    /// 消息到达时由入队唤醒；队列内部累计批次，任一阈值达到后调用 on_batch。
    /// 关闭时处理尾批；on_tick 用于业务背压、取消和结束判断，空闲等待时最多间隔 10ms 检查一次。
    /// 所有回调均在队列互斥锁之外执行；每个监听器串行执行自己的回调。
    /// 多个监听器共享同一队列时，消息仅投递给其中一个监听器。
    /// count 和每条消息的 weight 必须大于零；失败后不自动重投已取走的消息。
    pub fn listen_batches<E>(
        &self,
        policy: BatchPolicy,
        weight: impl Fn(&T) -> usize,
        mut on_tick: impl FnMut(ListenState) -> Result<ListenControl, E>,
        mut on_batch: impl FnMut(Vec<T>) -> Result<(), E>,
    ) -> Result<(), E> {
        assert!(policy.count > 0, "批量数量阈值必须大于零");
        let mut pending = Vec::new();
        let mut pending_weight = 0usize;
        let mut deadline = None;
        let mut closed = false;
        loop {
            let control = on_tick(ListenState {
                closed,
                pending_weight,
            })?;
            if control.finish && pending.is_empty() {
                return Ok(());
            }
            let limit = policy.count.min(control.capacity);
            if control.capacity > 0
                && !pending.is_empty()
                && (pending_weight >= limit
                    || control.flush
                    || control.finish
                    || deadline.is_some_and(|at| Instant::now() >= at))
            {
                on_batch(std::mem::take(&mut pending))?;
                pending_weight = 0;
                deadline = None;
                continue;
            }
            let wait = deadline
                .map(|at| {
                    at.saturating_duration_since(Instant::now())
                        .min(CANCEL_INTERVAL)
                })
                .unwrap_or(CANCEL_INTERVAL);
            if closed || control.capacity == 0 || pending_weight >= limit || control.max_items == 0
            {
                std::thread::sleep(if wait.is_zero() {
                    CANCEL_INTERVAL
                } else {
                    wait
                });
                continue;
            }
            match self.recv_batch_timeout((limit - pending_weight).min(control.max_items), wait) {
                Ok(items) => {
                    if pending.is_empty() {
                        deadline = Some(Instant::now() + policy.interval);
                    }
                    for item in items {
                        let size = weight(&item);
                        assert!(size > 0, "消息权重必须大于零");
                        pending_weight = pending_weight.saturating_add(size);
                        pending.push(item);
                    }
                }
                Err(RecvError::Closed) => closed = true,
                Err(RecvError::Timeout | RecvError::Empty) => {}
                Err(RecvError::Cancelled) => unreachable!("未提供取消条件"),
            }
        }
    }
}

enum ChannelSender<T> {
    Bounded(mpsc::SyncSender<T>),
    Unbounded(mpsc::Sender<T>),
}
impl<T> ChannelSender<T> {
    fn try_send(&self, value: T) -> Result<(), mpsc::TrySendError<T>> {
        match self {
            Self::Bounded(sender) => sender.try_send(value),
            Self::Unbounded(sender) => sender
                .send(value)
                .map_err(|error| mpsc::TrySendError::Disconnected(error.0)),
        }
    }
}

struct State<T> {
    sender: ChannelSender<T>,
    receiver: Option<mpsc::Receiver<T>>,
    len: usize,
    closed: bool,
    senders: usize,
    receivers: usize,
}
struct Shared<T> {
    state: Mutex<State<T>>,
    readable: Condvar,
    writable: Condvar,
    capacity: Option<usize>,
}
impl<T> State<T> {
    // 调用方持有同一把锁，并提前确认容量；外部无法访问底层通道。
    fn enqueue_batch(&mut self, values: impl IntoIterator<Item = T>) {
        for value in values {
            if self.sender.try_send(value).is_err() {
                unreachable!("队列容量与底层通道不一致");
            }
            self.len += 1;
        }
    }
    fn dequeue_batch(&mut self, max: usize) -> Vec<T> {
        let count = self.len.min(max);
        let mut values = Vec::with_capacity(count);
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
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
    fn wait(
        &self,
        condition: &Condvar,
        state: MutexGuard<'_, State<T>>,
        remaining: Option<Duration>,
        cancellable: bool,
    ) {
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
    fn close(&self) {
        self.lock().closed = true;
        self.readable.notify_all();
        self.writable.notify_all();
    }
}

/// 队列扩展接口，只约定消息存取与生命周期，不限定底层存储或调度实现。
/// 当前仅提供普通有界队列、普通无界队列；后续类型可独立实现本接口。
/// 入队成功不代表处理成功，不覆盖消息，也不按消息内容去重。
pub trait Queue<T>: Send + Sync {
    /// None 表示无容量上限。
    fn capacity(&self) -> Option<usize>;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn is_closed(&self) -> bool;
    fn close(&self);
    fn send(&self, value: T) -> Result<(), SendError<T>>;
    fn send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>>;
    fn recv(&self) -> Result<T, RecvError>;
    fn recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError>;
    fn try_send(&self, value: T) -> Result<(), SendError<T>>;
    fn try_send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>>;
    fn try_recv(&self) -> Result<T, RecvError>;
    fn try_recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError>;
}

/// 普通有界队列。
///
/// 1. 默认在队列满时等待可用容量，在队列空时等待消息。
/// 2. 支持多个生产者、多个消费者，以及批量入队、批量取出。
/// 3. 不覆盖已有消息，每次入队的消息只能被一个消费者取走一次，不支持消息去重。
/// 4. 不保证多个生产者并发投递时的入队先后，也不要求多个消费者按顺序处理。
pub struct BoundedQueue<T> {
    sender: Sender<T>,
    receiver: Receiver<T>,
}

/// 普通无界队列。
///
/// 1. 不设置容量上限，入队不因容量而等待，在队列空时等待消息。
/// 2. 支持多个生产者、多个消费者，以及批量入队、批量取出。
/// 3. 不覆盖已有消息，每次入队的消息只能被一个消费者取走一次，不支持消息去重。
/// 4. 不保证多个生产者并发投递时的入队先后，也不要求多个消费者按顺序处理。
pub struct UnboundedQueue<T> {
    sender: Sender<T>,
    receiver: Receiver<T>,
}

/// 可克隆的生产端；capacity 返回 None 表示没有容量上限。
pub struct Sender<T>(Arc<Shared<T>>);
/// 可克隆的消费端；capacity 返回 None 表示没有容量上限。
pub struct Receiver<T>(Arc<Shared<T>>);

fn channel<T>(capacity: Option<usize>) -> (Sender<T>, Receiver<T>) {
    let (sender, receiver) = match capacity {
        Some(capacity) => {
            let (sender, receiver) = mpsc::sync_channel(capacity);
            (ChannelSender::Bounded(sender), receiver)
        }
        None => {
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
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "队列容量必须大于零");
        let (sender, receiver) = channel(Some(capacity));
        Self { sender, receiver }
    }
    /// 默认等待可用容量，不丢弃、不覆盖已有消息。
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        self.sender.send(value)
    }
    /// 等待足够容量后投递整批消息；批次大小不能超过固定容量。
    pub fn send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        self.sender.send_batch(values)
    }
    pub fn recv(&self) -> Result<T, RecvError> {
        self.receiver.recv()
    }
    pub fn recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError> {
        self.receiver.recv_batch(max)
    }
    pub fn try_send(&self, value: T) -> Result<(), SendError<T>> {
        self.sender.try_send(value)
    }
    pub fn try_recv(&self) -> Result<T, RecvError> {
        self.receiver.try_recv()
    }
    /// 整批放入，Vec[0] 最先入队；容量不足时整批退回。
    pub fn try_send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        self.sender.try_send_batch(values)
    }
    /// 只放入能容纳的前缀，按原顺序返回尚未入队的后缀。
    pub fn try_send_batch_partial(&self, values: Vec<T>) -> Result<Vec<T>, SendError<Vec<T>>> {
        self.sender.try_send_batch_partial(values)
    }
    /// 按 FIFO 返回最多 max 项，Vec[0] 最先出队。
    pub fn try_recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError> {
        self.receiver.try_recv_batch(max)
    }
    pub fn split(self) -> (Sender<T>, Receiver<T>) {
        (self.sender, self.receiver)
    }
    pub fn capacity(&self) -> usize {
        self.sender.capacity().expect("有界队列必须设置容量")
    }
    pub fn len(&self) -> usize {
        self.sender.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
    pub fn close(&self) {
        self.sender.close();
    }
}
impl<T> Default for UnboundedQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}
impl<T> UnboundedQueue<T> {
    pub fn new() -> Self {
        let (sender, receiver) = channel(None);
        Self { sender, receiver }
    }
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        self.sender.send(value)
    }
    pub fn send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        self.sender.send_batch(values)
    }
    pub fn recv(&self) -> Result<T, RecvError> {
        self.receiver.recv()
    }
    pub fn recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError> {
        self.receiver.recv_batch(max)
    }
    pub fn try_send(&self, value: T) -> Result<(), SendError<T>> {
        self.sender.try_send(value)
    }
    pub fn try_recv(&self) -> Result<T, RecvError> {
        self.receiver.try_recv()
    }
    pub fn try_send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        self.sender.try_send_batch(values)
    }
    /// 无界队列可放入整个批次，成功时返回空 Vec。
    pub fn try_send_batch_partial(&self, values: Vec<T>) -> Result<Vec<T>, SendError<Vec<T>>> {
        self.sender.try_send_batch_partial(values)
    }
    pub fn try_recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError> {
        self.receiver.try_recv_batch(max)
    }
    pub fn split(self) -> (Sender<T>, Receiver<T>) {
        (self.sender, self.receiver)
    }
    pub fn capacity(&self) -> Option<usize> {
        None
    }
    pub fn len(&self) -> usize {
        self.sender.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
    pub fn close(&self) {
        self.sender.close();
    }
}

impl<T: Send> Queue<T> for BoundedQueue<T> {
    fn capacity(&self) -> Option<usize> {
        self.sender.capacity()
    }
    fn len(&self) -> usize {
        self.sender.len()
    }
    fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
    fn close(&self) {
        self.sender.close();
    }
    fn send(&self, value: T) -> Result<(), SendError<T>> {
        self.sender.send(value)
    }
    fn send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        self.sender.send_batch(values)
    }
    fn recv(&self) -> Result<T, RecvError> {
        self.receiver.recv()
    }
    fn recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError> {
        self.receiver.recv_batch(max)
    }
    fn try_send(&self, value: T) -> Result<(), SendError<T>> {
        self.sender.try_send(value)
    }
    fn try_send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        self.sender.try_send_batch(values)
    }
    fn try_recv(&self) -> Result<T, RecvError> {
        self.receiver.try_recv()
    }
    fn try_recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError> {
        self.receiver.try_recv_batch(max)
    }
}

impl<T: Send> Queue<T> for UnboundedQueue<T> {
    fn capacity(&self) -> Option<usize> {
        self.sender.capacity()
    }
    fn len(&self) -> usize {
        self.sender.len()
    }
    fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
    fn close(&self) {
        self.sender.close();
    }
    fn send(&self, value: T) -> Result<(), SendError<T>> {
        self.sender.send(value)
    }
    fn send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        self.sender.send_batch(values)
    }
    fn recv(&self) -> Result<T, RecvError> {
        self.receiver.recv()
    }
    fn recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError> {
        self.receiver.recv_batch(max)
    }
    fn try_send(&self, value: T) -> Result<(), SendError<T>> {
        self.sender.try_send(value)
    }
    fn try_send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        self.sender.try_send_batch(values)
    }
    fn try_recv(&self) -> Result<T, RecvError> {
        self.receiver.try_recv()
    }
    fn try_recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError> {
        self.receiver.try_recv_batch(max)
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.0.lock().senders += 1;
        Self(Arc::clone(&self.0))
    }
}
impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        self.0.lock().receivers += 1;
        Self(Arc::clone(&self.0))
    }
}
impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.senders -= 1;
        if state.senders == 0 {
            state.closed = true;
        }
        drop(state);
        self.0.readable.notify_all();
    }
}
impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.receivers -= 1;
        let abandoned = if state.receivers == 0 {
            state.closed = true;
            state.len = 0;
            state.receiver.take()
        } else {
            None
        };
        drop(state);
        self.0.writable.notify_all();
        // 在锁外直接释放底层接收端及其消息，避免为清空无界队列另建同样大的集合。
        // 任务的析构可能访问队列，不能在内部互斥锁中执行。
        drop(abandoned);
    }
}

impl<T> Sender<T> {
    pub fn capacity(&self) -> Option<usize> {
        self.0.capacity
    }
    pub fn len(&self) -> usize {
        self.0.lock().len
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn is_closed(&self) -> bool {
        self.0.lock().closed
    }
    pub fn close(&self) {
        self.0.close();
    }
    pub fn try_send(&self, value: T) -> Result<(), SendError<T>> {
        let mut state = self.0.lock();
        if state.closed {
            return Err(SendError::Closed(value));
        }
        match state.sender.try_send(value) {
            Ok(()) => {
                state.len += 1;
                drop(state);
                self.0.readable.notify_one();
                Ok(())
            }
            Err(mpsc::TrySendError::Full(value)) => Err(SendError::Full(value)),
            Err(mpsc::TrySendError::Disconnected(value)) => Err(SendError::Closed(value)),
        }
    }
    /// 在一次加锁内整批入队。Vec[0] 最先入队，其他生产者不能插入批次内部。
    /// 空 Vec 是无操作；有界队列可能返回 TooLarge 或 Full，无界队列不检查容量。
    pub fn try_send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        if values.is_empty() {
            return Ok(());
        }
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
        if self
            .capacity()
            .is_some_and(|capacity| values.len() > capacity - state.len)
        {
            return Err(SendError::Full(values));
        }
        state.enqueue_batch(values);
        drop(state);
        self.0.readable.notify_all();
        Ok(())
    }
    /// 在一次加锁内放入有序前缀，返回有序后缀；没有空位时整批返回 Full。
    /// 成功放入的前缀不会被其他生产者插入，但后缀再次提交是另一次独立操作。
    pub fn try_send_batch_partial(&self, mut values: Vec<T>) -> Result<Vec<T>, SendError<Vec<T>>> {
        if values.is_empty() {
            return Ok(values);
        }
        let mut state = self.0.lock();
        if state.closed {
            return Err(SendError::Closed(values));
        }
        let count = self.capacity().map_or(values.len(), |capacity| {
            values.len().min(capacity - state.len)
        });
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
    pub fn send_batch(&self, values: Vec<T>) -> Result<(), SendError<Vec<T>>> {
        self.wait_send_batch(values, None, None)
    }
    pub fn send_batch_with(
        &self,
        values: Vec<T>,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), SendError<Vec<T>>> {
        self.wait_send_batch(values, None, Some(&cancelled))
    }
    pub fn send_batch_timeout(
        &self,
        values: Vec<T>,
        timeout: Duration,
    ) -> Result<(), SendError<Vec<T>>> {
        self.wait_send_batch(values, Some(timeout), None)
    }
    pub fn send_batch_timeout_with(
        &self,
        values: Vec<T>,
        timeout: Duration,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), SendError<Vec<T>>> {
        self.wait_send_batch(values, Some(timeout), Some(&cancelled))
    }
    fn wait_send_batch(
        &self,
        values: Vec<T>,
        timeout: Option<Duration>,
        cancelled: Option<&dyn Fn() -> bool>,
    ) -> Result<(), SendError<Vec<T>>> {
        if values.is_empty() {
            return Ok(());
        }
        if self
            .capacity()
            .is_some_and(|capacity| values.len() > capacity)
        {
            return Err(SendError::TooLarge(values));
        }
        let started = Instant::now();
        loop {
            if cancelled.is_some_and(|check| check()) {
                return Err(SendError::Cancelled(values));
            }
            let mut state = self.0.lock();
            if state.closed {
                return Err(SendError::Closed(values));
            }
            if self
                .capacity()
                .is_none_or(|capacity| values.len() <= capacity - state.len)
            {
                state.enqueue_batch(values);
                drop(state);
                self.0.readable.notify_all();
                return Ok(());
            }
            let remaining = timeout.map(|timeout| timeout.saturating_sub(started.elapsed()));
            if remaining == Some(Duration::ZERO) {
                return Err(SendError::Timeout(values));
            }
            self.0
                .wait(&self.0.writable, state, remaining, cancelled.is_some());
        }
    }
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        self.wait_send(value, None, None)
    }
    pub fn send_with(&self, value: T, cancelled: impl Fn() -> bool) -> Result<(), SendError<T>> {
        self.wait_send(value, None, Some(&cancelled))
    }
    pub fn send_timeout(&self, value: T, timeout: Duration) -> Result<(), SendError<T>> {
        self.wait_send(value, Some(timeout), None)
    }
    pub fn send_timeout_with(
        &self,
        value: T,
        timeout: Duration,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), SendError<T>> {
        self.wait_send(value, Some(timeout), Some(&cancelled))
    }
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
                Err(mpsc::TrySendError::Full(returned)) => value = returned,
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
    pub fn capacity(&self) -> Option<usize> {
        self.0.capacity
    }
    pub fn len(&self) -> usize {
        self.0.lock().len
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn is_closed(&self) -> bool {
        self.0.lock().closed
    }
    pub fn close(&self) {
        self.0.close();
    }
    pub fn try_recv(&self) -> Result<T, RecvError> {
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
                Ok(value)
            }
            Err(_) if state.closed => Err(RecvError::Closed),
            Err(_) => Err(RecvError::Empty),
        }
    }
    /// 一次加锁取出有序前缀，Vec[0] 最先出队。max=0 是无操作。
    pub fn try_recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError> {
        if max == 0 {
            return Ok(Vec::new());
        }
        let mut state = self.0.lock();
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
    pub fn recv_batch(&self, max: usize) -> Result<Vec<T>, RecvError> {
        self.wait_recv_batch(max, None, None)
    }
    pub fn recv_batch_with(
        &self,
        max: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<Vec<T>, RecvError> {
        self.wait_recv_batch(max, None, Some(&cancelled))
    }
    pub fn recv_batch_timeout(&self, max: usize, timeout: Duration) -> Result<Vec<T>, RecvError> {
        self.wait_recv_batch(max, Some(timeout), None)
    }
    pub fn recv_batch_timeout_with(
        &self,
        max: usize,
        timeout: Duration,
        cancelled: impl Fn() -> bool,
    ) -> Result<Vec<T>, RecvError> {
        self.wait_recv_batch(max, Some(timeout), Some(&cancelled))
    }
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
            if cancelled.is_some_and(|check| check()) {
                return Err(RecvError::Cancelled);
            }
            let mut state = self.0.lock();
            if state.len > 0 {
                let values = state.dequeue_batch(max);
                drop(state);
                self.0.writable.notify_all();
                return Ok(values);
            }
            if state.closed {
                return Err(RecvError::Closed);
            }
            let remaining = timeout.map(|timeout| timeout.saturating_sub(started.elapsed()));
            if remaining == Some(Duration::ZERO) {
                return Err(RecvError::Timeout);
            }
            self.0
                .wait(&self.0.readable, state, remaining, cancelled.is_some());
        }
    }
    pub fn recv(&self) -> Result<T, RecvError> {
        self.wait_recv(None, None)
    }
    pub fn recv_with(&self, cancelled: impl Fn() -> bool) -> Result<T, RecvError> {
        self.wait_recv(None, Some(&cancelled))
    }
    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvError> {
        self.wait_recv(Some(timeout), None)
    }
    pub fn recv_timeout_with(
        &self,
        timeout: Duration,
        cancelled: impl Fn() -> bool,
    ) -> Result<T, RecvError> {
        self.wait_recv(Some(timeout), Some(&cancelled))
    }
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
                Err(_) if state.closed => return Err(RecvError::Closed),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::HashSet,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        thread,
    };

    #[test]
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
    fn unbounded_last_consumer_releases_unread_resources() {
        struct Resource(Arc<AtomicUsize>);
        impl Drop for Resource {
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
    fn batch_receive_waits_for_first_item_without_waiting_to_fill_the_batch() {
        let (sender, receiver) = BoundedQueue::new(3).split();
        let worker = thread::spawn(move || receiver.recv_batch(10));
        sender.send_batch(vec![10, 11]).unwrap();
        assert_eq!(worker.join().unwrap().unwrap(), vec![10, 11]);
    }

    #[test]
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
    fn empty_batch_and_zero_receive_limit_are_no_ops() {
        let (sender, receiver) = BoundedQueue::<u8>::new(1).split();
        sender.close();
        assert_eq!(sender.send_batch_with(vec![], || true), Ok(()));
        assert_eq!(sender.try_send_batch_partial(vec![]), Ok(vec![]));
        assert_eq!(receiver.recv_batch_with(0, || true), Ok(vec![]));
        assert_eq!(receiver.try_recv_batch(0), Ok(vec![]));
    }

    #[test]
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
    fn last_receiver_releases_tasks_outside_the_queue_lock() {
        struct Task(Option<Box<dyn FnOnce() + Send>>);
        impl Drop for Task {
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
    fn cancellation_callbacks_can_inspect_the_queue() {
        let (sender, receiver) = BoundedQueue::new(1).split();
        assert!(sender.send_with(1, || sender.is_closed()).is_ok());
        assert_eq!(receiver.recv_with(|| receiver.is_closed()), Ok(1));
    }
}

#[cfg(test)]
mod listener_tests {
    use super::*;
    use std::{
        cell::Cell,
        sync::atomic::{AtomicUsize, Ordering},
    };
    fn policy(count: usize, interval: Duration) -> BatchPolicy {
        BatchPolicy { count, interval }
    }
    #[test]
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
    #[test]
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
    #[test]
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
    #[test]
    fn cancelling_listener_drops_pending_resources_without_calling_consumer() {
        struct Resource(Arc<AtomicUsize>);
        impl Drop for Resource {
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
    #[test]
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
