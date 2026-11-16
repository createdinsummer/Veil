//! 文件作用：定义长截图流程中的帧数据层——RGB24 帧结构、亮度采样、以及
//! FFmpeg 输出的 P6 PPM 管道的解析。
//! 所属业务模块：长截图工具（scrollshot）的帧数据子模块。
//! 核心结构体/函数清单：RgbFrame（帧数据载体）、frame_bytes（尺寸校验）、
//! read_ppm（流式解析一帧）、token（PPM 头字段读取）。
//! 依赖标准库缓冲读取与 anyhow 错误封装；不管理 FFmpeg 进程，只处理帧字节格式。
//! 业务目标：以内存安全的方式承接解码出的原始像素，供匹配与拼接两个模块消费。
use anyhow::{Result, bail, ensure};
use std::io::BufRead;

/// 单帧载荷上限：宽×高×3 不得超过 96 MiB；避免无界单帧分配，不代表整个进程内存配额。
const MAX_FRAME_BYTES: usize = 96 * 1024 * 1024;

/// RGB24 帧，像素按行排列，不包含行填充。
/// 业务定位：一帧抽样的原始像素载体，是整个长截图流程里帧数据的基本单位。
/// 数据说明：pixels 为 RGB24 字节流（每个像素 3 字节，R、G、B 各 1 字节），
/// 按“第 0 行、第 1 行……”顺序紧密排列，没有行末填充字节。
/// 使用场景：视频模块解析 FFmpeg 输出后构造它；匹配模块在其上采样亮度列；
/// 拼接模块按行切取条带写入长图。
#[derive(Clone, Debug)]
pub struct RgbFrame {
    /// 帧宽（像素）。
    width: u32,
    /// 帧高（像素）。
    height: u32,
    /// RGB24 像素字节流，长度恒等于 width*height*3。
    pixels: Vec<u8>,
}

impl RgbFrame {
    /// 接收一份紧密排列的 RGB24 像素所有权并验证帧尺寸。
    /// 入参：width/height:u32 源帧宽高；pixels:`Vec<u8>` 行优先、每像素 R/G/B 三字节，无行填充。
    /// 返回值：尺寸乘积在 1..=96 MiB 且字节数准确时 Ok(RgbFrame)，否则 Err。
    /// 异常：尺寸乘法溢出、零边长、超过上限或数据长度不符返回错误。
    /// 注意事项：按值移动 pixels，不复制已有像素；上限仅针对一帧字节，不限制多个 Clone 或算法缓存总内存。
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self> {
        let bytes = frame_bytes(width, height)?;
        ensure!(pixels.len() == bytes, "RGB 数据长度与帧尺寸不符");
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// 读取帧宽度用于裁剪及输出尺寸计算。
    /// 入参：&self，只读有效帧。
    /// 返回值：u32，像素列数。
    /// 异常：不返回错误。
    /// 注意事项：不访问文件或重新检查像素数据。
    pub fn width(&self) -> u32 {
        self.width
    }
    /// 读取帧高度用于逐行处理及裁剪。
    /// 入参：&self，只读有效帧。
    /// 返回值：u32，像素行数。
    /// 异常：不返回错误。
    /// 注意事项：单位是像素，不是视频时间或累计长图高度。
    pub fn height(&self) -> u32 {
        self.height
    }
    /// 为像素比较等调用方提供原始帧内容的只读视图。
    /// 入参：&self，只读有效帧。
    /// 返回值：&[u8]，完整行优先 RGB24 字节切片，生命周期随 self 借用。
    /// 异常：不返回错误或分配副本。
    /// 注意事项：调用方不能在借用仍有效时销毁或可变修改原帧；不含宽高信息头。
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// 读取一行原始像素，供拼接裁剪后写入条带。
    /// 入参：&self；y:usize 从零起的行号，必须小于 height。
    /// 返回值：&[u8]，该行 width*3 个 RGB 字节，不复制。
    /// 异常：越界行号或极大索引算术可能 panic。
    /// 注意事项：内部接口依赖调用方先校验裁剪范围，不返回 Result；切片只在帧借用期间有效。
    pub(crate) fn row(&self, y: usize) -> &[u8] {
        let stride = self.width as usize * 3;
        &self.pixels[y * stride..(y + 1) * stride]
    }

    /// 将一个 RGB 像素压成亮度，供匹配与固定栏统计。
    /// 入参：&self；x/y:usize，从零起的列/行，须分别小于 width/height。
    /// 返回值：u8，以 (77R+150G+29B)/256 向下取整的亮度值。
    /// 异常：无 Result，越界索引可能 panic；x 超宽也可能落到下一行而不立即报错。
    /// 注意事项：不单独检查坐标，不做颜色空间校正；只为低成本图像比较提供近似亮度。
    pub(crate) fn luma(&self, x: usize, y: usize) -> u8 {
        let i = (y * self.width as usize + x) * 3;
        ((self.pixels[i] as u32 * 77
            + self.pixels[i + 1] as u32 * 150
            + self.pixels[i + 2] as u32 * 29)
            >> 8) as u8
    }
}

/// 计算单帧 RGB24 所需字节并拒绝不合理尺寸。
/// 入参：width/height:u32 像素宽高。
/// 返回值：Ok(usize) 为宽×高×3，要求大于零且不超过 MAX_FRAME_BYTES；否则 Err。
/// 异常：乘法溢出按错误处理，不分配像素缓冲。
/// 注意事项：96 MiB 是帧载荷上限，不保证系统一定有足够内存完成分配。
fn frame_bytes(width: u32, height: u32) -> Result<usize> {
    let bytes = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(3));
    match bytes {
        Some(n) if n > 0 && n <= MAX_FRAME_BYTES => Ok(n),
        _ => bail!("视频帧尺寸无效或超过 96 MiB 限制：{width}×{height}"),
    }
}

