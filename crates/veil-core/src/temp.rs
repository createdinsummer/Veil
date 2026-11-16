//! 临时明文文件的 RAII 守卫。
//!
//! 需要把解密内容交给外部程序读取时，本模块负责将其写入受控临时位置，并通过
//! `TempPlaintext` 在作用域结束时自动清理。文件优先落在内存盘，权限按当前平台
//! 能力收紧；删除属于 best-effort，不能承诺在 SSD 等介质上物理擦除。
//!
//! 设计上只暴露文件路径，不暴露内部清理策略，避免调用方绕过生命周期管理。
//!
//! # 业务定位
//!
//! 容器内文件默认以密文存在。当需要"打开给外部程序看"（图片查看器、播放器等
//! 无法直接读密文流）时，由本模块把明文写入临时文件交给外部程序，外部读取
//! 结束后守卫析构自动删除。核心风险是明文泄露，因此：
//! - 文件名由进程号、计数器和原扩展名组成，不含原文件名，但不是随机名称。
//! - 目录优先选内存盘（Linux /dev/shm），降低明文写入持久化介质的概率。
//! - 文件权限尽量收紧（Unix 0600，仅当前用户读写）。
//! - 生命周期用 RAII（资源获取即初始化）绑定：守卫被丢弃时尝试删除；
//!   清理失败或进程异常终止时可能残留明文。

//!
//! 核心清单：TempPlaintext 绑定明文路径与清理生命周期，decrypt_to_temp 复制已由调用方解密的输入流，
//! create_private_file / write_private_file 系列控制新建与替换，preferred_temp_dir / unique_temp_name 选择位置和名称。
//! 依赖：标准文件/流接口、原子计数器和 fsutil 原子写入。名称不含原文件名，但不保证随机或物理擦除。
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// 临时明文文件的所有权守卫，销毁时自动删除对应文件。
///
/// 调用方可将 [`path`](Self::path) 交给外部程序，但必须保持该值存活到外部读取结束。
///
/// 业务定位：把"临时明文的生命周期"与守卫对象的生命周期绑定；外部程序读完
/// （调用方 drop 守卫）后尝试删除文件；删除失败会记录告警，仍可能残留明文。
pub struct TempPlaintext {
    /// 临时文件的绝对或平台可用路径。
    /// 业务含义：交给外部程序的唯一入口；不暴露创建/清理策略。
    path: PathBuf,
}

impl TempPlaintext {
    /// 返回临时文件路径。
    ///
    /// 功能：把路径交给外部程序（如系统播放器）读取。
    /// 入参：self: &TempPlaintext 为持有临时明文的守卫。
    /// 返回值：&Path 借用当前临时文件路径。
    /// 异常：不读文件、不返回错误。
    /// 注意事项：路径引用不能超过守卫生命周期；外部程序使用期间调用方须保持守卫存活。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempPlaintext {
    /// 删除临时明文文件；清理失败不会在析构过程中传播 panic。
    ///
    /// 功能：守卫销毁时删除明文文件；找不到文件（已被外部程序移走等）不视为
    /// 失败，其他失败仅记录告警日志，不 panic、不向上传播。
    /// 入参：self: &mut TempPlaintext 为即将析构的临时明文守卫。
    /// 返回值：无；成功或文件已不存在都正常结束。
    /// 异常：remove_file 失败仅记录告警，不以 Result 返回；未删除的明文可能继续残留。
    /// 注意事项：只删除目录项，不承诺物理擦除或等候外部程序结束；进程异常终止也可能绕过析构。
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            // 清理失败只做告警：析构器里不能抛错；残留在临时目录的明文
            // 可能继续残留，不能依赖系统自动清理，也不能承诺物理擦除。
            crate::logging::failure(
                crate::logging::Level::Warn,
                "storage",
                "cleanup_plaintext",
                "PLAINTEXT_CLEANUP_FAILED",
                &error,
                &[("temporary_file", crate::logging::Field::Path(&self.path))],
            );
        }
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
///
/// # 注意事项
/// 明文内容不经过数据库；复制失败时守卫同样被丢弃，并尽力删除部分明文。
pub fn decrypt_to_temp<R: Read>(virtual_path: &str, mut reader: R) -> io::Result<TempPlaintext> {
    // 文件名只决定扩展名和唯一性，明文内容由调用方流式传入。
    let path = preferred_temp_dir().join(unique_temp_name(virtual_path));

    // create_new 防止覆盖已有文件；Unix 模式位进一步限制为仅当前用户读写。
    let mut out = create_private_file(&path)?;
    // 先建立守卫再复制：即使复制中途失败，守卫析构也会尝试删除半截明文。
    let temporary = TempPlaintext { path };
    let copied = io::copy(&mut reader, &mut out);
    // 显式关闭文件句柄，保证后续删除不受句柄占用影响（Windows 尤其需要）。
    drop(out);
    // 复制失败向上返回 Err；守卫随栈展开被丢弃并清理。
    copied?;

    Ok(temporary)
}

