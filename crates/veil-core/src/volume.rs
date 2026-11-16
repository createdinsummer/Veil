//! 运行时枚举挂载卷、识别路径所属卷；卷信息不写入配置或链接。
//!
//! # 业务定位
//!
//! 卷（volume）是操作系统的挂载点/盘符（如 macOS 的 `/Volumes/xxx`、Windows 的
//! `D:\`）。本模块提供两个运行时能力：枚举当前已挂载的外部卷，以及判断某条
//! 路径属于哪个卷（用于提示"这个容器在外接盘上"等场景）。卷信息是运行时
//! 观测结果，不会持久化到配置或链接文件。
//!
//! # 平台差异
//!
//! - macOS 用 `getfsstat` 系统调用列举挂载点，外部卷 = 位于 `/Volumes` 下。
//! - Linux 解析 `/proc/self/mountinfo`，外部卷 = `/media`、`/mnt`、`/run/media` 下。
//! - Windows 枚举逻辑盘符并查驱动器类型（可移动/固定）。
//! - iOS 只用应用私有目录，不枚举系统或外接卷，一律视为非外部卷。
use crate::error::VeilError;
use std::path::{Path, PathBuf};

/// 当前挂载卷的运行时描述。
///
/// 业务定位：回答"这个卷叫什么、根在哪、是不是外部卷"三个问题，供 UI 提示
/// 或路径归类使用；只是一份观察快照，不承担写入职责。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    /// 卷的展示名称：取挂载根路径的最后一段（如 "MyUSB"）。
    pub volume_name: String,
    /// 卷的挂载根路径（如 `/Volumes/MyUSB`、`D:\`），用于判断路径归属。
    pub root_path: PathBuf,
    /// 是否按平台启发式规则归为外部位置；只作展示提示，不足以判断介质能否安全断开。
    pub if_out_volume: bool,
}

/// 功能：列出当前被项目启发式规则归为“外部卷”的挂载位置，供容器位置选择和展示。
/// 入参：无，按当前编译平台查询挂载表或盘符。
/// 返回值：Ok(`Vec<VolumeInfo>`)，每项是一条符合外部规则的挂载根快照；iOS 为列表为空。
/// 异常：挂载表读取或系统枚举失败时传播 I/O 转换的 VeilError。
/// 注意事项：不扫描目录树、不创建配置；“外部”由路径或驱动器类别推断，不严格证明是物理外接设备。
/// 枚举结果可能随拔插改变，不能据此保证挂载仍可访问或可安全弹出。
pub fn mounted_volumes() -> Result<Vec<VolumeInfo>, VeilError> {
    Ok(mount_roots()?
        .into_iter()
        // 过滤掉非外部卷：只有外接盘才有枚举价值，系统盘/应用目录不列出。
        .filter(|root| is_external_mount(root))
        .map(|root_path| VolumeInfo {
            // 卷名取挂载点最后一段路径（如 "MyUSB"），取不到时退回整个路径文本。
            volume_name: root_path
                .file_name()
                .unwrap_or(root_path.as_os_str())
                .to_string_lossy()
                .into_owned(),
            root_path,
            // 能进入本列表的必然是外部卷。
            if_out_volume: true,
        })
        .collect())
}

/// 功能：把现有或计划创建的容器路径归入当前最具体的挂载卷。
/// 入参：path: &Path 为任意目标位置，可为相对路径、符号链接或尚不存在的后缀。
/// 返回值：Ok(VolumeInfo)，选择解析后最近已存在位置所覆盖的最深挂载根，并生成名称及外部标记。
/// 异常：路径绝对化/规范化或挂载枚举失败传播 I/O 错误；无可用祖先或无覆盖挂载时返回 ConfigError。
/// 注意事项：按路径组成部分匹配挂载根，处理嵌套挂载；只观察已存在祖先，不能预知后续新增挂载。
/// 外部标记只供展示，不验证文件访问权限、剩余空间或介质安全性。
pub fn volume_for_path(path: &Path) -> Result<VolumeInfo, VeilError> {
    // 目标路径可能还不存在（例如尚未创建的容器目录），先找到最近已存在祖先。
    let probe = nearest_existing_path(path)?;
    let root_path = mount_roots()?
        .into_iter()
        // 只保留"覆盖该路径"的挂载点。
        .filter(|root| probe.starts_with(root))
        // 取路径层级最深（components 数量最多）的挂载点，正确穿透嵌套挂载。
        .max_by_key(|root| root.components().count())
        .ok_or_else(|| VeilError::ConfigError(format!("无法识别路径所属卷: {}", path.display())))?;
    Ok(VolumeInfo {
        // 卷名仍取挂载点最后一段路径作为展示名。
        volume_name: root_path
            .file_name()
            .unwrap_or(root_path.as_os_str())
            .to_string_lossy()
            .into_owned(),
        // 区分外部卷：位于外接盘上的路径会得到 true。
        if_out_volume: is_external_mount(&root_path),
        root_path,
    })
}

