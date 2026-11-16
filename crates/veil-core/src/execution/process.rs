//! 进程实例检查；权限不足、平台不支持时保持未知，禁止按 PID 猜测持有者已退出。
//!
//! 所属业务模块：veil-core 执行身份层的操作系统适配。本模块回答"某个进程实例
//! 是否还活着"：跨进程查询文件锁持有者时，先确认对方进程实例，避免把 PID 复用
//! 的陌生进程当成原持有者。
//!
//! 文件内核心清单：
//! - `Observation`：一次进程观察结果——存在（带系统实例标识）/已退出/未知。
//! - `current`：采集本进程的 `ProcessIdentity`。
//! - `check`：把期望身份与实际观察比对，输出存活三态。
//! - `observe`（按平台实现）：Linux/Android 读 /proc，macOS 用 proc_pidinfo，
//!   Windows 用 OpenProcess，其余平台一律 Unknown。
//! - `parse_linux_stat`：解析 /proc/{pid}/stat 的状态与启动时间字段。
//!
//! 依赖能力：
//! - 各平台系统接口：/proc 文件系统、libc proc_pidinfo/sysctl、kernel32 API。
//!
//! 整体业务目标：只有"确认原实例已退出"才返回 Dead（启动标识不一致也算）；
//! 缺少信息、权限不足、平台不支持一律 Unknown，由租约机制兜底。

use super::{Liveness, ProcessIdentity};
use std::io;

/// 一次进程观察的结果。
pub(super) enum Observation {
    /// 进程存在；携带操作系统提供的进程实例标识（可能为 None，如接口不支持）。
    Present(Option<String>),
    /// 确认进程已退出。
    Dead,
    /// 无法判断（权限不足、平台不支持、信息缺失）。
    Unknown,
}

/// 功能：为当前执行运行时采集本进程身份，供其他进程判断记录的执行者是否仍为原实例。
/// 入参：无；从操作系统读取当前 PID，并生成本次运行时的随机实例标记。
/// 返回值：Ok(ProcessIdentity)，含 pid、16 字节随机值编码成的 instance_id，以及可选系统启动身份。
/// 异常：随机源失败转为 io::Error；无法观察系统身份不使采集失败，而是保存 None。
/// 注意事项：重复调用会生成不同随机标记，调用方应在运行时初始化时采集并复用；系统标记缺失会让跨进程判断更保守。
pub(super) fn current() -> io::Result<ProcessIdentity> {
    let mut random = [0; 16];
    getrandom::getrandom(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
    let pid = std::process::id();
    // 系统标识获取失败不影响启动：置 None，检查时保守按 Unknown 处理
    let system_identity = match observe(pid) {
        Observation::Present(identity) => identity,
        _ => None,
    };
    Ok(ProcessIdentity {
        pid,
        instance_id: hex::encode(random),
        system_identity,
    })
}

/// 功能：将租约记录的进程身份与当前系统观察比对，区分原执行者仍活着、已退出或无法判断。
/// 入参：expected: &ProcessIdentity 为原持有者保存的 PID、随机实例标记和可选系统实例身份。
/// 返回值：Liveness；PID 为零或随机标记为空为 Unknown；确认不存在或系统身份不一致为 Dead；
/// 进程存在且双方系统身份完全相同为 Alive；缺少标记、无权限或无法观察为 Unknown。
/// 异常：不以 Result 返回系统错误，观察失败降级为 Unknown。
/// 注意事项：随机 instance_id 在这里仅检查非空，不靠操作系统核对其内容；不能用 PID 存在单独证明同一实例。
/// Alive 是本次瞬时观察，不延长数据库租约，也不保证对应线程或业务任务仍在执行。
pub(super) fn check(expected: &ProcessIdentity) -> Liveness {
    // 没有有效 PID 或实例 ID 的记录不可信：按 Unknown 处理
    if expected.pid == 0 || expected.instance_id.is_empty() {
        return Liveness::Unknown;
    }
    match observe(expected.pid) {
        Observation::Dead => Liveness::Dead,
        Observation::Present(Some(actual)) => match &expected.system_identity {
            // 期望带标识：与实际完全一致才 Alive，不一致说明实例已换（PID 复用）
            Some(identity) if *identity == actual => Liveness::Alive,
            Some(_) => Liveness::Dead,
            // 期望没保存标识：无法确认是否同一实例，保守 Unknown
            None => Liveness::Unknown,
        },
        _ => Liveness::Unknown,
    }
}

/// Linux/Android：读 /proc/{pid}/stat 判断进程状态与启动时间，并组合启动 ID
/// 作为系统实例标识。
#[cfg(any(target_os = "linux", target_os = "android"))]
/// 功能：在 Linux/Android 上观察目标进程状态和启动身份，辅助租约执行者存活判断。
/// 入参：pid: u32 为目标进程号，通常来自已验证的持有者记录。
/// 返回值：Observation；僵尸/退出状态为 Dead；读到进程状态时为 Present(可选启动标识)；无法可靠观察为 Unknown。
/// 异常：不返回 Result；/proc 读取、格式或 boot_id 缺失均转为保守状态。
/// 注意事项：Linux 在 stat 不存在时还以信号 0 的 ESRCH 确认不存在；Android 不把沙箱隐藏项直接判死。
/// 系统实例身份拼接启动 ID 和进程启动刻度，防止系统重启或 PID 复用被当成原实例。
fn observe(pid: u32) -> Observation {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) => {
            // Android 的沙箱可能隐藏其他进程；缺少 /proc 项不能证明其已退出。
            // Linux 上进一步用信号 0 探测：ESRCH 才敢判 Dead。
            #[cfg(target_os = "linux")]
            if error.kind() == io::ErrorKind::NotFound && pid <= i32::MAX as u32 {
                // SAFETY: 信号 0 只检查状态，不向目标投递信号。
                if unsafe { libc::kill(pid as i32, 0) } == -1
                    && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    return Observation::Dead;
                }
            }
            let _ = error;
            return Observation::Unknown;
        }
    };
    let Some((state, started)) = parse_linux_stat(&stat) else {
        return Observation::Unknown;
    };
    // 僵尸或已退出状态：判 Dead
    if matches!(state, "Z" | "X" | "x") {
        return Observation::Dead;
    }
    // 系统实例标识 = "linux:启动ID:进程启动时间"；启动 ID 用于识别系统重启，
    // 启动时间用于识别同一次启动内的不同进程（PID 复用）。
    let identity = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .filter(|boot| !boot.trim().is_empty())
        .map(|boot| format!("linux:{}:{started}", boot.trim()));
    Observation::Present(identity)
}

