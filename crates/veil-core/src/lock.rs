//! Veil 锁模块。
//!
//! 本模块是锁的唯一操作入口，统一负责：
//! - `config.lock`、`.veil-meta.lock` 和 `.enc` 文件锁；
//! - 共享锁和排他锁的获取、等待与释放；
//! - 锁令牌、PID、进程启动时间和主机标识；
//! - 锁记录的创建、校验和陈旧锁判断。
//!
//! 上层模块只能调用本模块获取锁，不得直接调用 `flock`、`LockFileEx` 或
//! `File::lock` 等平台锁接口。
//!
//! 文件锁使用标准库的跨平台实现。进程存活与启动时间检查支持 Linux、
//! macOS 和 Windows。

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{Result, VeilError};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

/// 锁记录对应的逻辑资源类型。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum LockScope {
    /// Work 级操作，例如创建或分配 Work 下的 `veil_dir`。
    ///
    /// 该级别不对应单个 Veil，`veil_id` 可以留空。
    Work,
    /// 单个 Veil 的逻辑级操作，例如初始化、恢复和 Veil 维护。
    Veil,
    /// `.veil-meta` 的读取快照或原子提交。
    Metadata,
    /// 单个逻辑文件内容的读取、重写或删除。
    File,
    /// 全局配置文件的读取和修改。
    Config,
}

/// 文件锁的共享模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    /// 多个读取者可以同时持有共享锁。
    Shared,
    /// 同一资源同一时间只能有一个写者持有排他锁。
    Exclusive,
}

/// 一次持锁的唯一令牌。
///
/// 令牌使用系统随机源生成。更新或清除锁记录时必须比较令牌，不能只按 PID 判断，
/// 否则可能清除另一个进程重新获得的锁。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockRecord {
    /// 锁记录对应的资源类型。
    pub scope: LockScope,

    /// 锁记录所属的 Veil 稳定 ID。
    ///
    /// Work 级和全局配置级锁可以留空；Veil 级、元数据级和文件级锁必须填写。
    pub veil_id: String,

    /// 文件级锁对应的稳定文件 ID。
    ///
    /// 只有 [`LockScope::File`] 使用该字段。不能使用原始路径或随机密文文件名，
    /// 因为文件重命名和完整改密都会改变这些值。
    pub file_id: Option<String>,

    /// 本次持锁的唯一令牌。
    pub lock_token: String,

    /// 持锁进程的操作系统进程 ID。
    ///
    /// PID 只用于本机诊断和辅助判断，不能单独证明进程仍然持有锁。
    pub pid: u32,

    /// 持锁进程的启动时间标识。
    ///
    /// 该值来自操作系统，不是当前时间。与 PID 一起比较可以避免 PID 被复用后
    /// 把无关进程误认为锁持有者。
    pub process_start_time: String,

    /// 持锁进程所在的主机标识。
    ///
    /// 多主机共享外置盘或网络卷时，本机 PID 检查没有意义，因此必须先比较
    /// `host_id`。该字段优先使用稳定机器 ID，无法获取时回退到主机名。
    pub host_id: String,

    /// 获取锁时执行的操作名称。
    ///
    /// 例如 `add`、`rm`、`passwd-full` 或 `cleanup`，用于诊断和提示用户当前
    /// 为什么无法获取资源。
    pub operation: String,

    /// 获取锁的 RFC 3339 时间。
    ///
    /// 该字段用于展示、排序和判断长时间未释放的操作，不应作为唯一的超时依据。
    pub acquired_at: String,
}

impl LockRecord {
    /// 创建当前进程的锁记录，并自动生成 `lock_token`。
    pub fn new(
        scope: LockScope,
        veil_id: impl Into<String>,
        file_id: Option<String>,
        operation: impl Into<String>,
    ) -> Self {
        Self::with_token(scope, veil_id, file_id, operation, generate_lock_token())
    }

