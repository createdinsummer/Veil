//! 文件系统持久化辅助函数。
//!
//! 本模块只处理崩溃一致性所需的底层动作：创建目录后同步父目录、先写临时文件再
//! 原子替换目标文件，以及同步目录项。加密格式和业务事务仍由各模块负责。
//!
//! 所属业务模块：veil-core 的文件系统工具层。上层（file_ops 的 .enc 发布、
//! 容器的元数据/配置文件落盘、容器目录移动）都依赖这里的"原子发布"和"持久化
//! 顺序"：先写入并同步临时文件，再重命名并同步目录项，以降低崩溃后出现
//! 不完整数据的风险。实际持久化能力仍取决于操作系统、文件系统及存储设备。
//!
//! 文件内核心清单：
//! - `sync_file`：macOS 用 F_FULLFSYNC 请求设备刷新，其他平台 fsync；不支持时退回。
//! - `sync_directory`/`sync_parent`：同步目录项，让重命名/创建/删除持久化。
//! - `create_dir_all_durable`：创建缺失目录树并逐层请求同步父目录，降低崩溃丢失目录项的风险。
//! - `atomic_write`/`atomic_write_metadata`：临时文件 + 原子替换的通用落盘入口。
//! - `is_internal_temp_name`：识别本项目临时文件命名（供清理器使用）。
//! - `rename_no_replace`/`publish_staged_file`/`copy_file_new`：排他重命名与
//!   "禁止覆盖"的文件复制，用于密文发布与容器目录移动。
//! - `fault_point`/`simulate_fullfsync_unsupported`：故障注入（仅测试特性）。
//!
//! 依赖能力：`tempfile`（同目录临时文件）、`libc`（fcntl/syscall）、
//! `crate::progress`（可取消复制）、`crate::logging`（发布后同步失败告警）。
//!
//! 整体业务目标：所有"先写临时、再原子发布"的落盘路径共享同一套顺序与回退语义，
//! 尽量让数据库提交前的新密文和元数据文件在断电/崩溃后保持一致。

use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// 在 macOS 上请求设备刷新；其他平台使用系统文件同步。
///
/// F_FULLFSYNC 只在 HFS、FAT、UDF 和 APFS 上实现。SMB 挂载以及 fskit 的 exFAT 等
/// 文件系统会返回 ENOTSUP（部分实现返回 EINVAL），此时退回普通 `sync_all`；
/// 具体同步语义仍取决于系统、文件系统和设备。
///
/// 功能：请求系统同步文件内容。macOS 优先请求 `F_FULLFSYNC`，不支持时回退；
/// 成功返回表示系统调用成功，不单独保证所有硬件故障或断电场景下的数据持久性。
///
/// 入参：`file` 已打开的目标文件。
/// 返回值：`Ok(())` 刷新完成；`Err` 底层同步失败。
///
/// 注意事项：被信号中断（EINTR）会重试；ENOTSUP/EINVAL 退回 `sync_all`。
/// 异常：除 macOS 的受支持回退码与 EINTR 重试外，底层刷新错误原样返回 io::Error。
/// 注意事项：file: &File 应是仍有效的目标句柄；调用会阻塞，不包含父目录同步。
pub fn sync_file(file: &fs::File) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        loop {
            // SAFETY: 文件描述符来自仍然存活的 File，F_FULLFSYNC 不读取额外参数。
            let success = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } == 0;
            #[cfg(any(test, feature = "fault-injection"))]
            let success = success && !FULLFSYNC_UNSUPPORTED.load(std::sync::atomic::Ordering::Relaxed);
            if success {
                return Ok(());
            }
            // 测试注入"不支持"时伪造 ENOTSUP 错误，验证退回路径
            #[cfg(any(test, feature = "fault-injection"))]
            let error = if FULLFSYNC_UNSUPPORTED.load(std::sync::atomic::Ordering::Relaxed) {
                io::Error::from_raw_os_error(libc::ENOTSUP)
            } else {
                io::Error::last_os_error()
            };
            #[cfg(not(any(test, feature = "fault-injection")))]
            let error = io::Error::last_os_error();
            // 被信号打断：循环重试
            if error.kind() != io::ErrorKind::Interrupted {
                // 文件系统不支持设备级刷新请求：退回 sync_all，请求系统同步文件内容和元数据。
                if error.raw_os_error() == Some(libc::ENOTSUP)
                    || error.raw_os_error() == Some(libc::EINVAL)
                {
                    return file.sync_all();
                }
                return Err(error);
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    file.sync_all()
}

