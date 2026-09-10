/// 输出编码自适应模块
///
/// 核心思想：
/// 1. 自动检测当前显示环境的编码（跨平台）
/// 2. 提供 UTF-8 到目标编码的转换函数（统一 API）
/// 3. 不负责实际输出，只提供编码转换能力
///
/// 适用场景：
/// - CLI 控制台输出
/// - TUI 文本界面
/// - GUI 窗口显示
/// - 日志文件输出
///
/// 设计原则：
/// - 跨平台统一 API
/// - 程序内部统一使用 UTF-8
/// - 文件内容保持原始字节，不转码
/// - 只在需要显示给用户时才进行编码转换
#[cfg(target_os = "windows")]
use std::io;

/// 支持的编码类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayEncoding {
    Utf8,
    #[cfg(target_os = "windows")]
    Gbk,
    #[cfg(target_os = "windows")]
    Gb2312,
    #[cfg(target_os = "windows")]
    ShiftJis,
    #[cfg(target_os = "windows")]
    Big5,
    #[cfg(target_os = "windows")]
    Unknown(u32),
}

impl DisplayEncoding {
    /// 从代码页 ID 创建编码类型
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

/// 获取当前显示环境的输出编码（跨平台）
///
/// - Windows: 通过 GetConsoleOutputCP() 检测
/// - Unix/Linux/macOS: 默认 UTF-8（现代系统标准）
fn get_display_encoding() -> DisplayEncoding {
    get_display_encoding_impl()
}

// Windows 实现
#[cfg(target_os = "windows")]
fn get_display_encoding_impl() -> DisplayEncoding {
    unsafe {
        unsafe extern "system" {
            fn GetConsoleOutputCP() -> u32;
        }
        let cp = GetConsoleOutputCP();
        DisplayEncoding::from_code_page(cp)
    }
}

// Unix/Linux/macOS 实现
#[cfg(not(target_os = "windows"))]
fn get_display_encoding_impl() -> DisplayEncoding {
    // Unix 系统现代默认都是 UTF-8
    // 如果需要更精确的检测，可以读取 LANG 环境变量
    DisplayEncoding::Utf8
}

/// 将 UTF-8 字符串编码为显示环境可识别的字节序列（跨平台）
///
/// 这是核心转换函数，适用于所有需要向用户显示文本的场景。
///
/// # 参数
/// - `utf8_str`: 程序内部的 UTF-8 字符串
///
/// # 返回
/// - `Vec<u8>`: 目标编码的字节序列
///
/// # 示例
/// ```text
/// // CLI 输出
/// let bytes = encode_for_display("请输入密码: ");
/// std::io::stdout().write_all(&bytes)?;
///
/// // TUI 渲染
/// let bytes = encode_for_display("文件列表");
/// tui.render_text(&bytes);
///
/// // GUI 显示
/// let bytes = encode_for_display("保存成功");
/// gui.set_label(&bytes);
/// ```
pub fn encode_for_display(utf8_str: &str) -> Vec<u8> {
    let encoding = get_display_encoding();

    // 如果是 UTF-8，直接返回
    if matches!(encoding, DisplayEncoding::Utf8) {
        return utf8_str.as_bytes().to_vec();
    }

    // 否则进行编码转换
    convert_utf8_to_encoding(utf8_str, encoding)
}

// ==================== 平台相关实现（内部函数） ====================

// Windows: 使用 Windows API 进行编码转换
#[cfg(target_os = "windows")]
fn convert_utf8_to_encoding(utf8_str: &str, encoding: DisplayEncoding) -> Vec<u8> {
    use winapi::um::stringapiset::{MultiByteToWideChar, WideCharToMultiByte};
    use winapi::um::winnls::CP_UTF8;

    unsafe {
        // 步骤 1: UTF-8 → UTF-16
        let utf8_bytes = utf8_str.as_bytes();
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
        MultiByteToWideChar(
            CP_UTF8,
            0,
            utf8_bytes.as_ptr() as *const i8,
            utf8_bytes.len() as i32,
            wide_buf.as_mut_ptr(),
            wide_len,
        );

        // 步骤 2: UTF-16 → 目标编码
        let target_cp = match encoding {
            DisplayEncoding::Utf8 => CP_UTF8,
            DisplayEncoding::Gbk => 936,
            DisplayEncoding::Gb2312 => 20936,
            DisplayEncoding::ShiftJis => 932,
            DisplayEncoding::Big5 => 950,
            DisplayEncoding::Unknown(cp) => cp,
        };

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

// Unix/Linux/macOS: UTF-8 是默认编码，直接返回
#[cfg(not(target_os = "windows"))]
fn convert_utf8_to_encoding(utf8_str: &str, _encoding: DisplayEncoding) -> Vec<u8> {
    utf8_str.as_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_utf8_conversion() {
        let test_str = "测试中文";
        let bytes = encode_for_display(test_str);
        assert!(!bytes.is_empty());
    }
}