    /// 使用调用方提供的锁令牌创建当前进程的锁记录。
    ///
    /// 文件锁守卫和锁记录必须使用同一个令牌，因此文件操作应优先调用
    /// [`FileLockGuard::to_record`]。
    pub fn with_token(
        scope: LockScope,
        veil_id: impl Into<String>,
        file_id: Option<String>,
        operation: impl Into<String>,
        lock_token: impl Into<String>,
    ) -> Self {
        Self {
            scope,
            veil_id: veil_id.into(),
            file_id,
            lock_token: lock_token.into(),
            pid: std::process::id(),
            process_start_time: current_process_start_time(),
            host_id: current_host_id(),
            operation: operation.into(),
            acquired_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// 返回锁记录中的令牌。
    pub fn token(&self) -> &str {
        &self.lock_token
    }

    /// 校验给定令牌是否与锁记录一致。
    pub fn matches_token(&self, token: &str) -> bool {
        self.lock_token == token
    }

    /// 校验锁记录是否仍然属于给定令牌。
    pub fn validate_token(&self, token: &str) -> Result<()> {
        if self.matches_token(token) {
            Ok(())
        } else {
            Err(VeilError::LockError(
                "锁令牌不匹配，拒绝释放其他进程持有的锁".to_string(),
            ))
        }
    }

    /// 判断锁记录是否已经陈旧。
    ///
    /// 只有锁记录来自本机时才能得到确定结果：
    /// - 返回 `Some(true)`：进程不存在或启动时间不一致，记录可回收；
    /// - 返回 `Some(false)`：进程仍存在且启动时间一致，记录仍有效；
    /// - 返回 `None`：锁记录来自其他主机，必须依赖操作系统锁或租约判断。
    pub fn owner_alive_locally(&self) -> Option<bool> {
        if self.host_id != current_host_id() {
            return None;
        }

        if !process_is_alive(self.pid) {
            return Some(false);
        }

        match process_start_time_for_pid(self.pid) {
            Some(current_start_time) if !self.process_start_time.is_empty() => {
                Some(self.process_start_time == current_start_time)
            }
            _ => Some(true),
        }
    }

    /// 返回本机锁记录是否已经失效。
    ///
    /// 跨主机锁记录返回 `false`，表示当前程序不能仅凭本机 PID 判断其是否陈旧。
    pub fn is_stale_locally(&self) -> bool {
        self.owner_alive_locally() == Some(false)
    }
}

/// 基于操作系统文件锁的 RAII 守卫。
///
/// 守卫析构并关闭文件时，操作系统会自动释放锁。即使进程被强制杀死，也不会留下
/// 无法释放的操作系统锁。
#[derive(Debug)]
pub struct FileLockGuard {
    file: File,
    path: PathBuf,
    mode: LockMode,
    lock_token: String,
    released: bool,
}

impl FileLockGuard {
    /// 阻塞获取文件锁。
    ///
    /// `timeout` 为 [`Duration::ZERO`] 时只尝试一次；否则按短间隔重试，直到超时。
    /// 锁文件不存在时会自动创建，Unix 权限为 `0600`。
    pub fn acquire(path: impl AsRef<Path>, mode: LockMode, timeout: Duration) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = open_lock_file(&path)?;
        let started_at = Instant::now();

        loop {
            match try_lock_file(&file, mode) {
                Ok(()) => return Ok(Self::new(file, path, mode)),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if started_at.elapsed() >= timeout {
                        return Err(VeilError::LockError(format!(
                            "等待锁超时: {}",
                            path.display()
                        )));
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => {
                    return Err(VeilError::LockError(format!(
                        "获取锁失败 {}: {}",
                        path.display(),
                        error
                    )));
                }
            }
        }
    }

    /// 非阻塞尝试获取文件锁。
    ///
    /// 返回 `Ok(None)` 表示锁当前被其他进程或文件句柄占用。
    pub fn try_acquire(path: impl AsRef<Path>, mode: LockMode) -> Result<Option<Self>> {
        let path = path.as_ref().to_path_buf();
        let file = open_lock_file(&path)?;

        match try_lock_file(&file, mode) {
            Ok(()) => Ok(Some(Self::new(file, path, mode))),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(VeilError::LockError(format!(
                "尝试获取锁失败 {}: {}",
                path.display(),
                error
            ))),
        }
    }

    /// 返回锁文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 返回当前锁模式。
    pub fn mode(&self) -> LockMode {
        self.mode
    }

    /// 返回本次持锁的令牌。
    pub fn token(&self) -> &str {
        &self.lock_token
    }

    /// 为当前守卫创建对应锁记录，确保记录令牌与操作系统锁一致。
    pub fn to_record(
        &self,
        scope: LockScope,
        veil_id: impl Into<String>,
        file_id: Option<String>,
        operation: impl Into<String>,
    ) -> LockRecord {
        LockRecord::with_token(scope, veil_id, file_id, operation, self.lock_token.clone())
    }

    /// 显式释放锁并返回释放错误。
    ///
    /// 无论该方法成功还是失败，守卫都会在返回前结束生命周期。
    pub fn release(mut self) -> Result<()> {
        let result = self
            .file
            .unlock()
            .map_err(|error| VeilError::LockError(format!("释放锁失败: {}", error)));
        self.released = true;
        result
    }
}

impl Drop for FileLockGuard {
    /// 守卫离开作用域时尽力释放锁；真正释放仍由关闭文件句柄保证。
    fn drop(&mut self) {
        if !self.released {
            let _ = self.file.unlock();
        }
    }
}

impl FileLockGuard {
    /// 使用已打开的文件和令牌创建守卫。
    fn new(file: File, path: PathBuf, mode: LockMode) -> Self {
        Self {
            file,
            path,
            mode,
            lock_token: generate_lock_token(),
            released: false,
        }
    }
}

/// 打开或创建锁文件，并保持文件句柄用于操作系统锁。
fn open_lock_file(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        crate::fsutil::create_dir_all_durable(parent)?;
    }

    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);

