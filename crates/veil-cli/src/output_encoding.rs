//! 终端显示文本的编码适配。
//!
//! 程序内部统一使用 UTF-8；本模块只在向当前终端显示文本时转换编码。Windows 下根据
//! 控制台输出代码页转换，Unix 平台直接返回 UTF-8 字节。模块不负责写出，也不转换
//! 文件内容。
#[cfg(target_os = "windows")]
use std::io;

/// 当前显示环境可识别的文本编码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayEncoding {
    /// UTF-8。
    Utf8,
    #[cfg(target_os = "windows")]
    /// GBK。
    Gbk,
    #[cfg(target_os = "windows")]
    /// GB2312。
    Gb2312,
    #[cfg(target_os = "windows")]
    /// Shift-JIS。
    ShiftJis,
    #[cfg(target_os = "windows")]
    /// Big5。
    Big5,
    #[cfg(target_os = "windows")]
    /// 未单独映射的 Windows 代码页。
    Unknown(u32),
}

impl DisplayEncoding {
    /// 将 Windows 控制台代码页 ID 映射为已知编码。
    #[cfg(target_os = "windows")]
    fn from_code_page(cp: u32) -> Self {
        match cp {
            65001 => DisplayEncoding::Utf8,
            936 => DisplayEncoding::Gbk,
            20936 => DisplayEncoding::Gb2312,
            932 => DisplayEncoding::ShiftJis,
            950 => DisplayEncoding::Big5,
            _ => DisplayEncoding::Unknown(cp),
        }
    }
}

/// 检测当前显示环境的输出编码。
///
/// - Windows: 通过 GetConsoleOutputCP() 检测
/// - Unix/Linux/macOS: 默认 UTF-8（现代系统标准）
fn get_display_encoding() -> DisplayEncoding {
    get_display_encoding_impl()
}

/// 在 Windows 上读取当前控制台输出代码页。
#[cfg(target_os = "windows")]
fn get_display_encoding_impl() -> DisplayEncoding {
    // SAFETY: `GetConsoleOutputCP` 不接收指针，只读取当前进程控制台的整型代码页；
    // 返回值由本函数立即按已知映射处理。
    unsafe {
        // 声明当前进程所需的 Windows 控制台 API。
        unsafe extern "system" {
            /// Windows API：读取当前控制台输出代码页。
            fn GetConsoleOutputCP() -> u32;
        }
        let cp = GetConsoleOutputCP();
        DisplayEncoding::from_code_page(cp)
    }
}

/// 在非 Windows 平台返回 UTF-8。
#[cfg(not(target_os = "windows"))]
fn get_display_encoding_impl() -> DisplayEncoding {
    DisplayEncoding::Utf8
}

/// 将 UTF-8 字符串编码为当前显示环境可识别的字节序列。
///
/// UTF-8 环境直接复制原字节；Windows 非 UTF-8 代码页则先转换为 UTF-16，再转为目标
/// 代码页。
///
/// # 参数
/// - `utf8_str`: 程序内部的 UTF-8 字符串
///
/// # 返回
/// 返回可直接写入标准输出的目标编码字节序列。
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
    let encoding = get_display_encoding();

    if matches!(encoding, DisplayEncoding::Utf8) {
        return utf8_str.as_bytes().to_vec();
    }

    convert_utf8_to_encoding(utf8_str, encoding)
}

/// 在 Windows 上通过系统 API 将 UTF-8 转为目标代码页。
///
/// 任一次转换调用失败时回退为原始 UTF-8 字节，避免终端输出流程中断。
#[cfg(target_os = "windows")]
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

        mb_buf
    }
}

/// 在非 Windows 平台直接返回 UTF-8 字节。
#[cfg(not(target_os = "windows"))]
fn convert_utf8_to_encoding(utf8_str: &str, _encoding: DisplayEncoding) -> Vec<u8> {
    utf8_str.as_bytes().to_vec()
}

/// 显示编码检测和转换的单元测试。
#[cfg(test)]
mod tests {
    use super::*;

    /// 验证当前平台能返回受支持的显示编码。
    #[test]
    fn test_encoding_detection() {
        let encoding = get_display_encoding();
        println!("当前显示编码: {:?}", encoding);
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
    fn test_utf8_conversion() {
        let test_str = "测试中文";
        let bytes = encode_for_display(test_str);
        assert!(!bytes.is_empty());
    }
}