/// 功能：在目标尚未创建时找到最近可解析的位置，为所属卷判断提供探测路径。
/// 入参：path: &Path，可为现有文件、目录或包含不存在后缀的路径。
/// 返回值：Ok(PathBuf)，为目标自身或首个存在祖先的 canonicalize 结果；Windows 常规盘符去掉 VerbatimDisk 前缀。
/// 异常：路径绝对化、权限等非 NotFound 错误转为 VeilError；一直无法找到位置时返回 ConfigError。
/// 注意事项：不保证结果一定是目录，也不把不存在的后缀拼回；它是探测点，不是目标最终规范路径。
/// 仅规范化 Windows VerbatimDisk 分支，不能据此认为所有 UNC 等路径形式都已统一。
pub fn nearest_existing_path(path: &Path) -> Result<PathBuf, VeilError> {
    // 先转成绝对路径，保证回溯有确定起点。
    let mut current = std::path::absolute(path)?;
    loop {
        match std::fs::canonicalize(&current) {
            Ok(path) => {
                // Windows：canonicalize 可能带 `\\?\` verbatim 前缀（如 \\?\D:\xxx），
                // 规范化成 `D:\xxx`，避免与用户输入的 `D:\` 形式比较失败。
                #[cfg(windows)]
                if let Some(std::path::Component::Prefix(prefix)) = path.components().next()
                    && let std::path::Prefix::VerbatimDisk(letter) = prefix.kind()
                {
                    let mut normalized =
                        PathBuf::from(format!("{}:\\", (letter as char).to_ascii_uppercase()));
                    // 跳过 verbatim 前缀和盘符两段（\\?\D:），拼接剩余部分。
                    for part in path.components().skip(2) {
                        normalized.push(part.as_os_str());
                    }
                    return Ok(normalized);
                }
                // 当前层真实存在，直接作为探测结果返回。
                return Ok(path);
            }
            // 这一层不存在：继续向上一层回溯（NotFound 是回溯的正常信号）。
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            // 其他错误（权限、IO 故障等）不能靠回溯解决，直接返回。
            Err(error) => return Err(error.into()),
        }
        // pop 去掉最后一段路径向父目录回溯；已经到根（pop 失败）说明全链不存在。
        if !current.pop() {
            return Err(VeilError::ConfigError(format!(
                "找不到可用路径: {}",
                path.display()
            )));
        }
    }
}

// iOS 只使用应用私有目录，不枚举系统或外接卷。
#[cfg(target_os = "ios")]
/// 功能：为 iOS 提供受限的挂载根观察，不枚举设备上的其他卷。
/// 入参：无。
/// 返回值：Ok(`Vec<PathBuf>`)，仅包含根路径 /。
/// 异常：此分支不调用枚举系统接口，没有主动返回错误的路径。
/// 注意事项：配合 iOS 外部标记恒为 false，mounted_volumes 返回空列表；不表示系统实际上没有其他卷。
fn mount_roots() -> Result<Vec<PathBuf>, VeilError> {
    // 直接返回根目录作为唯一"挂载点"，配合 is_external_mount 恒为 false，
    // 整个模块退化为"所有路径都属根、无外部卷"。
    Ok(vec![PathBuf::from("/")])
}

