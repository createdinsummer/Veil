//! 锁等待选项；数据库文件租约协议由 veil-service 实现。
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
#[derive(Clone, Default)]
pub struct LockOptions {
    /// 单调计时的最长等待时间；None 允许一直等待。
    pub timeout: Option<Duration>,
    /// 仅取消等待，不能提前结束仍在运行的阻塞工作。
    pub cancellation: Option<Arc<AtomicBool>>,
}
