//! 终端显示文本的编码适配。
//!
//! 程序内部统一使用 UTF-8；本模块只在向当前终端显示文本时转换编码。Windows 下根据
//! 控制台输出代码页转换，其他平台返回UTF-8。本模块也提供write_stdout及输出宏，但它们直接写UTF-8，
//! 不自动调用转换函数；编码转换主要由密码提示等显式调用，不处理文件内容。
//!
//! 核心清单：`write_stdout` 带 I/O 错误返回的输出入口；`outln!`/`out!` 可失败输出宏；
//! `DisplayEncoding` 显示编码枚举；`encode_for_display` 把内部 UTF-8 文本转为终端字节。
use std::fmt;
use std::io::{self, Write};

/// 向CLI标准输出写格式化文本，并把I/O失败交给调用方。
/// 入参：args（fmt::Arguments）为待格式化内容；newline（bool）决定是否追加换行。
/// 返回值：写入并flush成功为Ok(())。
/// 异常：fd检查、格式化写入、换行或flush失败返回io::Error；自定义Display实现仍可能panic。
/// 注意事项：持StdoutLock覆盖整次写入；不调用encode_for_display，文本保持UTF-8。flush不是磁盘fsync，预检查不检测后续所有管道错误。
pub fn write_stdout(args: fmt::Arguments<'_>, newline: bool) -> io::Result<()> {
    // 先确认标准输出描述符仍可用（例如管道未被下游程序提前关闭）。
    ensure_stdout_fd_open()?;
    let stdout = io::stdout();
    // 锁定标准输出，保证本进程多线程下的整行写入不被交错。
    let mut output = stdout.lock();
    output.write_fmt(args)?;
    if newline {
        output.write_all(b"\n")?;
    }
    output.flush()
}

/// Unix 上显式检查标准输出描述符，避免 Rust 标准库在无效 fd 上静默写入。
#[cfg(unix)]
/// 在Unix上提前发现stdout描述符已关闭的情况。
/// 入参：无，查询STDOUT_FILENO的F_GETFD。
/// 返回值：fcntl成功为Ok(())。
/// 异常：fcntl返回-1时返回last_os_error。
/// 注意事项：只验证fd可查询，不证明可写或下游仍读；实际write/flush仍可失败。
fn ensure_stdout_fd_open() -> io::Result<()> {
    // SAFETY: `fcntl` 只查询 fd 标志，不访问内存；`STDOUT_FILENO` 是稳定的进程 fd。
    let result = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_GETFD) };
    // 返回 -1 表示描述符已失效（如管道被关闭），把系统错误交给上层处理。
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// 非 Unix 平台先沿用标准库写入结果检测输出错误。
#[cfg(not(unix))]
/// 在非Unix平台把输出可用性检查交给后续标准库写入。
/// 入参：无。
/// 返回值：始终Ok(())。
/// 异常：本分支不返回错误。
/// 注意事项：不调用操作系统验证句柄；后续write_stdout承担实际I/O检查。
fn ensure_stdout_fd_open() -> io::Result<()> {
    Ok(())
}

/// 记录标准输出失败，尽力向stderr说明后结束CLI进程。
/// 入参：error（io::Error）为输出失败。
/// 返回值：!，固定process::exit(1)，不会返回。
/// 异常：日志flush及stderr写失败忽略，不以Result传播。
/// 注意事项：BrokenPipe记Info但仍退出1；process::exit不展开作用域析构，不能据此保证在途资源清理完成。
pub fn exit_for_output_error(error: io::Error) -> ! {
    if error.kind() == io::ErrorKind::BrokenPipe {
        // 管道被下游程序提前关闭（如 `veil help | head`）属于正常现象，按信息级别记录。
        veil_core::logging::record(
            veil_core::logging::Level::Info,
            "cli",
            "write_output",
            "OUTPUT_CLOSED",
        );
    } else {
        // 其余写入错误属于真实故障，记录完整错误链。
        veil_core::logging::error_chain("cli", "write_output", &error);
    }
    let _ = veil_core::logging::flush();
    // 标准错误是最后手段；这里即使写失败也只能忽略，随后照常退出。
    let stderr = io::stderr();
    let mut output = stderr.lock();
    let _ = writeln!(output, "❌ 错误 [9001]: I/O 错误: {error}");
    std::process::exit(1);
}