#[cfg(target_os = "macos")]
/// 功能：通过 macOS 挂载表获取当前可见的卷根，供路径归属与外部卷筛选。
/// 入参：无，使用 getfsstat 的 MNT_NOWAIT 查询。
/// 返回值：Ok(`Vec<PathBuf>`)，每项取一个 statfs 记录的 f_mntonname，保留非 UTF-8 路径字节。
/// 异常：任一次 getfsstat 返回负数时返回当前系统 I/O 错误。
/// 注意事项：先取数量、再分配私有缓冲并查询，避免共享静态缓冲竞争；两次查询间挂载可能变化，新增条目可能未纳入本次容量。
/// 只把实际已填充且不超过容量的条目标为已初始化，不读取缓冲区未写部分。
fn mount_roots() -> Result<Vec<PathBuf>, VeilError> {
    use std::os::unix::ffi::OsStrExt;
    // getfsstat 写入调用方拥有的缓冲区，避免 getmntinfo 的共享静态缓冲区竞争。
    // 第一次调用传入空指针只取数量，用于计算需要分配的缓冲区大小。
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if count < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // 给两次调用之间新增的挂载留 16 个记录余量；系统按传入字节数限制写入，
    // 余量不足可能漏掉新记录，而不是允许系统越界写入。
    let mut mounts = Vec::<libc::statfs>::with_capacity(count as usize + 16);
    let count = unsafe {
        libc::getfsstat(
            mounts.as_mut_ptr(),
            (mounts.capacity() * std::mem::size_of::<libc::statfs>()) as _,
            libc::MNT_NOWAIT,
        )
    };
    if count < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // 系统最多写入缓冲区容量个完整结构体。
    // 用实际返回数量（不超容量）标记有效长度，避免读取未初始化元素。
    unsafe {
        mounts.set_len((count as usize).min(mounts.capacity()));
    }
    Ok(mounts
        .iter()
        .map(|mount| {
            // f_mntonname 是 C 字符串形式的挂载点；按字节转 OsStr 保留非 UTF-8 路径。
            let name = unsafe { std::ffi::CStr::from_ptr(mount.f_mntonname.as_ptr()) };
            PathBuf::from(std::ffi::OsStr::from_bytes(name.to_bytes()))
        })
        .collect())
}

#[cfg(target_os = "linux")]
/// 功能：从 Linux 进程挂载视图读取卷根，并还原路径中的八进制转义。
/// 入参：无，读取 /proc/self/mountinfo。
/// 返回值：Ok(`Vec<PathBuf>`)，每行第五列形成一条挂载根，缺少该列的行跳过。
/// 异常：读取文件或把整份文本解为 UTF-8 失败时返回 I/O 错误。
/// 注意事项：只解析挂载点字段，不完整验证每行结构；转义按内核生成的三个八进制数字规则还原，依赖系统提供可信内容。
/// 不会递归扫描 /media 或 /mnt；这些路径只用于后续外部卷分类。
fn mount_roots() -> Result<Vec<PathBuf>, VeilError> {
    use std::os::unix::ffi::OsStringExt;
    // mountinfo 的第 5 列是挂载点；空格等字节使用八进制转义。
    // /proc/self/mountinfo 每行描述一个挂载：字段依次为挂载 ID、父 ID、设备号、
    // 根、挂载点、选项……第 5 列（下标 4）就是挂载点路径。
    let content = std::fs::read_to_string("/proc/self/mountinfo")?;
    Ok(content
        .lines()
        // 逐行取第 5 个空白分隔字段，即挂载点；解析失败的行直接跳过。
        .filter_map(|line| line.split_whitespace().nth(4))
        .map(|field| {
            // 挂载点中的空格、反斜杠等按八进制转义（\040、\134 形式），需要还原成真实字节。
            let mut bytes = Vec::new();
            let raw = field.as_bytes();
            let mut i = 0;
            while i < raw.len() {
                // 遇到反斜杠 + 三个八进制数字（\040 形式）就是转义序列。
                if raw[i] == b'\\'
                    && i + 3 < raw.len()
                    && raw[i + 1..i + 4].iter().all(|b| (b'0'..=b'7').contains(b))
                {
                    // 把三个八进制数字换算成原始字节（百位*64 + 十位*8 + 个位）。
                    bytes.push(
                        (raw[i + 1] - b'0') * 64 + (raw[i + 2] - b'0') * 8 + raw[i + 3] - b'0',
                    );
                    // 跳过整个转义序列（反斜杠 + 3 位数字共 4 字节）。
                    i += 4;
                } else {
                    // 普通字节原样保留。
                    bytes.push(raw[i]);
                    i += 1;
                }
            }
            PathBuf::from(std::ffi::OsString::from_vec(bytes))
        })
        .collect())
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    // 内核32 的两个系统接口：枚举逻辑盘符位掩码、查询驱动器类型。
    /// 功能：枚举 Windows 当前可用盘符，供空间位置与卷识别使用。
    /// 入参：无。
    /// 返回值：u32 位掩码，第 0～25 位对应 A～Z；零表示调用失败。
    /// 异常：失败时由调用方读取 last_os_error。
    /// 注意事项：这是盘符快照，不代表所有卷都已就绪或可访问。
    fn GetLogicalDrives() -> u32;
    /// 功能：查询指定 Windows 卷的驱动器类别，辅助识别本地盘、可移动盘等。
    /// 入参：root 为有效的 NUL 结尾 UTF-16 卷根目录指针。
    /// 返回值：返回 Windows DRIVE_* 类别码；未知或无根目录也通过类别码表达。
    /// 异常：不返回 Rust Result，不在此处转换系统错误。
    /// 注意事项：unsafe 外部接口；路径缓冲区须覆盖整个调用期间。
    fn GetDriveTypeW(root: *const u16) -> u32;
}