    #[cfg(unix)]
    options.mode(0o600);

    options.open(path).map_err(|error| {
        VeilError::LockError(format!("打开锁文件失败 {}: {}", path.display(), error))
    })
}

/// 根据锁模式执行一次非阻塞操作系统锁尝试。
fn try_lock_file(file: &File, mode: LockMode) -> io::Result<()> {
    let result = match mode {
        LockMode::Shared => file.try_lock_shared(),
        LockMode::Exclusive => file.try_lock(),
    };

    result.map_err(|error| match error {
        TryLockError::WouldBlock => io::Error::new(io::ErrorKind::WouldBlock, "锁被占用"),
        TryLockError::Error(error) => error,
    })
}

/// 生成 128 位锁令牌。
pub fn generate_lock_token() -> String {
    let mut token = [0u8; 16];
    getrandom::getrandom(&mut token).expect("生成锁令牌失败");
    hex::encode(token)
}

/// 返回当前进程的稳定启动时间标识。
pub fn current_process_start_time() -> String {
    process_start_time_for_pid(std::process::id()).unwrap_or_default()
}

/// 返回当前主机的稳定标识。
pub fn current_host_id() -> String {
    static HOST_ID: OnceLock<String> = OnceLock::new();
    HOST_ID.get_or_init(compute_host_id).clone()
}

/// 计算主机标识，优先使用平台机器 ID，失败时回退到主机名。
fn compute_host_id() -> String {
    if let Some(host_id) = platform_host_id() {
        return host_id;
    }

    for key in ["HOSTNAME", "COMPUTERNAME"] {
        if let Ok(value) = std::env::var(key)
            && !value.trim().is_empty()
        {
            return value;
        }
    }

    "unknown-host".to_string()
}