// FFmpeg 通过 P6 PPM 管道提供尺寸和 RGB 数据，不依赖 ffprobe。
/// 功能：从字节流中流式解析一帧 P6 PPM 图像。
/// 入参：reader 为缓冲读取器（视频模块包在 FFmpeg stdout 上）。
/// 返回值：读到完整一帧时返回 Ok(Some(帧))；流已耗尽（空）返回 Ok(None)，
/// 供上层据此结束逐帧循环。
/// 异常/错误：流中断、头字段非法（非 P6、尺寸越界、非 255 最大色值、头过长）
/// 时返回 Err，由上层终止解码并附上 FFmpeg 诊断。
/// 注意事项：P6 格式为“P6\n宽 高\n255\n+ 二进制像素”；本函数只读一帧，
/// 下一帧直接从当前位置继续，因此支持管道连续多帧。
pub(crate) fn read_ppm(reader: &mut impl BufRead) -> Result<Option<RgbFrame>> {
    if reader.fill_buf()?.is_empty() {
        // 管道关闭且无剩余字节，说明 FFmpeg 已输出完所有帧。
        return Ok(None);
    }
    ensure!(token(reader)? == "P6", "解码器输出的帧不是 P6 PPM 格式");
    let width = token(reader)?.parse::<u32>()?;
    let height = token(reader)?.parse::<u32>()?;
    ensure!(token(reader)? == "255", "只支持 8 位 RGB PPM 帧");
    // 头解析完后按尺寸精确读取像素区；长度不足说明流被截断，read_exact 会报错。
    let mut pixels = vec![0; frame_bytes(width, height)?];
    reader.read_exact(&mut pixels)?;
    Ok(Some(RgbFrame::new(width, height, pixels)?))
}

/// 读取 PPM 头中的一个字段，并消费结束该字段的一个空白字节。
/// 入参：reader:&mut impl BufRead，同步可读字节流，位置应位于 PPM 头而非二进制像素。
/// 返回值：Ok(String) 为非空 UTF-8 字段；字节读取、UTF-8 或超过 32 字节字段错误返回 Err。
/// 异常：读取可阻塞，不捕获分配失败；# 开头注释通过 read_until 整行存入 Vec，注释长度没有 32 字节上限。
/// 注意事项：字段前可跳过空白/注释，但字段后只消费首个空白。最大色值后若多放额外空白（例如 CRLF 的第二字节），剩余字节会被像素读取，不是通用 PPM 排版兼容器。
fn token(reader: &mut impl BufRead) -> Result<String> {
    let mut result = Vec::new();
    loop {
        let mut byte = [0];
        reader.read_exact(&mut byte)?;
        if byte[0] == b'#' && result.is_empty() {
            // 字段开头遇到 '#' 视为整行注释：吞掉到换行为止后继续找字段。
            let mut comment = Vec::new();
            reader.read_until(b'\n', &mut comment)?;
            continue;
        }
        if byte[0].is_ascii_whitespace() {
            // 字段之间的空白符：若已有内容则字段结束，否则继续跳过空白。
            if !result.is_empty() {
                break;
            }
        } else {
            result.push(byte[0]);
            ensure!(result.len() <= 32, "PPM 帧头过长");
        }
    }
    Ok(String::from_utf8(result)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// 验证 PPM 解析不会把像素区的空白字节（\n、\r、空格）误当成分隔符，
    /// 且同一管道可以连续解析多帧。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn ppm_stream_preserves_binary_whitespace_and_multiple_frames() {
        let bytes = b"P6\n# comment\n1 1\n255\n\n\r P6\n1 1\n255\n\x00\xff\x23";
        let mut reader = Cursor::new(bytes);
        assert_eq!(read_ppm(&mut reader).unwrap().unwrap().pixels(), b"\n\r ");
        assert_eq!(
            read_ppm(&mut reader).unwrap().unwrap().pixels(),
            &[0, 255, 35]
        );
        assert!(read_ppm(&mut reader).unwrap().is_none());
    }

    /// 验证截断的像素区与超大帧头都会触发错误而不是静默产生坏帧。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    /// 异常：测试准备或断言失败会 panic；场景中预期的错误由测试体捕获或核对，不作为生产错误返回。
    fn truncated_frame_and_oversized_header_fail() {
        assert!(read_ppm(&mut Cursor::new(b"P6\n1 1\n255\n\x00")).is_err());
        assert!(read_ppm(&mut Cursor::new(b"P6\n4294967295 4294967295\n255\n")).is_err());
    }
}
