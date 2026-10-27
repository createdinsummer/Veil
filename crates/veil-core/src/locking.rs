//! 容器内跨进程锁；锁文件始终保留，锁由文件句柄控制。

use crate::error::VeilError;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

/// 每次锁等待的超时和取消标志；默认无限等待。
#[derive(Clone, Default)]
pub struct LockOptions {
    /// 每个锁的最长等待时间；`None` 表示无限等待。
    pub timeout: Option<Duration>,
    /// 设置为 `true` 时取消正在等待的锁请求。
    pub cancellation: Option<Arc<AtomicBool>>,
}

/// 句柄关闭时释放系统锁。
#[doc(hidden)]
pub struct Guard(Option<File>);

impl Guard {
    fn release(mut self) -> std::io::Result<()> {
        match self.0.take() {
            Some(file) => file.unlock(),
            None => Ok(()),
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(file) = self.0.take() {
            let _ = file.unlock();
        }
    }
}

/// 对指定容器中的逻辑文件获取跨进程锁。
///
/// 锁通过稳定文件 ID 定位；本接口不检查该文件是否存在，也不读取密码或元数据。
/// 锁不可重入：持有锁时不要在同一调用链中调用会重新获取文件锁或维护锁的容器操作。
pub struct FileLockManager {
    veil_dir: PathBuf,
    options: LockOptions,
}

impl FileLockManager {
    /// 绑定容器目录，默认无限等待锁。
    pub fn new(veil_dir: impl Into<PathBuf>) -> Self {
        Self {
            veil_dir: veil_dir.into(),
            options: LockOptions::default(),
        }
    }

    /// 设置每次锁等待的超时和取消标志。
    pub fn with_lock_options(mut self, options: LockOptions) -> Self {
        self.options = options;
        self
    }

    /// 获取文件共享锁；可与同一文件的其他共享锁并存。
    pub fn lock_file_shared(&self, file_id: &str) -> Result<FileLockGuard, VeilError> {
        self.lock_file(file_id, true)
    }

    /// 获取文件独占锁；与同一文件的共享锁和独占锁互斥。
    pub fn lock_file_exclusive(&self, file_id: &str) -> Result<FileLockGuard, VeilError> {
        self.lock_file(file_id, false)
    }

    fn lock_file(&self, file_id: &str, shared: bool) -> Result<FileLockGuard, VeilError> {
        if file_id.trim().is_empty() {
            return Err(VeilError::InvalidFormat("文件 ID 不能为空".into()));
        }
        let maintenance = maintenance(&self.veil_dir, true, &self.options)?;
        let file = file(&self.veil_dir, file_id, shared, &self.options)?;
        Ok(FileLockGuard {
            file: Some(file),
            maintenance: Some(maintenance),
        })
    }
}

/// 文件锁守卫。离开作用域时自动释放，也可调用 [`Self::release`] 显式释放。
///
/// 守卫同时持有容器共享维护锁，保证清理、改密不会与持锁操作冲突。
#[must_use = "丢弃守卫会立即释放文件锁"]
pub struct FileLockGuard {
    // 字段按声明顺序释放，保证文件锁先于维护锁释放。
    file: Option<Guard>,
    maintenance: Option<Guard>,
}

impl FileLockGuard {
    /// 显式释放文件锁和容器共享维护锁，返回解锁时的 I/O 错误。
    pub fn release(mut self) -> Result<(), VeilError> {
        let file_result = self.file.take().map_or(Ok(()), Guard::release);
        let maintenance_result = self.maintenance.take().map_or(Ok(()), Guard::release);
        file_result.and(maintenance_result).map_err(VeilError::Io)
    }
}

fn open(veil_dir: &Path, name: &str) -> Result<File, VeilError> {
    if !veil_dir.is_dir() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "容器目录不存在").into());
    }
    let directory = veil_dir.join(".locks");
    fs::create_dir_all(&directory)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(name))?;
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: 文件句柄有效，成功返回时系统完整填充 statfs。
        if unsafe { libc::fstatfs(file.as_raw_fd(), info.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: fstatfs 已成功初始化结构体。
        let info = unsafe { info.assume_init() };
        if info.f_flags & (libc::MNT_LOCAL as u32) == 0 {
            return Err(VeilError::WorkError(
                "容器位于不受支持的网络文件系统".into(),
            ));
        }
    }
    Ok(file)
}

