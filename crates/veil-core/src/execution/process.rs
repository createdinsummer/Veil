//! 进程实例检查；权限不足、平台不支持时保持未知，禁止按 PID 猜测持有者已退出。

use super::{Liveness, ProcessIdentity};
use std::io;

pub(super) enum Observation {
    Present(Option<String>),
    Dead,
    Unknown,
}

pub(super) fn current() -> io::Result<ProcessIdentity> {
    let mut random = [0; 16];
    getrandom::getrandom(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
    let pid = std::process::id();
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

/// 启动标识不一致意味着原实例已退出，即使其 PID 已被其他进程复用。
pub(super) fn check(expected: &ProcessIdentity) -> Liveness {
    if expected.pid == 0 || expected.instance_id.is_empty() {
        return Liveness::Unknown;
    }
    match observe(expected.pid) {
        Observation::Dead => Liveness::Dead,
        Observation::Present(Some(actual)) => match &expected.system_identity {
            Some(identity) if *identity == actual => Liveness::Alive,
            Some(_) => Liveness::Dead,
            None => Liveness::Unknown,
        },
        _ => Liveness::Unknown,
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn observe(pid: u32) -> Observation {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) => {
            // Android 的沙箱可能隐藏其他进程；缺少 /proc 项不能证明其已退出。
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
    if matches!(state, "Z" | "X" | "x") {
        return Observation::Dead;
    }
    let identity = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .filter(|boot| !boot.trim().is_empty())
        .map(|boot| format!("linux:{}:{started}", boot.trim()));
    Observation::Present(identity)
}

#[cfg(any(target_os = "linux", target_os = "android", test))]
fn parse_linux_stat(stat: &str) -> Option<(&str, u64)> {
    // comm 可包含空格及括号，必须从最后一个右括号后开始解析字段 3。
    let fields: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    Some((*fields.first()?, fields.get(19)?.parse().ok()?))
}

#[cfg(target_os = "macos")]
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

#[cfg(windows)]
fn observe(pid: u32) -> Observation {
    use std::ffi::c_void;
    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
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
            Observation::Dead
        } else if GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) != 0 {
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
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    windows
)))]
fn observe(_pid: u32) -> Observation {
    Observation::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_stat_handles_spaces_and_parentheses() {
        let mut values = vec!["S".to_owned()];
        values.extend((4..=21).map(|_| "0".to_owned()));
        values.push("98765".to_owned());
        let stat = format!("123 (worker (a) b) {}", values.join(" "));
        assert_eq!(parse_linux_stat(&stat), Some(("S", 98765)));
        assert!(parse_linux_stat("invalid").is_none());
    }
}