/// 区分替换前失败和替换后持久化状态不确定。
///
/// 业务定位：原子写入的两种失败：
/// - `published=false`：目标文件仍是旧内容（或不存在），可安全重试。
/// - `published=true`：重命名已完成，新内容已经生效，只是目录同步失败——
///   绝不能当成"没写成"而删除新数据。
pub struct AtomicWriteError {
    /// 底层 I/O 错误。
    pub error: io::Error,
    /// 目标是否已被新内容替换：true 表示新内容已生效。
    pub published: bool,
}

/// 故障注入点：进程在指定阶段直接退出（86）或暂停等待外部放行。
///
/// 仅供测试/故障注入特性使用；通过环境变量 `VEIL_TEST_FAILPOINT`（注入崩溃）
/// 或 `VEIL_TEST_PAUSEPOINT`（暂停）选择触发阶段。
#[cfg(any(test, feature = "fault-injection"))]
#[doc(hidden)]
/// 入参：stage: &str 为当前阶段名，须精确匹配 VEIL_TEST_FAILPOINT 或 VEIL_TEST_PAUSEPOINT。
/// 返回值：未匹配时立即返回；暂停模式在 resume 文件出现后返回；崩溃模式不返回。
/// 异常：暂停目录环境变量缺失、标记写入失败或暂停超过 20 秒时 panic；崩溃模式直接 exit(86)。
/// 注意事项：仅测试/故障注入构建存在；exit 不执行正常析构，外部测试进程负责隔离与清理。
pub fn fault_point(stage: &str) {
    if std::env::var("VEIL_TEST_FAILPOINT").as_deref() == Ok(stage) {
        // 模拟进程崩溃：在指定阶段直接退出，验证崩溃一致性
        std::process::exit(86);
    }
    if std::env::var("VEIL_TEST_PAUSEPOINT").as_deref() == Ok(stage) {
        // 模拟暂停：写标记文件后轮询放行文件，供外部进程制造故障
        let root = std::path::PathBuf::from(std::env::var_os("VEIL_FAULT_TEST_ROOT").unwrap());
        fs::write(root.join("paused"), stage).unwrap();
        let start = std::time::Instant::now();
        while !root.join("resume").exists() {
            // 20 秒仍不恢复视为测试卡死，直接失败
            assert!(start.elapsed() < std::time::Duration::from_secs(20));
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

/// 模拟目标文件系统不支持 F_FULLFSYNC（如 fskit 的 exFAT、SMB），供回退路径测试。
#[cfg(any(test, feature = "fault-injection"))]
static FULLFSYNC_UNSUPPORTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 打开/关闭 F_FULLFSYNC 不支持模拟（仅测试特性）。
#[cfg(any(test, feature = "fault-injection"))]
#[doc(hidden)]
/// 入参：unsupported: bool，true 强制走不支持设备刷新的测试回退，false 恢复正常判断。
/// 返回值：无；更新进程共享原子标志。
/// 异常：不返回错误。
/// 注意事项：全局状态可能影响并发测试；使用后应恢复 false，仅在测试/故障注入构建可用。
pub fn simulate_fullfsync_unsupported(unsupported: bool) {
    FULLFSYNC_UNSUPPORTED.store(unsupported, std::sync::atomic::Ordering::Relaxed);
}

/// 返回路径的父目录；没有父目录时使用当前目录。
/// 入参：path: &Path 为待写入或同步的目标路径。
/// 返回值：非空父路径的借用；没有非空父级时返回代表当前目录的 Path。
/// 异常：不访问文件系统，不返回错误。
/// 注意事项：只拆解路径，不创建或规范化目录，也不验证该父级存在。
fn parent_or_current(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// 请求同步目录项，降低重命名、创建和删除在崩溃后丢失的风险。
///
/// Unix 平台直接对目录执行 `fsync`。Windows 标准库没有稳定暴露目录句柄同步接口，
/// 因此该平台保留原子重命名语义，并在本函数中不额外执行目录同步。
///
/// 功能：崩溃一致性链条的最后一步——文件已重命名，但目录项本身若不落盘，
/// 断电后可能"目录里看不到刚发布的文件"或"看到旧名字"。
/// 入参：path: &Path 为需要同步目录项的目录路径。
/// 返回值：Unix 打开并同步成功时为 Ok(())；非 Unix 路径为空操作，也返回 Ok(())。
/// 异常：Unix 打开或同步目录失败返回 io::Error。
/// 注意事项：非 Unix 的成功不代表已执行目录刷新；也不自动同步整个祖先链。
pub fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let directory = fs::File::open(path)?;
        directory.sync_all()?;
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}

/// 同步目标路径所在目录。
/// 入参：path: &Path 为目录项发生变化的目标文件或目录。
/// 返回值：父目录同步结果，无非空父级时同步当前目录。
/// 异常：原样传播 sync_directory 的 io::Error。
/// 注意事项：只同步一层父级，不同步目标文件内容。
pub(crate) fn sync_parent(path: &Path) -> io::Result<()> {
    sync_directory(parent_or_current(path))
}

/// 创建目录树，并在创建每一层后同步其父目录。
///
/// 普通 `create_dir_all` 只保证进程内可见；断电或系统崩溃后，新目录项可能丢失。
/// 本函数从最上层缺失目录开始创建，并请求同步新目录项的父级；已有目录直接返回，
/// 不会重新验证或同步整条路径，实际断电持久性仍取决于平台与存储设备。
///
/// 功能：从最浅缺失层往深逐层 `create_dir` + 同步父目录；目标已是目录时幂等
/// 直接返回。逐层创建（而非一次 create_dir_all）是为了每层都能同步父目录。
///
/// 入参：`path` 目标目录路径。
/// 返回值：`Ok(())` 表示目标已是目录、路径为空，或缺失目录创建及同步请求成功。
/// 异常：创建、目录类型检查或父目录同步失败返回 io::Error；已创建的部分不会自动删除。
/// 注意事项：空路径是无操作；非 Unix 平台目录同步为空操作，不能据成功返回推断断电保证。
pub fn create_dir_all_durable(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() || path.is_dir() {
        return Ok(());
    }

    // missing 收集"从深到浅"所有尚不存在的层级，稍后逆序（从浅到深）创建
    let mut missing = Vec::new();
    let mut current = path.to_path_buf();
    while !current.as_os_str().is_empty() && !current.exists() {
        missing.push(current.clone());
        current.pop();
    }

    // 从浅到深逐层创建：每建一层请求同步其父目录，降低崩溃丢失新目录项的风险
    for directory in missing.iter().rev() {
        match fs::create_dir(directory) {
            Ok(()) => sync_parent(directory)?,
            // 并发下已被他人创建：视为成功，继续下一层
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }

    // 最后复核：目标是目录才算成功；被同名文件挡住则报错
    if path.is_dir() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("路径不是目录: {}", path.display()),
        ))
    }
}

/// 先完整写入同目录临时文件，再原子替换目标路径并同步目录。
///
/// 未受其他写入者影响时，成功返回表示目标已是完整新内容；替换前中断则保留旧目标，
/// 不会把半截内容当成正式文件发布。进程在写入过程中被终止时，残留临时文件使用 `.veil-tmp-` 前缀，
/// 不会遮蔽正式文件。
///
/// 重命名完成后目录同步失败无法再安全回滚：此时正式文件已经是完整的新内容，若
/// 返回错误，调用方可能错误删除新数据。因此提交后的目录同步按尽力而为处理。
///
/// 功能：通用"整文件原子替换"入口——普通调用（strict=false）把目录同步失败按
/// 警告处理；元数据调用（strict=true）把失败作为 `published=true` 的明确错误
/// 上报，让调用方知道"新内容已生效但持久性未确认"。
/// 入参：path: &Path 为可替换目标；data: &[u8] 为完整新内容。
/// 返回值：成功完成重命名后为 Ok(())；目录同步失败仅告警，不撤销已发布内容。
/// 异常：发布前失败返回 io::Error，目标在本流程内仍保留旧内容或不存在。
/// 注意事项：不协调并发写者；需要知道替换后持久性是否确认时使用 atomic_write_metadata。
pub(crate) fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    atomic_write_inner(path, data, false).map_err(|error| error.error)
}