#[cfg(windows)]
/// 功能：把 Windows 逻辑盘符位图转换成可用于路径前缀匹配的卷根。
/// 入参：无，调用 GetLogicalDrives。
/// 返回值：Ok(`Vec<PathBuf>`)，按 A～Z 顺序包含已置位盘符的 X:\ 根路径。
/// 异常：位图为零时读取 last_os_error 并转换为 VeilError。
/// 注意事项：只列出逻辑盘符，不查询卷内容或保证设备已就绪；驱动器类型筛选由 is_external_mount 另行完成。
fn mount_roots() -> Result<Vec<PathBuf>, VeilError> {
    // 返回 u32 位掩码：第 i 位为 1 表示盘符 A+i 存在。
    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok((0..26)
        // 逐位检查 A 到 Z 26 个盘符是否存在。
        .filter(|i| mask & (1 << i) != 0)
        // 把存在的盘符还原成 "X:\" 形式的根路径。
        .map(|i| PathBuf::from(format!("{}:\\", (b'A' + i) as char)))
        .collect())
}

/// 功能：按当前平台约定把挂载根归为外部位置，供 UI 归类和提示。
/// 入参：root: &Path 为挂载根；通常来自本模块枚举，不应传入任意文件路径。
/// 返回值：bool；iOS 为 false，macOS 位于 /Volumes 下为 true，Linux 位于常见媒体挂载目录下为 true；
/// Windows 仅接受可移动/固定驱动器，并排除用户主目录所在卷。
/// 异常：不返回 Result；Windows 主目录探测失败被忽略，可能影响分类而不报告错误。
/// 注意事项：这是启发式规则，内置盘、网络挂载或路径约定可能被归到“外部”，不能用于权限、弹出或安全决策。
fn is_external_mount(root: &Path) -> bool {
    #[cfg(target_os = "ios")]
    {
        // iOS 只用应用私有目录，任何路径都不视为外部卷。
        let _ = root;
        false
    }
    #[cfg(target_os = "macos")]
    {
        // 用 /Volumes 路径约定分类；该位置也可能包含网络卷或其他并非物理外接的挂载。
        root.starts_with("/Volumes")
    }
    #[cfg(target_os = "linux")]
    {
        // Linux 桌面环境常见外部挂载目录约定。
        root.starts_with("/media") || root.starts_with("/mnt") || root.starts_with("/run/media")
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        // 转成宽字符并以 0 结尾，符合 GetDriveTypeW 的参数要求。
        let wide: Vec<_> = root.as_os_str().encode_wide().chain(Some(0)).collect();
        let kind = unsafe { GetDriveTypeW(wide.as_ptr()) };
        // 2=可移动（U盘）、3=固定盘；但用户主目录所在的盘不视为外部卷。
        let home_is_here = dirs::home_dir()
            .and_then(|home| nearest_existing_path(&home).ok())
            .is_some_and(|home| home.starts_with(root));
        matches!(kind, 2 | 3) && !home_is_here
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// 验证卷路径解析既能处理已存在路径，也能定位未来路径，且解析过程不会创建目录。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn resolves_existing_and_future_paths_without_creating_them() {
        let temp = tempfile::TempDir::new().unwrap();
        let future = temp.path().join("future/work");
        assert_eq!(
            volume_for_path(temp.path()).unwrap(),
            volume_for_path(&future).unwrap()
        );
        assert!(!future.exists());
        for volume in mounted_volumes().unwrap() {
            assert!(volume.root_path.is_dir());
        }
    }
}