/// 选择临时目录：Linux 上优先 `/dev/shm`，其余平台使用系统临时目录。
///
/// 功能：Linux 优先选择通常挂载为 tmpfs 的 /dev/shm，降低明文写入持久化介质的概率；
/// 该目录不存在时退回系统临时目录，本函数并不实际识别挂载类型。
/// 入参：无。
/// 返回值：PathBuf；Linux 的 /dev/shm 存在且为目录时优先使用，否则返回系统临时目录。
/// 异常：不返回 Result，不检测选中目录是否可写或空间是否足够。
/// 注意事项：只检查目录形状，不验证挂载类型；内存文件系统也可能使用交换空间，不能承诺明文绝不落盘。
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
///
/// 功能：用 `create_new`（文件已存在即失败）防符号链接/旧文件攻击，
/// Unix 上同时用 0600 模式位限制访问。
/// 入参：path: &Path 为尚不存在的临时明文文件路径，父目录应已存在。
/// 返回值：Ok(File) 为新建的可写句柄；不自动创建父目录。
/// 注意事项：Unix 在创建时请求 0600 权限，其他平台依赖默认访问控制；create_new 只保护最终路径不覆盖。
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
///
/// 功能：适合一次性小数据（如导出副本）的私有落盘。
///
/// 入参：`path` 是目标路径；`data` 是完整内容。
///
/// 错误：创建或写入失败时返回 [`io::Error`]。
/// 返回值：创建、写入及文件 sync_all 均成功时为 Ok(())。
/// 注意事项：path: &Path 不得已存在；data: &[u8] 是完整内容；失败可能留下部分目标，本函数没有清理守卫。
/// 文件同步成功不等于父目录项已经同步，也不保证所有硬件故障下的数据持久性。
pub fn write_private_file(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut file = create_private_file(path)?;
    file.write_all(data)?;
    // 请求系统同步文件内容；实际持久化保证取决于操作系统、文件系统与设备。
    file.sync_all()?;
    Ok(())
}

/// 以私有临时文件加原子替换的方式写入完整内容。
///
/// 与 [`write_private_file`] 不同，本函数不会直接创建最终目标；进程在写入过程中
/// 中断时，目标路径只会保留旧版本，不会看到半截新内容。
///
/// 功能：先写同目录临时文件再原子重命名，防止崩溃留下半截文件。
/// 入参：path: &Path 为最终目标；data: &[u8] 为需要完整替换的内容，允许为空。
/// 返回值：Ok(()) 表示已完成替换；父目录同步失败在通用路径中仅记录告警。
/// 异常：创建父目录、临时文件、写入、文件同步或替换失败返回 io::Error。
/// 注意事项：已有目标允许替换；并发写入者没有在此处互斥，最终内容取决于最后成功发布者。
pub fn write_private_file_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    crate::fsutil::atomic_write(path, data)
}

/// 生成进程内唯一的临时文件名，并在可行时保留原扩展名。
///
/// 功能：文件名格式 `veil_<进程号>_<计数器>.<扩展名>`：进程号区分不同实例，
/// 计数器区分同进程并发导出，扩展名帮助外部程序选对解码器。
/// 入参：virtual_path: &str 为容器内逻辑路径，只用它提取扩展名。
/// 返回值：String 类型的 `veil_<PID>``_<计数器>`.<扩展名>；没有可用 UTF-8 扩展名时使用 bin。
/// 异常：不返回 Result，不校验文件名是否已被其他进程预先占用。
/// 注意事项：命名不是随机安全令牌；PID 重用、历史残留或计数器回绕可能重名，创建阶段靠 create_new 拒绝覆盖。
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// 验证解密写入的临时明文过程中读取失败时，部分文件会被自动清理。
    /// 读取测试流中的明文数据；失败注入用于模拟磁盘 / 输入故障后的清理路径。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn reader_failure_removes_partial_plaintext() {
        /// 失败注入测试输入：布尔字段表示是否已经成功读过一次；先提供四字节明文，之后返回读取错误。
        /// 用于确认临时明文创建后发生输入故障时仍会尝试删除部分文件，不用于生产读取。
        struct FailsAfterData(bool);
        impl Read for FailsAfterData {
            /// 功能：注入“先读到部分明文、随后读取失败”的场景，验证临时文件清理。
            /// 入参：self 保存是否已读取过一次；output 为复制器提供的输出缓冲区。
            /// 返回值：首次写入 data 并返回 4；后续返回注入的 I/O 错误。
            /// 异常：第二次及以后返回 io::Error；output 小于 4 字节会因切片越界 panic。
            /// 注意事项：仅供本测试 io::copy 使用，不能作为通用 Read 实现。
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                if self.0 {
                    return Err(io::Error::other("注入读取失败"));
                }
                self.0 = true;
                output[..4].copy_from_slice(b"data");
                Ok(4)
            }
        }
        let matching = || {
            std::fs::read_dir(preferred_temp_dir())
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "veilpartialtest")
                })
                .collect::<std::collections::BTreeSet<_>>()
        };
        let before = matching();
        assert!(decrypt_to_temp("private.veilpartialtest", FailsAfterData(false)).is_err());
        assert_eq!(matching(), before);
    }
}
