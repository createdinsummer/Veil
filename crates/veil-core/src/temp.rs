//! 临时明文文件的 RAII 守卫。
//!
//! 需要把解密内容交给外部程序读取时，本模块负责将其写入受控临时位置，并通过
//! [`TempPlaintext`] 在作用域结束时自动清理。文件优先落在内存盘，权限按当前平台
//! 能力收紧；删除属于 best-effort，不能承诺在 SSD 等介质上物理擦除。
//!
//! 设计上只暴露文件路径，不暴露内部清理策略，避免调用方绕过生命周期管理。

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// 临时明文文件的所有权守卫，销毁时自动删除对应文件。
///
/// 调用方可将 [`path`](Self::path) 交给外部程序，但必须保持该值存活到外部读取结束。
pub struct TempPlaintext {
    /// 临时文件的绝对或平台可用路径。
    path: PathBuf,
}

impl TempPlaintext {
    /// 返回临时文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempPlaintext {
    /// 删除临时明文文件；清理失败不会在析构过程中传播 panic。
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// 将明文流式写入受控临时文件并返回清理守卫。
///
/// # 参数
/// - `virtual_path`：容器内路径，仅用于保留扩展名，帮助外部程序识别内容类型。
/// - `reader`：待写入临时文件的明文流。
///
/// # 返回
/// 返回负责删除临时文件的守卫。
///
/// # 错误
/// 创建目标文件或复制字节流失败时返回 [`io::Error`]。
pub fn decrypt_to_temp<R: Read>(virtual_path: &str, mut reader: R) -> io::Result<TempPlaintext> {
    // 文件名只决定扩展名和唯一性，明文内容由调用方流式传入。
    let path = preferred_temp_dir().join(unique_temp_name(virtual_path));

    // create_new 防止覆盖已有文件；Unix 模式位进一步限制为仅当前用户读写。
    let mut out = create_private_file(&path)?;
    io::copy(&mut reader, &mut out)?;

    Ok(TempPlaintext { path })
}

/// 选择临时目录：Linux 上优先 `/dev/shm`，其余平台使用系统临时目录。
fn preferred_temp_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        // /dev/shm 通常是内存盘，能降低明文落到持久化介质的概率。
        let shm = Path::new("/dev/shm");
        if shm.is_dir() {
            return shm.to_path_buf();
        }
    }
    std::env::temp_dir()
}

/// 创建不覆盖既有文件的新文件；Unix 下将权限设置为仅当前用户可读写。
///
/// # 错误
/// 文件已存在、权限设置或创建失败时返回 [`io::Error`]。
pub(crate) fn create_private_file(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Unix 使用 create_new 与 0600，避免复用旧文件和放宽权限。
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    }
    #[cfg(not(unix))]
    {
        // 其他平台至少保证只创建新文件，权限控制依赖系统临时目录默认值。
        OpenOptions::new()
            .write(true) // 只写，不读
            .create_new(true)
            .open(path)
    }
}

/// 以仅当前用户可读写的方式创建新文件并写入完整内容。
///
/// Unix 上使用 `create_new` 和 `0600`，避免覆盖既有文件或继承过宽权限。
/// 其他平台依赖系统临时目录和默认权限语义。
pub fn write_private_file(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut file = create_private_file(path)?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}

/// 生成进程内唯一的临时文件名，并在可行时保留原扩展名。
fn unique_temp_name(virtual_path: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    // 进程号区分不同实例，原子计数器区分同一进程中的并发导出。
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    // 扩展名只影响外部程序选择解码器，不参与安全判断。
    let ext = Path::new(virtual_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin");
    format!("veil_{}_{}.{}", std::process::id(), n, ext)
}