fn acquire(
    veil_dir: &Path,
    name: &str,
    shared: bool,
    options: &LockOptions,
) -> Result<Guard, VeilError> {
    let file = open(veil_dir, name)?;
    let start = Instant::now();
    loop {
        if options
            .cancellation
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            return Err(VeilError::WorkError("锁等待已取消".into()));
        }
        let result = if shared {
            file.try_lock_shared()
        } else {
            file.try_lock()
        };
        match result {
            Ok(()) => return Ok(Guard(Some(file))),
            Err(TryLockError::Error(error)) => return Err(error.into()),
            Err(TryLockError::WouldBlock) => {
                if options
                    .timeout
                    .is_some_and(|timeout| start.elapsed() >= timeout)
                {
                    return Err(VeilError::WorkError("锁等待超时".into()));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[doc(hidden)]
pub fn maintenance(
    veil_dir: &Path,
    shared: bool,
    options: &LockOptions,
) -> Result<Guard, VeilError> {
    acquire(veil_dir, "maintenance.lock", shared, options)
}

#[doc(hidden)]
pub fn config(veil_dir: &Path, shared: bool, options: &LockOptions) -> Result<Guard, VeilError> {
    acquire(veil_dir, "config.lock", shared, options)
}

#[doc(hidden)]
pub fn namespace(veil_dir: &Path, shared: bool, options: &LockOptions) -> Result<Guard, VeilError> {
    acquire(veil_dir, "namespace.lock", shared, options)
}

#[doc(hidden)]
pub fn file(
    veil_dir: &Path,
    file_id: &str,
    shared: bool,
    options: &LockOptions,
) -> Result<Guard, VeilError> {
    let name = format!("file-{}.lock", blake3::hash(file_id.as_bytes()).to_hex());
    acquire(veil_dir, &name, shared, options)
}

#[doc(hidden)]
pub fn path(
    veil_dir: &Path,
    path: &str,
    key: &[u8; 32],
    options: &LockOptions,
) -> Result<Guard, VeilError> {
    // 带密钥摘要避免锁文件名泄露可猜测的原始路径。
    let name = format!(
        "path-{}.lock",
        blake3::keyed_hash(key, path.as_bytes()).to_hex()
    );
    acquire(veil_dir, &name, false, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn lock_worker() {
        let Some(directory) = std::env::var_os("VEIL_LOCK_TEST_DIR") else {
            return;
        };
        let directory = Path::new(&directory);
        let _guard = file(directory, "one", false, &LockOptions::default()).unwrap();
        fs::write(directory.join("ready"), b"ready").unwrap();
        std::thread::sleep(Duration::from_secs(20));
    }

    #[test]
    fn cross_process_locks_timeout_cancel_and_release_on_death() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "locking::tests::lock_worker"])
            .env("VEIL_LOCK_TEST_DIR", dir.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let start = Instant::now();
        while !dir.path().join("ready").exists() {
            assert!(start.elapsed() < Duration::from_secs(10), "子进程未获取锁");
            std::thread::sleep(Duration::from_millis(10));
        }
        let timeout = LockOptions {
            timeout: Some(Duration::from_millis(50)),
            cancellation: None,
        };
        assert!(file(dir.path(), "one", true, &timeout).is_err());
        let _other = file(dir.path(), "two", false, &timeout).unwrap();
        let cancelled = LockOptions {
            timeout: None,
            cancellation: Some(Arc::new(AtomicBool::new(true))),
        };
        assert!(file(dir.path(), "one", false, &cancelled).is_err());
        child.kill().unwrap();
        child.wait().unwrap();
        let _released = file(dir.path(), "one", false, &timeout).unwrap();
    }

    #[test]
    fn shared_locks_coexist_and_exclude_writer() {
        let dir = tempfile::TempDir::new().unwrap();
        let timeout = LockOptions {
            timeout: Some(Duration::from_millis(20)),
            cancellation: None,
        };
        let first = file(dir.path(), "one", true, &timeout).unwrap();
        let second = file(dir.path(), "one", true, &timeout).unwrap();
        assert!(file(dir.path(), "one", false, &timeout).is_err());
        drop(first);
        drop(second);
        assert!(file(dir.path(), "one", false, &timeout).is_ok());
    }
}