/// 解析 /proc/{pid}/stat：返回 (状态字符, 启动时间)。
///
/// 入参：`stat` 一行 /proc 内容。
/// 返回值：`Some((状态, 启动时间刻度))`；格式异常返回 None（观察者按 Unknown 处理）。
#[cfg(any(target_os = "linux", target_os = "android", test))]
/// 功能：从 Linux stat 文本取出状态和启动刻度，用于识别僵尸及 PID 复用。
/// 入参：stat: &str 为 /proc/<pid>/stat 的一行内容；进程名可包含空格和括号。
/// 返回值：Some((&str,u64)) 为最后一个右括号后的状态字段借用及第 22 字段启动刻度；缺字段或数字无法解析为 None。
/// 异常：不返回 Result，解析失败以 None 表达。
/// 注意事项：只抽取所需字段，不完整校验 PID、括号配对或状态取值；起始刻度不是 Unix 时间，须结合系统启动身份。
fn parse_linux_stat(stat: &str) -> Option<(&str, u64)> {
    // comm 可包含空格及括号，必须从最后一个右括号后开始解析字段 3。
    // 括号后按空格切分：第 1 段是字段 3（状态），第 20 段（索引 19）是字段 22（启动时间）。
    let fields: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    Some((*fields.first()?, fields.get(19)?.parse().ok()?))
}