/// 可失败的 `crate::outln!` 替代宏。
///
/// 任一标准输出写入失败都会转换为稳定 I/O 错误并退出，不会 panic 或静默成功。
#[macro_export]
macro_rules! outln {
    // 无参数版本：只输出一个换行（空行）。
    () => {{
        if let Err(error) = $crate::output_encoding::write_stdout(format_args!(""), true) {
            $crate::output_encoding::exit_for_output_error(error);
        }
    }};
    // 格式化版本：输出一行格式化文本并换行。
    ($($arg:tt)*) => {{
        if let Err(error) = $crate::output_encoding::write_stdout(format_args!($($arg)*), true) {
            $crate::output_encoding::exit_for_output_error(error);
        }
    }};
}

/// 可失败的 `crate::out!` 替代宏。
///
/// 与 `outln!` 相同，只是不追加换行，供提示词、进度条等需要原地刷新的场景。
#[macro_export]
macro_rules! out {
    () => {{
        if let Err(error) = $crate::output_encoding::write_stdout(format_args!(""), false) {
            $crate::output_encoding::exit_for_output_error(error);
        }
    }};
    ($($arg:tt)*) => {{
        if let Err(error) = $crate::output_encoding::write_stdout(format_args!($($arg)*), false) {
            $crate::output_encoding::exit_for_output_error(error);
        }
    }};
}

/// 当前显示环境可识别的文本编码。
///
/// 只在需要把内部 UTF-8 文案写到 Windows 旧式控制台时使用；Unix 始终是 Utf8。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayEncoding {
    /// UTF-8。
    Utf8,
    #[cfg(target_os = "windows")]
    /// GBK。
    ///
    /// Windows 简体中文旧版控制台的默认代码页（936），覆盖大部分常用汉字。
    Gbk,
    #[cfg(target_os = "windows")]
    /// GB2312。
    ///
    /// 更早的简体中文标准（20936），字符集比 GBK 小。
    Gb2312,
    #[cfg(target_os = "windows")]
    /// Shift-JIS。
    ///
    /// 日文 Windows 控制台代码页（932）。
    ShiftJis,
    #[cfg(target_os = "windows")]
    /// Big5。
    ///
    /// 繁体中文 Windows 控制台代码页（950）。
    Big5,
    #[cfg(target_os = "windows")]
    /// 未单独映射的 Windows 代码页。
    ///
    /// 保留原始代码页 ID，转换时直接按该 ID 调用系统 API。
    Unknown(u32),
}

impl DisplayEncoding {
    /// 将 Windows 控制台代码页 ID 映射为已知编码。
    #[cfg(target_os = "windows")]
    /// 将Windows控制台代码页映射成显示编码枚举。
    /// 入参：cp（u32）为系统返回代码页；65001/936/20936/932/950映射已知项。
    /// 返回值：DisplayEncoding，其他值含0保留为Unknown(cp)。
    /// 异常：无错误返回。
    /// 注意事项：仅Windows编译，识别不是转换成功保证，不查询终端是否重定向。
    fn from_code_page(cp: u32) -> Self {
        match cp {
            // 65001 = UTF-8 代码页，与内部编码一致，转换时直接透传。
            65001 => DisplayEncoding::Utf8,
            // 936 = 简体中文 GBK；20936 = 旧版简体中文 GB2312。
            936 => DisplayEncoding::Gbk,
            20936 => DisplayEncoding::Gb2312,
            // 932 = 日文 Shift-JIS；950 = 繁体中文 Big5。
            932 => DisplayEncoding::ShiftJis,
            950 => DisplayEncoding::Big5,
            // 未收录的代码页保留原始数值，转换时按 ID 调用系统 API 尽力转换。
            _ => DisplayEncoding::Unknown(cp),
        }
    }
}

/// 按当前平台取得本次编码转换采用的显示代码页。
/// 入参：无。
/// 返回值：Windows读当前控制台输出代码页；其他平台Utf8。
/// 异常：无Result返回，系统未知值保留Unknown。
/// 注意事项：每次调用都会检测，不缓存；控制台代码页可被外部改变。
fn get_display_encoding() -> DisplayEncoding {
    get_display_encoding_impl()
}