/// 元数据文件的原子写入：返回可区分"替换前失败/替换后持久化不确定"的错误。
/// 入参：path: &Path 为元数据目标；data: &[u8] 为完整内容。
/// 返回值：替换和目录同步均成功时 Ok(())。
/// 异常：返回 AtomicWriteError；published=false 表示本次尚未替换，true 表示已经替换但目录同步未确认。
/// 注意事项：published=true 时不能按“未写入”删除新数据；并发业务写入由调用方协调。
pub fn atomic_write_metadata(path: &Path, data: &[u8]) -> Result<(), AtomicWriteError> {
    atomic_write_inner(path, data, true)
}

/// 原子写入公共实现。
///
/// 顺序：建目录（持久化）→ 写同目录临时文件 → 同步临时文件 → 原子替换 →
/// 同步目录。`strict` 为 true 时把目录同步失败作为 published=true 的错误返回。
/// 入参：path: &Path 为目标；data: &[u8] 为完整数据；strict: bool 决定是否上报发布后的目录同步失败。
/// 返回值：写入和发布完成时 Ok(())；strict=false 允许目录同步失败后告警返回成功。
/// 异常：发布前失败的 published 为 false；strict=true 的发布后目录同步错误为 true。
/// 注意事项：只保证本次替换不暴露半截目标，不构成与数据库事务之间的跨资源原子提交。
fn atomic_write_inner(path: &Path, data: &[u8], strict: bool) -> Result<(), AtomicWriteError> {
    // 替换前失败统一包装为 published=false：目标未被改动
    let before = |error| AtomicWriteError {
        error,
        published: false,
    };
    let parent = parent_or_current(path);
    create_dir_all_durable(parent).map_err(before)?;

    // 临时文件与目标同目录：重命名是原子的，不会跨文件系统
    let mut temp_file = tempfile::Builder::new()
        .prefix(".veil-tmp-")
        .tempfile_in(parent)
        .map_err(before)?;
    temp_file.write_all(data).map_err(before)?;
    #[cfg(any(test, feature = "fault-injection"))]
    if strict {
        fault_point("config_temp_written");
    }
    // 重命名前先把内容刷到磁盘：保证新文件名指向的数据完整
    sync_file(temp_file.as_file()).map_err(before)?;
    #[cfg(any(test, feature = "fault-injection"))]
    if strict {
        fault_point("before_replace");
    }
    // 原子替换目标（替换前的任何失败都保持旧内容）
    temp_file
        .persist(path)
        .map_err(|error| before(error.error))?;
    #[cfg(any(test, feature = "fault-injection"))]
    if strict {
        fault_point("after_replace");
    }
    #[cfg(any(test, feature = "fault-injection"))]
    if strict && std::env::var("VEIL_TEST_FAILPOINT").as_deref() == Ok("after_replace_error") {
        // 注入"替换完成但目录同步失败"：验证 published=true 的错误语义
        return Err(AtomicWriteError {
            error: io::Error::other("注入目录同步失败"),
            published: true,
        });
    }
    let synced = sync_directory(parent);
    if strict {
        // 元数据路径：目录同步失败必须如实上报（新内容已生效，持久性未确认）
        synced.map_err(|error| AtomicWriteError {
            error,
            published: true,
        })?;
        #[cfg(any(test, feature = "fault-injection"))]
        fault_point("after_commit");
    } else if let Err(error) = synced {
        // 普通路径：目录同步失败按尽力而为处理，只记日志不回滚
        crate::logging::failure(
            crate::logging::Level::Warn,
            "storage",
            "sync_published_file",
            "PUBLISHED_DIRECTORY_SYNC_FAILED",
            &error,
            &[
                ("destination", crate::logging::Field::Path(path)),
                ("published", crate::logging::Field::Label("true")),
            ],
        );
    }

    Ok(())
}

