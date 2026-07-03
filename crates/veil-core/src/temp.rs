//! # temp —— 临时明文文件的 RAII 守卫（视频 V1 用）
//!
//! 视频太大，不能整份塞内存显示。V1 的做法：把它解密到一个**受控临时位置**，
//! 交系统播放器打开，看完即删。本模块提供 [`TempPlaintext`]——一个 RAII 守卫：
//! 它持有临时文件路径，**离开作用域（Drop）时自动删除该文件**，确保明文不残留。
//!
//! 安全纪律（对应 spec §6.2 / §9）：
//! - 临时位置**优先 RAM 盘**（Linux `/dev/shm`），断电即无；否则退回磁盘临时目录。
//! - 文件权限设为**仅当前用户**（Unix `0600`）。
//! - Drop 时 best-effort 删除；**SSD 上删除不保证物理擦除**，需在 UI 如实告知。

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// 持有一个临时明文文件；**Drop 时自动删除它**（RAII 守卫）。
///
/// 拿它的 [`path`](Self::path) 交给系统播放器；守卫变量一旦离开作用域，
/// 临时明文文件即被删除——不必手动清理，也不怕中途 return/panic 漏删。
pub struct TempPlaintext {
    path: PathBuf,
}

impl TempPlaintext {
    /// 临时文件路径（交给播放器打开）。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempPlaintext {
    /// 守卫销毁时自动调用：删除临时明文文件。
    fn drop(&mut self) {
        // best-effort：删除失败也不 panic（Drop 里绝不能 panic）
        let _ = std::fs::remove_file(&self.path);
    }
}

/// 把 `reader` 里的明文流式写到一个受控临时文件，返回 RAII 守卫。
///
/// 流式 `io::copy`，不整份进内存（大视频友好）。文件建在优先 RAM 盘、权限 0600。
///
/// # 参数
/// - `virtual_path`: 原文件的容器内路径（仅用来取扩展名，播放器靠它选解码器）
/// - `reader`:       已解密的明文流（如 [`crate::container::Container`] 的 blob 解密流）
pub fn decrypt_to_temp<R: Read>(virtual_path: &str, mut reader: R) -> io::Result<TempPlaintext> {
    // 生成唯一临时文件名：目录+文件名
    let path = preferred_temp_dir().join(unique_temp_name(virtual_path));

    // 建一个"仅当前用户可读写"的新文件，再把解密流拷进去
    let mut out = create_private_file(&path)?;
    io::copy(&mut reader, &mut out)?; // 边解边写，内存里只过一小段

    Ok(TempPlaintext { path })
}

/// 选临时目录：优先 RAM 盘（Linux `/dev/shm`），否则系统临时目录。
fn preferred_temp_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        // Linux 下优先 RAM 盘 /dev/shm
        let shm = Path::new("/dev/shm");
        if shm.is_dir() {
            // RAM 盘存在，返回它
            return shm.to_path_buf();
        }
    }
    // macOS/Windows 默认无 tmpfs，退回系统临时目录（磁盘）
    std::env::temp_dir()
}

/// 造一个"仅当前用户可读写"的新文件（Unix 0600）。用 `create_new` 拒绝复用已存在文件。
fn create_private_file(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true) // 文件已存在则报错，避免抢占/覆盖
            .mode(0o600) // 仅当前用户读写
            .open(path) // 打开文件
    }
    #[cfg(not(unix))]
    {
        std::fs::OpenOptions::new()
            .write(true) // 只写，不读
            .create_new(true) // 文件已存在则报错，避免抢占/覆盖
            .open(path) // 打开文件
    }
}

/// 进程内唯一的临时文件名，保留原扩展名（播放器靠扩展名/MIME 选解码器）。
fn unique_temp_name(virtual_path: &str) -> String {
    // 进程内共享的计数器
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    // 取当前值，同时+1（原子操作）同一进程内的多次调用会返回不同的值
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    // 从原路径取扩展名
    // "photos/a.mp4" → "mp4"
    let ext = Path::new(virtual_path)
        .extension()// 可能没扩展名
        .and_then(|e| e.to_str())// 有的话再试着转成 &str
        .unwrap_or("bin");// 没扩展名就用 "bin"
    // 拼成 "veil_<进程号pid>_<计数器n>.<扩展名ext>"
    // veil_48213_0.mp4、veil_48213_1.mkv
    // std::process::id() 进程号	区分不同进程/实例
    format!("veil_{}_{}.{}", std::process::id(), n, ext)
}