/// 在 Windows 上读取当前控制台输出代码页。
#[cfg(target_os = "windows")]
/// 读取Windows当前控制台输出代码页并解释成显示编码。
/// 入参：无。
/// 返回值：DisplayEncoding，系统返回0也按Unknown(0)处理。
/// 异常：无Result错误返回。
/// 注意事项：只读控制台值，不检测stdout是否是管道；不切换控制台代码页。
fn get_display_encoding_impl() -> DisplayEncoding {
    // SAFETY: `GetConsoleOutputCP` 不接收指针，只读取当前进程控制台的整型代码页；
    // 返回值由本函数立即按已知映射处理。
    unsafe {
        // 声明当前进程所需的 Windows 控制台 API。
        unsafe extern "system" {
            /// 读取当前 Windows 控制台输出代码页。
            /// 入参：无。
            /// 返回值：u32 代码页编号，失败返回 0。
            /// 异常：外部系统 API 不返回 Rust Result。
            /// 注意事项：仅 Windows 声明；不改变代码页，不代表重定向输出文件使用同一编码。
            fn GetConsoleOutputCP() -> u32;
        }
        let cp = GetConsoleOutputCP();
        DisplayEncoding::from_code_page(cp)
    }
}

/// 在非 Windows 平台返回 UTF-8。
#[cfg(not(target_os = "windows"))]
/// 在非Windows构建中选择UTF-8显示字节。
/// 入参：无。
/// 返回值：DisplayEncoding::Utf8。
/// 异常：无错误返回。
/// 注意事项：不读取系统locale编码，即使终端配置其他编码仍返回UTF-8。
fn get_display_encoding_impl() -> DisplayEncoding {
    DisplayEncoding::Utf8
}

/// 为显式需要终端代码页适配的调用方转换UTF-8文本。
/// 入参：utf8_str（&str）为内部Unicode文本，允许空串。
/// 返回值：`Vec<u8>`；UTF-8直接复制，Windows其他代码页调用转换，部分失败回退UTF-8。
/// 异常：无Result错误返回，转换不支持/失败通过回退处理。
/// 注意事项：每次检测当前代码页；不是write_stdout自动步骤。不能据返回字节证明所有字符可表示，旧代码页可能用替代字符。
///
/// # 示例
/// ```no_run
/// # use std::io::Write;
/// # use veil_cli::output_encoding::encode_for_display;
/// # fn main() -> std::io::Result<()> {
/// let bytes = encode_for_display("请输入密码: ");
/// std::io::stdout().write_all(&bytes)?;
/// # Ok(())
/// # }
/// ```
pub fn encode_for_display(utf8_str: &str) -> Vec<u8> {
    // 每次读取当前控制台代码页，不缓存；同一进程期间也可能被外部切换。
    let encoding = get_display_encoding();

    // UTF-8环境无需转码，但to_vec仍分配并复制原字节。
    if matches!(encoding, DisplayEncoding::Utf8) {
        return utf8_str.as_bytes().to_vec();
    }

    // 其余编码走"UTF-8 → UTF-16 → 目标代码页"两步系统 API 转换。
    convert_utf8_to_encoding(utf8_str, encoding)
}