/// Linux 使用 machine-id 作为稳定主机标识。
#[cfg(target_os = "linux")]
fn platform_host_id() -> Option<String> {
    for path in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
        if let Ok(value) = std::fs::read_to_string(path) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// macOS 使用系统 host UUID 作为稳定主机标识。
#[cfg(target_os = "macos")]
fn platform_host_id() -> Option<String> {
    let mut uuid = [0u8; 16];
    let result = unsafe { libc::gethostuuid(uuid.as_mut_ptr(), std::ptr::null()) };
    (result == 0).then(|| hex::encode(uuid))
}

/// Windows 使用计算机名作为主机标识回退值。
#[cfg(windows)]
fn platform_host_id() -> Option<String> {
    windows_computer_name()
}

/// 其他平台没有稳定机器 ID 时使用环境变量回退。
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn platform_host_id() -> Option<String> {
    None
}

/// 判断本机进程是否存在。
#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return true;
    }

    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Windows 通过进程句柄等待状态判断进程是否存在。
#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    let handle = windows_open_process(
        pid,
        windows::SYNCHRONIZE | windows::PROCESS_QUERY_LIMITED_INFORMATION,
    );
    if handle == 0 {
        return windows::last_error() == windows::ERROR_ACCESS_DENIED;
    }

    let status = unsafe { windows::WaitForSingleObject(handle, 0) };
    unsafe {
        windows::CloseHandle(handle);
    }
    status == windows::WAIT_TIMEOUT
}

/// 非 Unix、非 Windows 平台只识别当前进程。
#[cfg(not(any(unix, windows)))]
fn process_is_alive(pid: u32) -> bool {
    pid == std::process::id()
}

/// 读取指定进程的启动时间。
#[cfg(target_os = "linux")]
fn process_start_time_for_pid(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let command_end = stat.rfind(')')?.checked_add(1)?;
    let fields: Vec<_> = stat[command_end..].split_whitespace().collect();
    // `/proc/<pid>/stat` 的 starttime 是第 22 个字段；命令名结束后它是索引 19。
    fields.get(19).map(|value| (*value).to_string())
}

/// macOS 使用 `proc_pidinfo` 读取进程启动时间。
#[cfg(target_os = "macos")]
fn process_start_time_for_pid(pid: u32) -> Option<String> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let result = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };

    (result == size).then(|| format!("{}.{}", info.pbi_start_tvsec, info.pbi_start_tvusec))
}

/// 其他平台暂时无法获取稳定的进程启动时间。
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_start_time_for_pid(_pid: u32) -> Option<String> {
    #[cfg(windows)]
    {
        return windows_process_start_time(_pid);
    }

    #[cfg(not(windows))]
    {
        None
    }
}

/// 打开 Windows 进程并返回可用于等待或查询的句柄。
#[cfg(windows)]
fn windows_open_process(pid: u32, access: u32) -> isize {
    unsafe { windows::OpenProcess(access, 0, pid) }
}

/// 读取 Windows 进程启动时间，返回 FILETIME 组合后的 64 位值。
#[cfg(windows)]
fn windows_process_start_time(pid: u32) -> Option<String> {
    let handle = windows_open_process(pid, windows::PROCESS_QUERY_LIMITED_INFORMATION);
    if handle == 0 {
        return None;
    }

    let mut creation = windows::FileTime::default();
    let mut exit = windows::FileTime::default();
    let mut kernel = windows::FileTime::default();
    let mut user = windows::FileTime::default();
    let result = unsafe {
        windows::GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user)
    };
    unsafe {
        windows::CloseHandle(handle);
    }

    if result == 0 {
        return None;
    }

    let file_time = (u64::from(creation.high) << 32) | u64::from(creation.low);
    Some(file_time.to_string())
}

/// 读取 Windows DNS 主机名。
#[cfg(windows)]
fn windows_computer_name() -> Option<String> {
    const COMPUTER_NAME_DNS_HOSTNAME: i32 = 1;

    let mut size = 0u32;
    unsafe {
        windows::GetComputerNameExW(COMPUTER_NAME_DNS_HOSTNAME, std::ptr::null_mut(), &mut size);
    }
    if size == 0 {
        return None;
    }

    let mut buffer = vec![0u16; size as usize];
    let result = unsafe {
        windows::GetComputerNameExW(COMPUTER_NAME_DNS_HOSTNAME, buffer.as_mut_ptr(), &mut size)
    };
    if result == 0 {
        return None;
    }

    buffer.truncate(size as usize);
    while buffer.last() == Some(&0) {
        buffer.pop();
    }
    String::from_utf16(&buffer).ok()
}