/// 判断目录项是否由本项目的临时文件命名规则生成。
///
/// 供清理器使用：只识别这三种已知前缀的残留临时文件，未知文件一律保留。
/// 入参：name: &str 为单个目录项名称。
/// 返回值：带 .veil-tmp-、.veil-pw- 或 .veil-file- 前缀时为 true，否则 false。
/// 异常：不访问文件系统，不返回错误。
/// 注意事项：只识别前缀，不验证文件类型、所有权或是否仍被使用；清理器须另行判断能否删除。
pub fn is_internal_temp_name(name: &str) -> bool {
    name.starts_with(".veil-tmp-")
        || name.starts_with(".veil-pw-")
        || name.starts_with(".veil-file-")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证原子替换后旧内容被完整新内容取代，且不残留临时文件。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn atomic_write_replaces_complete_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("state.bin");
        atomic_write(&path, b"old").unwrap();
        atomic_write(&path, b"new-state").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new-state");
        // 目录里不应残留任何本项目临时文件
        assert!(
            fs::read_dir(dir.path())
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !is_internal_temp_name(&entry.file_name().to_string_lossy()))
        );
    }

    /// 验证目录树创建后目标目录可用。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn create_dir_all_durable_creates_nested_paths() {
        let dir = tempfile::TempDir::new().unwrap();
        let nested = dir.path().join("a").join("b").join("c");
        create_dir_all_durable(&nested).unwrap();
        assert!(nested.is_dir());
    }

    /// 验证 F_FULLFSYNC 不受支持时退回 fsync，导出到 exFAT、SMB 等卷仍可完成。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn fullfsync_unsupported_falls_back_to_fsync() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("fallback");
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(b"content").unwrap();
        // 模拟文件系统不支持 F_FULLFSYNC，同步必须退回 fsync 并成功
        simulate_fullfsync_unsupported(true);
        sync_file(&file).unwrap();
        simulate_fullfsync_unsupported(false);
        assert_eq!(fs::read(&path).unwrap(), b"content");
    }
}