/// macOS：用 proc_pidinfo 取进程 BSD 信息（状态 + 启动时间），用 sysctl 取
/// 系统启动会话 UUID，组合成系统实例标识。
#[cfg(target_os = "macos")]
/// 功能：在 macOS 查询目标进程的状态、创建时间和系统启动会话，识别原执行者实例。
/// 入参：pid: u32 为目标进程号；超出系统有符号 PID 范围时直接 Unknown。
/// 返回值：僵尸或信号 0 确认不存在为 Dead；成功读取 BSD 信息为 Present(可选实例标识)；其余 Unknown。
/// 异常：系统调用失败不抛业务错误；无法读取启动会话 UUID 时只缺失标识，不把现存进程判死。
/// 注意事项：只有系统完整写入 proc_bsdinfo 后才读取结构；身份包含启动会话 UUID、创建秒与微秒。
/// 瞬时观察可能随进程退出改变，最终是否回收租约仍由上层事务验证。
fn observe(pid: u32) -> Observation {
    if pid > i32::MAX as u32 {
        return Observation::Unknown;
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    // SAFETY: 缓冲区大小与请求的系统结构一致，成功后才读取结构。
    let count = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            std::mem::size_of::<libc::proc_bsdinfo>() as i32,
        )
    };
    // 取信息失败：用信号 0 探测。权限错误不会被解释为死亡。
    if count != std::mem::size_of::<libc::proc_bsdinfo>() as i32 {
        // SAFETY: 信号 0 不投递信号；权限错误不会被解释为死亡。
        return if unsafe { libc::kill(pid as i32, 0) } == -1
            && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            Observation::Dead
        } else {
            Observation::Unknown
        };
    }
    // SAFETY: 上面确认系统已写入完整结构。
    let info = unsafe { info.assume_init() };
    // 僵尸进程：判 Dead
    if info.pbi_status == libc::SZOMB {
        return Observation::Dead;
    }
    let mut boot = [0u8; 128];
    let mut length = boot.len();
    // SAFETY: 提供了有效的只读键名、输出缓冲区及其大小，不修改系统配置。
    let result = unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            boot.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    // 标识 = "macos:启动会话UUID:启动秒:启动微秒"；sysctl 失败时返回 Present(None)
    let identity = (result == 0 && length > 1 && length <= boot.len()).then(|| {
        format!(
            "macos:{}:{}:{}",
            String::from_utf8_lossy(&boot[..length]).trim_end_matches('\0'),
            info.pbi_start_tvsec,
            info.pbi_start_tvusec
        )
    });
    Observation::Present(identity)
}

