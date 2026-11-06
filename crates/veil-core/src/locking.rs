//! 锁等待选项；数据库文件租约协议由 veil-service 实现。
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

/// 文件或目录租约的访问模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockMode {
    /// SR：共享读锁。
    Read,
    /// RO：不阻止写入，也不保存租约。
    ReadOnly,
    /// SW：共享写锁，允许普通读取。
    SharedWrite,
    /// EW：排他写锁。
    Write,
}

impl LockMode {
    /// 与既有容器数据库兼容的存储值。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read | Self::ReadOnly => "read",
            Self::SharedWrite | Self::Write => "write",
        }
    }
}
#[derive(Clone, Default)]
pub struct LockOptions {
    /// 单调计时的最长等待时间；None 允许一直等待。
    pub timeout: Option<Duration>,
    /// 仅取消等待，不能提前结束仍在运行的阻塞工作。
    pub cancellation: Option<Arc<AtomicBool>>,
}