/// 原子移动且拒绝覆盖目标，适用于密文发布和容器目录移动。
///
/// 功能：排他重命名——目标已存在（文件或目录）时失败，源保持不动。macOS 用
/// renameatx_np(RENAME_EXCL)，Linux/Android 用 renameat2(RENAME_NOREPLACE)，
/// Windows 用 MoveFileW（本身不覆盖目标）。
///
/// 入参：`source` 源路径；`destination` 目标路径（必须尚不存在）。
/// 返回值：`Ok(())` 移动成功；目标已存在返回 AlreadyExists；平台不支持返回
/// Unsupported；路径含 NUL 返回 InvalidInput。
///
/// 注意事项：同文件系统内是原子的；跨文件系统由操作系统拒绝，调用方回退到
/// "排他创建 + 复制"路径（见 publish_staged_file）。
/// 异常：目标已存在、源缺失、权限不足、跨文件系统或系统接口失败时返回 io::Error。
/// 注意事项：这里只移动目录项，不额外同步文件或父目录；跨文件系统错误本身不会在此自动复制。
pub fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        // 路径转 C 字符串：含 NUL 字节直接拒绝
        let source = std::ffi::CString::new(source.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "路径包含 NUL"))?;
        let destination = std::ffi::CString::new(destination.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "路径包含 NUL"))?;
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        // SAFETY: 两个 C 字符串在调用期间有效；RENAME_EXCL 禁止替换目标。
        let result = unsafe {
            libc::renameatx_np(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(any(target_os = "linux", target_os = "android"))]
        // SAFETY: syscall 仅在目标平台定义；参数来自仍有效的路径字符串。
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        // 其他 unix 平台（如 iOS 之外的 BSD）：暂未实现排他重命名
        #[cfg(not(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "linux",
            target_os = "android"
        )))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "此平台尚未实现排他重命名",
        ));
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "linux",
            target_os = "android"
        ))]
        return if result == 0 {
            // 返回值 0 表示重命名成功
            Ok(())
        } else {
            // 失败原因交给系统错误码（目标已存在时为 EEXIST）
            Err(io::Error::last_os_error())
        };
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            // MoveFileW 本身不覆盖已存在的目标文件
            /// 功能：调用 Windows 文件移动接口，在禁止覆盖的发布流程中移动文件或目录。
            /// 入参：source/destination 为有效的 NUL 结尾 UTF-16 路径指针。
            /// 返回值：非零表示成功；零表示失败，可读取 last_os_error。
            /// 异常：不抛 Rust 异常，通过零值报告系统失败。
            /// 注意事项：unsafe 外部接口；两指针须在调用期间有效，目标已存在时拒绝覆盖。
            fn MoveFileW(source: *const u16, destination: *const u16) -> i32;
        }
        // 路径转 NUL 结尾的 UTF-16 宽字符序列
        let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        // 检查原始路径中的 NUL（encode_wide 后 NUL 会提前截断语义）
        if source[..source.len() - 1].contains(&0)
            || destination[..destination.len() - 1].contains(&0)
        {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "路径包含 NUL"));
        }
        // SAFETY: UTF-16 路径以 NUL 结束，调用期间不会释放；MoveFileW 不覆盖目标。
        return if unsafe { MoveFileW(source.as_ptr(), destination.as_ptr()) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        };
    }
    // 非 unix 非 windows 平台：暂无实现
    #[cfg(not(any(unix, windows)))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "此平台尚未实现排他重命名",
    ))
}