/// Windows：OpenProcess 取进程句柄，等待 0 毫秒判断是否已退出，再取创建时间
/// 作为系统实例标识。
#[cfg(windows)]
/// 功能：在 Windows 通过进程句柄与创建时间判断目标执行者是否仍是原进程。
/// 入参：pid: u32 为目标进程号，调用时只申请查询和等待权限。
/// 返回值：已退出或 OpenProcess 返回参数无效错误为 Dead；成功读取创建时间为 Present(Some(identity))；其他失败为 Unknown。
/// 异常：权限、句柄查询等系统失败以 Observation 表达，不返回 Result。
/// 注意事项：WaitForSingleObject 使用零毫秒只观察，不等待进程结束；成功打开的句柄在本次路径结束前关闭一次。
/// 系统身份取创建时间高/低位，不以 PID 相等推断实例相同；关闭句柄失败未在此额外报告。
fn observe(pid: u32) -> Observation {
    use std::ffi::c_void;
    /// Win32 时间输出结构：两个 32 位字段组成一个系统 64 位时间值。
    /// 用于接收进程创建与 CPU 时间，本模块只组合创建时间生成实例标记，不存业务数据。
    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        /// 64 位时间值的低 32 位，由 GetProcessTimes 写入。
        low: u32,
        /// 64 位时间值的高 32 位，与 low 一起区分同一 PID 的不同进程实例。
        high: u32,
    }
    // 声明本函数用到的 kernel32 API。
    #[link(name = "kernel32")]
    unsafe extern "system" {
        /// 功能：取得目标执行者的 Windows 进程句柄，以查询其是否仍存活。
        /// 入参：access 为请求的进程权限位；inherit 为是否允许继承句柄；pid 为目标进程号。
        /// 返回值：成功返回进程句柄，失败返回空指针。
        /// 异常：失败原因由调用方读取系统错误码。
        /// 注意事项：unsafe 外部接口；成功句柄必须关闭，PID 本身不能证明仍是原来的进程实例。
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        /// 功能：释放存活探测临时打开的 Windows 进程句柄。
        /// 入参：handle 为本流程拥有且尚未关闭的有效句柄。
        /// 返回值：非零表示关闭成功，零表示失败。
        /// 异常：不返回 Rust 异常，失败可读取系统错误码。
        /// 注意事项：unsafe 外部接口；不能重复关闭或关闭借用而未取得所有权的句柄。
        fn CloseHandle(handle: *mut c_void) -> i32;
        /// 功能：检查目标进程句柄是否已进入终止状态，供存活探测使用。
        /// 入参：handle: *mut c_void 为有效进程句柄；milliseconds: u32 为等待毫秒数，本模块传 0 仅查询。
        /// 返回值：u32 系统等待码；本模块把 0（已信号）视作进程已结束，其余值再尝试读取创建时间。
        /// 异常：失败通过系统返回码表示，不产生 Rust Result。
        /// 注意事项：unsafe 接口，调用期间句柄必须有效；非零返回不能简单等同于“进程一定活着”。
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        /// 功能：读取进程时间信息，以创建时间为原进程实例建立可比对标记。
        /// 入参：handle: *mut c_void 为有效进程句柄；created/exited/kernel/user: *mut FileTime 为可写输出缓冲，
        /// 分别承接创建时间、退出时间、内核 CPU 时间和用户 CPU 时间，本模块仅用创建时间构造身份。
        /// 返回值：非零为成功，零为失败。
        /// 异常：系统失败以零值报告，不返回 Rust 错误。
        /// 注意事项：unsafe 接口，所有输出指针须有效且可写；仅在成功后使用输出，不把 CPU 时间误当成业务任务耗时。
        fn GetProcessTimes(
            handle: *mut c_void,
            created: *mut FileTime,
            exited: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
    }
    // SAFETY: 仅请求查询与等待权限，句柄在下面统一关闭。
    let handle = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
    if handle.is_null() {
        // 错误码 87 = 参数无效（进程不存在）；其他错误（如权限不足）按 Unknown
        return if io::Error::last_os_error().raw_os_error() == Some(87) {
            Observation::Dead
        } else {
            Observation::Unknown
        };
    }
    let mut created = FileTime::default();
    let mut exited = FileTime::default();
    let mut kernel = FileTime::default();
    let mut user = FileTime::default();
    // SAFETY: 句柄有效，所有输出指针指向已分配的结构，等待时间为 0。
    let observation = unsafe {
        if WaitForSingleObject(handle, 0) == 0 {
            // 等待 0 毫秒即返回已信号：进程已退出
            Observation::Dead
        } else if GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) != 0 {
            // 用创建时间作为实例标识（同一 PID 复用后创建时间会变）
            Observation::Present(Some(format!("windows:{}:{}", created.high, created.low)))
        } else {
            Observation::Unknown
        }
    };
    // SAFETY: 句柄由本函数打开，且只关闭一次。
    unsafe { CloseHandle(handle) };
    observation
}

// iOS 等平台不假定能查询其他沙箱进程；本进程及远程任务可通过登记服务确认。
/// 其他平台（如 iOS）：沙箱内无法观察其他进程，一律 Unknown。
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    windows
)))]
/// 功能：在缺少受支持进程观察接口的平台保持保守结论，避免误回收活动执行者的租约。
/// 入参：_pid: u32 为待查询进程号，本平台不读取它。
/// 返回值：始终为 Observation::Unknown。
/// 异常：不执行系统查询，没有主动错误返回。
/// 注意事项：Unknown 不是 Dead，也不是 Alive；本进程任务仍可由执行运行时或探测端点提供更具体状态。
fn observe(_pid: u32) -> Observation {
    Observation::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 /proc stat 解析能处理进程名中的空格与括号，并正确取出状态与启动时间。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn linux_stat_handles_spaces_and_parentheses() {
        // 构造字段 3（状态 S）+ 字段 4..21 为 0 + 字段 22（启动时间 98765）
        let mut values = vec!["S".to_owned()];
        values.extend((4..=21).map(|_| "0".to_owned()));
        values.push("98765".to_owned());
        // 进程名带空格和括号：解析必须从最后一个 ')' 之后开始
        let stat = format!("123 (worker (a) b) {}", values.join(" "));
        assert_eq!(parse_linux_stat(&stat), Some(("S", 98765)));
        // 残缺输入：解析失败返回 None（观察者按 Unknown 处理）
        assert!(parse_linux_stat("invalid").is_none());
    }
}