/// 在 Windows 上通过系统 API 将 UTF-8 转为目标代码页。
///
/// 长度查询失败回退UTF-8；实际填充调用未检查返回值，详见下方函数契约。
#[cfg(target_os = "windows")]
/// 借助Windows宽字符API把UTF-8提示转换为选定代码页。
/// 入参：utf8_str（&str）为UTF-8文本；encoding（DisplayEncoding）为目标代码页枚举。
/// 返回值：`Vec<u8>`，长度查询失败返回原UTF-8；正常分配UTF-16和目标字节缓冲后转换。
/// 异常：无Result返回；实际填充API返回值未检查，因此不能声称任一次API失败都必定回退。
/// 注意事项：仅Windows；长度转i32无额外范围验证，调用方应限制文本规模。未传替代字符检测标志，目标编码不能表达的字符可能被替换。
fn convert_utf8_to_encoding(utf8_str: &str, encoding: DisplayEncoding) -> Vec<u8> {
    use winapi::um::stringapiset::{MultiByteToWideChar, WideCharToMultiByte};
    use winapi::um::winnls::CP_UTF8;

    // SAFETY: 两次 API 调用均先查询长度，再使用该长度分配缓冲区；传入的指针和长度
    // 与实际分配一致，且字符串字节在调用期间保持有效。
    unsafe {
        let utf8_bytes = utf8_str.as_bytes();
        // 先查询 UTF-16 长度，再按该长度分配输出缓冲区。
        let wide_len = MultiByteToWideChar(
            CP_UTF8,
            0,
            utf8_bytes.as_ptr() as *const i8,
            utf8_bytes.len() as i32,
            std::ptr::null_mut(),
            0,
        );

        if wide_len == 0 {
            return utf8_str.as_bytes().to_vec();
        }

        let mut wide_buf = vec![0u16; wide_len as usize];
        // 第二次调用把 UTF-8 字节实际转换为 UTF-16。
        MultiByteToWideChar(
            CP_UTF8,
            0,
            utf8_bytes.as_ptr() as *const i8,
            utf8_bytes.len() as i32,
            wide_buf.as_mut_ptr(),
            wide_len,
        );

        // 把显示编码枚举还原为 Windows 代码页 ID，供第二次系统调用使用。
        let target_cp = match encoding {
            DisplayEncoding::Utf8 => CP_UTF8,
            DisplayEncoding::Gbk => 936,
            DisplayEncoding::Gb2312 => 20936,
            DisplayEncoding::ShiftJis => 932,
            DisplayEncoding::Big5 => 950,
            DisplayEncoding::Unknown(cp) => cp,
        };

        // 再查询目标代码页所需字节数，避免固定缓冲区截断。
        let mb_len = WideCharToMultiByte(
            target_cp,
            0,
            wide_buf.as_ptr(),
            wide_buf.len() as i32,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            std::ptr::null_mut(),
        );

        if mb_len == 0 {
            return utf8_str.as_bytes().to_vec();
        }

        let mut mb_buf = vec![0u8; mb_len as usize];
        // 最后把 UTF-16 转为 Windows 控制台所需的代码页字节。
        WideCharToMultiByte(
            target_cp,
            0,
            wide_buf.as_ptr(),
            wide_buf.len() as i32,
            mb_buf.as_mut_ptr() as *mut i8,
            mb_len,
            std::ptr::null(),
            std::ptr::null_mut(),
        );

        // 返回目标代码页字节序列，供标准输出直接写出。
        mb_buf
    }
}

/// 在非 Windows 平台直接返回 UTF-8 字节。
#[cfg(not(target_os = "windows"))]
/// 为非Windows平台保留UTF-8字节。
/// 入参：utf8_str为输入；_encoding为未使用的显示编码参数。
/// 返回值：原输入复制出的 `Vec<u8>`，空输入为空。
/// 异常：无错误返回。
/// 注意事项：不执行locale转换或系统调用；仅此平台备用分支。
fn convert_utf8_to_encoding(utf8_str: &str, _encoding: DisplayEncoding) -> Vec<u8> {
    utf8_str.as_bytes().to_vec()
}

/// 显示编码检测和转换的单元测试。
#[cfg(test)]
mod tests {
    use super::*;

    /// 验证当前平台能返回受支持的显示编码。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn test_encoding_detection() {
        let encoding = get_display_encoding();
        crate::outln!("当前显示编码: {:?}", encoding);
        #[cfg(target_os = "windows")]
        assert!(matches!(
            encoding,
            DisplayEncoding::Utf8
                | DisplayEncoding::Gbk
                | DisplayEncoding::Gb2312
                | DisplayEncoding::Unknown(_)
        ));
        #[cfg(not(target_os = "windows"))]
        assert_eq!(encoding, DisplayEncoding::Utf8);
    }

    /// 验证编码入口能够产生非空输出。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn test_utf8_conversion() {
        let test_str = "测试中文";
        let bytes = encode_for_display(test_str);
        assert!(!bytes.is_empty());
    }
}