/// 发布尚未被元数据引用的密文；不支持排他重命名时先排他创建再复制。
/// 调用方必须在本函数成功后才提交引用，且失败后的残留文件可安全清理。
///
/// 功能：把暂存区的完整密文发布到正式路径（如 `<lock_id>_<uuid>.enc`）：
/// 优先排他重命名；平台/文件系统不支持（Unsupported/ENOTSUP）时退回
/// "排他创建目标 + 复制 + 删除源"（同样拒绝覆盖）。
///
/// 注意事项：调用方须在数据库提交前隔离目标，保证业务读取不会提前使用复制中的文件。
/// 失败时须核对实际源/目标状态；是否可清理取决于业务引用，而不能只凭返回错误判断。
/// 入参：source/destination: &Path 分别为完整暂存密文和尚不存在的正式路径。
/// 返回值：发布完成返回 Ok(())，重命名或复制路径都不覆盖已有目标。
/// 异常：除 Unsupported/ENOTSUP 触发回退外，其余重命名错误直接返回；复制、同步或删除源失败也返回错误。
/// 注意事项：回退复制成功但删除源失败时，源与完整目标可能同时存在；普通 I/O 失败还可能遗留部分目标，
/// 不能一概把 Err 当作目标不存在，清理与数据库引用提交必须由业务层协调。
pub fn publish_staged_file(source: &Path, destination: &Path) -> io::Result<()> {
    match rename_no_replace(source, destination) {
        Ok(()) => Ok(()),
        // 平台不支持排他重命名：退回排他创建 + 复制
        Err(error) if error.kind() == io::ErrorKind::Unsupported => {
            copy_staged_file(source, destination)
        }
        // 文件系统不支持该重命名标志（如某些网络文件系统）：同样退回复制
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ENOTSUP) => {
            copy_staged_file(source, destination)
        }
        // 其他错误（如目标已存在）原样上报
        Err(error) => Err(error),
    }
}

/// 暂存发布的退回路径：排他创建目标并复制，成功后删除源。
///
/// 复制期间用户取消会删除半截目标，源保持不动。
/// 入参：source/destination: &Path 为暂存源与新目标。
/// 返回值：复制同步成功且源删除成功时为 Ok(())。
/// 异常：复制、同步或源删除失败返回 io::Error；取消时尽力删除部分目标。
/// 注意事项：目标完整后删除源仍可能失败；错误不代表没有发布数据，不能盲目覆盖重试。
fn copy_staged_file(source: &Path, destination: &Path) -> io::Result<()> {
    copy_file_new_impl(source, destination, false, true)?;
    fs::remove_file(source)
}

