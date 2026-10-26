use anyhow::{Result, bail, ensure};
use std::io::BufRead;

const MAX_FRAME_BYTES: usize = 96 * 1024 * 1024;

/// RGB24 帧，像素按行排列，不包含行填充。
#[derive(Clone, Debug)]
pub struct RgbFrame {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl RgbFrame {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self> {
        let bytes = frame_bytes(width, height)?;
        ensure!(pixels.len() == bytes, "RGB 数据长度与帧尺寸不符");
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub(crate) fn row(&self, y: usize) -> &[u8] {
        let stride = self.width as usize * 3;
        &self.pixels[y * stride..(y + 1) * stride]
    }

    pub(crate) fn luma(&self, x: usize, y: usize) -> u8 {
        let i = (y * self.width as usize + x) * 3;
        ((self.pixels[i] as u32 * 77
            + self.pixels[i + 1] as u32 * 150
            + self.pixels[i + 2] as u32 * 29)
            >> 8) as u8
    }
}

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
pub(crate) fn read_ppm(reader: &mut impl BufRead) -> Result<Option<RgbFrame>> {
    if reader.fill_buf()?.is_empty() {
        return Ok(None);
    }
    ensure!(token(reader)? == "P6", "解码器输出的帧不是 P6 PPM 格式");
    let width = token(reader)?.parse::<u32>()?;
    let height = token(reader)?.parse::<u32>()?;
    ensure!(token(reader)? == "255", "只支持 8 位 RGB PPM 帧");
    let mut pixels = vec![0; frame_bytes(width, height)?];
    reader.read_exact(&mut pixels)?;
    Ok(Some(RgbFrame::new(width, height, pixels)?))
}

fn token(reader: &mut impl BufRead) -> Result<String> {
    let mut result = Vec::new();
    loop {
        let mut byte = [0];
        reader.read_exact(&mut byte)?;
        if byte[0] == b'#' && result.is_empty() {
            let mut comment = Vec::new();
            reader.read_until(b'\n', &mut comment)?;
            continue;
        }
        if byte[0].is_ascii_whitespace() {
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

    #[test]
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

    #[test]
    fn truncated_frame_and_oversized_header_fail() {
        assert!(read_ppm(&mut Cursor::new(b"P6\n1 1\n255\n\x00")).is_err());
        assert!(read_ppm(&mut Cursor::new(b"P6\n4294967295 4294967295\n255\n")).is_err());
    }
}