/// Windows 原生 API 的最小 FFI 声明。
#[cfg(windows)]
mod windows {
    /// 查询有限进程信息的访问权限。
    pub const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    /// 等待进程对象的权限。
    pub const SYNCHRONIZE: u32 = 0x0010_0000;
    /// `WaitForSingleObject` 表示对象仍处于活动状态。
    pub const WAIT_TIMEOUT: u32 = 258;
    /// 进程存在但当前用户无权打开。
    pub const ERROR_ACCESS_DENIED: u32 = 5;

    /// 读取当前线程的 Windows 错误码。
    pub fn last_error() -> u32 {
        unsafe { GetLastError() }
    }

    /// Windows `FILETIME` 的布局。
    #[repr(C)]
    #[derive(Default)]
    pub struct FileTime {
        /// 低 32 位。
        pub low: u32,
        /// 高 32 位。
        pub high: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        /// 打开进程并返回句柄。
        pub fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        /// 关闭 Windows 句柄。
        pub fn CloseHandle(handle: isize) -> i32;
        /// 查询进程创建、退出、内核和用户时间。
        pub fn GetProcessTimes(
            handle: isize,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        /// 等待进程对象，超时值为 0 时只检查当前状态。
        pub fn WaitForSingleObject(handle: isize, milliseconds: u32) -> u32;
        /// 返回当前线程的 Windows 错误码。
        pub fn GetLastError() -> u32;
        /// 读取计算机名。
        pub fn GetComputerNameExW(name_type: i32, buffer: *mut u16, size: *mut u32) -> i32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// 验证锁记录可以完整序列化和反序列化。
    #[test]
    fn lock_record_roundtrip() {
        let record = LockRecord::new(
            LockScope::File,
            "veil-1234",
            Some("file-5678".to_string()),
            "passwd-full",
        );

        let bytes = serde_json::to_vec(&record).unwrap();
        let restored: LockRecord = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored, record);
    }

    /// 验证当前进程创建的锁记录不会被判断为陈旧。
    #[test]
    fn current_process_record_is_not_stale() {
        let record = LockRecord::new(LockScope::File, "veil-1234", None, "test");
        assert_eq!(record.owner_alive_locally(), Some(true));
        assert!(!record.is_stale_locally());
    }

    /// 验证多个共享锁可以同时存在。
    #[test]
    fn shared_locks_can_coexist() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("resource.lock");
        let first = FileLockGuard::try_acquire(&path, LockMode::Shared)
            .unwrap()
            .unwrap();
        let second = FileLockGuard::try_acquire(&path, LockMode::Shared)
            .unwrap()
            .unwrap();

        assert!(
            FileLockGuard::try_acquire(&path, LockMode::Exclusive)
                .unwrap()
                .is_none()
        );

        drop(first);
        drop(second);
    }

    /// 验证排他锁会阻塞其他共享锁和排他锁。
    #[test]
    fn exclusive_lock_blocks_other_locks() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("resource.lock");
        let guard = FileLockGuard::try_acquire(&path, LockMode::Exclusive)
            .unwrap()
            .unwrap();

        assert!(
            FileLockGuard::try_acquire(&path, LockMode::Shared)
                .unwrap()
                .is_none()
        );
        assert!(
            FileLockGuard::try_acquire(&path, LockMode::Exclusive)
                .unwrap()
                .is_none()
        );

        drop(guard);
    }

    /// 验证令牌校验可以阻止错误释放。
    #[test]
    fn lock_record_token_must_match() {
        let record = LockRecord::new(LockScope::Veil, "veil-1234", None, "init");
        assert!(record.validate_token(record.token()).is_ok());
        assert!(record.validate_token("wrong-token").is_err());
    }
}