/// 向全新文件复制密文并持久化，禁止覆盖并保持私有文件权限。
///
/// 功能：容器复制/移动时复制 .enc 密文到新位置：目标必须不存在（create_new），
/// 写入后同步文件；unix 上权限固定为 0600（仅所有者读写）。
/// 入参：source/destination: &Path 为源文件与尚不存在的目标文件。
/// 返回值：复制及文件同步成功返回 Ok(())，源保留。
/// 异常：打开、创建、设置权限、复制或同步失败返回 io::Error。
/// 注意事项：不创建父目录、不同步目录项；非取消错误可能留下部分或完整目标，清理由调用方负责。
pub fn copy_file_new(source: &Path, destination: &Path) -> io::Result<()> {
    copy_file_new_impl(source, destination, false, false)
}
/// 与 `copy_file_new` 相同，但带进度上报。
/// 入参：source/destination: &Path 为源文件与尚不存在的目标文件。
/// 返回值：复制和文件同步成功时 Ok(())，复制过程更新当前进度上下文。
/// 异常：打开、权限、复制、取消或同步错误通过 io::Error 传播。
/// 注意事项：当前进度上下文由调用方建立；取消删除部分目标为尽力而为，不保证所有失败都自动清理。
pub fn copy_file_new_with_progress(source: &Path, destination: &Path) -> io::Result<()> {
    copy_file_new_impl(source, destination, true, false)
}
/// 复制公共实现。
///
/// `progress` 为 true 使用带进度的复制；`cancellable` 为 true 使用可取消复制
/// （取消时删除已创建的半截目标）。
/// 入参：source/destination: &Path 为源和新目标；progress: bool 启用进度复制；cancellable: bool 启用无进度可取消复制。
/// 返回值：复制与 sync_file 成功时为 Ok(())，不删除源。
/// 异常：文件、权限、复制或同步失败返回 io::Error；可识别的取消会先关闭目标句柄再尝试删除目标。
/// 注意事项：progress 优先于 cancellable；Unix 创建后才设置 0600，不能把它等同于创建瞬间即固定私有权限。
fn copy_file_new_impl(
    source: &Path,
    destination: &Path,
    progress: bool,
    cancellable: bool,
) -> io::Result<()> {
    let mut input = fs::File::open(source)?;
    // create_new：目标已存在立即失败，保证绝不覆盖
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // 密文文件只允许所有者读写，防止其他本地用户读取
        output.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    // 按模式选择复制实现：进度/可取消/纯复制
    let copied = if progress {
        crate::progress::copy(&mut input, &mut output)
    } else if cancellable {
        crate::progress::copy_cancellable(&mut input, &mut output)
    } else {
        io::copy(&mut input, &mut output)
    };
    if let Err(error) = copied {
        // 用户取消：清掉刚创建的半截目标，源不受影响
        if crate::progress::is_cancelled_io(&error) {
            drop(output);
            let _ = fs::remove_file(destination);
        }
        return Err(error);
    }
    // 复制完成请求文件同步后再返回；目录项同步与实际断电持久性仍由平台和调用方流程决定。
    sync_file(&output)?;
    Ok(())
}

/// 验证密文暂存发布在取消和目标冲突时保护已有文件与源文件。
#[cfg(test)]
mod exclusive_publish_tests {
    use super::*;
    /// 验证复制中途取消会删除半成品目标，并保留原始源文件。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn cancelled_staged_copy_removes_its_partial_destination() {
        let root = tempfile::TempDir::new().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        fs::write(&source, b"payload").unwrap();
        let progress = crate::progress::Progress::default();
        let _scope = progress.begin("import");
        crate::progress::start(Some(7));
        assert!(progress.request_cancel());
        let error = copy_staged_file(&source, &target).unwrap_err();
        assert!(crate::progress::is_cancelled_io(&error));
        assert!(!target.exists());
        assert_eq!(fs::read(source).unwrap(), b"payload");
    }
    /// 验证复制拒绝覆盖已存在目标；目标移除后复制成功并清理暂存源。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn staged_copy_never_overwrites_existing_file() {
        let root = tempfile::TempDir::new().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        fs::write(&source, b"new").unwrap();
        fs::write(&target, b"existing").unwrap();
        assert_eq!(
            copy_staged_file(&source, &target).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&target).unwrap(), b"existing");
        assert_eq!(fs::read(&source).unwrap(), b"new");
        fs::remove_file(&target).unwrap();
        copy_staged_file(&source, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert!(!source.exists());
    }
    /// 验证排他重命名对已有文件和已有目录均拒绝替换。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn existing_file_and_existing_directory_are_never_replaced() {
        let root = tempfile::TempDir::new().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        fs::write(&source, b"new").unwrap();
        fs::write(&target, b"keep").unwrap();
        assert!(rename_no_replace(&source, &target).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"keep");
        fs::remove_file(&source).unwrap();
        fs::remove_file(&target).unwrap();
        fs::create_dir(&source).unwrap();
        fs::create_dir(&target).unwrap();
        assert!(rename_no_replace(&source, &target).is_err());
        assert!(source.is_dir());
        assert!(target.is_dir());
    }
}
