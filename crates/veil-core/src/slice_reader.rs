//! 将底层 reader 限定到指定字节区间的适配器。
//!
//! 单文件容器中的每个 blob 只占大文件的一段 `[start, start + len)`。
//! [`SliceReader`] 为该区间提供独立的 `Read + Seek` 视图，使上层解密器无需感知
//! 容器布局，同时通过段内边界检查防止读取相邻 blob。

use std::io::{self, Read, Seek, SeekFrom};

/// 将底层 reader 限定到 `[start, start + len)` 的字节视图。
pub struct SliceReader<R> {
    /// 底层 reader。
    inner: R,
    /// 区间在底层 reader 中的起始偏移。
    start: u64,
    /// 区间长度。
    len: u64,
    /// 区间内当前位置。
    pos: u64,
}

impl<R: Seek> SliceReader<R> {
    /// 创建受限视图并将底层游标移动到区间起点。
    ///
    /// # 参数
    /// - `inner`：具备随机定位能力的底层 reader。
    /// - `start`：区间起始偏移。
    /// - `len`：区间长度。
    ///
    /// # 错误
    /// 底层定位失败时返回对应的 [`std::io::Error`]。
    pub fn new(mut inner: R, start: u64, len: u64) -> io::Result<Self> {
        // 构造时同步底层游标，之后 pos 始终只表达区间内相对位置。
        inner.seek(SeekFrom::Start(start))?;
        Ok(Self { inner, start, len, pos: 0 })
    }
}

impl<R: Read> Read for SliceReader<R> {
    /// 从当前区间位置读取，最多到达区间末尾。
    ///
    /// # 参数
    /// - `buf`：接收数据的调用方缓冲区。
    ///
    /// # 返回
    /// 返回实际读取的字节数；区间耗尽时返回 `Ok(0)`。底层读取失败时返回错误。
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // remaining 是区间内尚未读取的字节数，是防止越界读取的唯一边界。
        let remaining = self.len - self.pos;
        if remaining == 0 {
            return Ok(0);
        }

        // 先按区间剩余量裁剪，底层 reader 不可能读到相邻 blob。
        let max = remaining.min(buf.len() as u64) as usize;
        let n = self.inner.read(&mut buf[..max])?;
        // 只按本次实际读取量推进，兼容底层短读。
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Seek> Seek for SliceReader<R> {
    /// 在区间坐标系内定位游标，允许位置 `0..=len`。
    ///
    /// # 参数
    /// - `style`：相对于区间起点、末尾或当前位置的定位方式。
    ///
    /// # 返回
    /// 返回相对于区间起点的新位置。
    ///
    /// # 错误
    /// 目标位置小于 0 或大于区间长度时返回 [`io::ErrorKind::InvalidInput`]。
    fn seek(&mut self, style: SeekFrom) -> io::Result<u64> {
        // 先把三种基准统一换算为区间内绝对位置，再做一次边界校验。
        let new_pos: i64 = match style {
            SeekFrom::Start(offset) => offset as i64,
            SeekFrom::End(offset) => self.len as i64 + offset,
            SeekFrom::Current(offset) => self.pos as i64 + offset,
        };

        // 负位置表示越过区间起点，读取端不应看到底层文件的其他区域。
        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SliceReader: seek 到段起点之前",
            ));
        }
        let new_pos = new_pos as u64;
        // 等于 len 合法，表示 EOF；超过 len 则违反 SliceReader 契约。
        if new_pos > self.len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SliceReader: seek 越过段末尾",
            ));
        }

        self.inner.seek(SeekFrom::Start(self.start + new_pos))?;
        self.pos = new_pos;
        Ok(self.pos)
    }
}

/// 区间读取器边界和定位行为的单元测试。
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// 验证读取不会越过区间末尾。
    #[test]
    fn read_stays_within_slice() {
        // 底层 10 字节 [0,1,...,9]，取中间一段 [3, 3+4) = 字节 3..7
        let data: Vec<u8> = (0..10).collect();
        // 使用Cursor::new(data)把一段内存字节包装成一个「可读、可写、可 seek 的假文件」
        let mut reader = SliceReader::new(Cursor::new(data), 3, 4).unwrap();

        let mut out = Vec::new();
        reader.read_to_end(&mut out).unwrap(); // 反复 read 直到返回 0
        assert_eq!(out, vec![3, 4, 5, 6]); // 只读到这一段，没越界读到 7、8、9
    }

    /// 验证段内定位会映射到底层正确字节。
    #[test]
    fn seek_within_slice() {
        let data: Vec<u8> = (0..10).collect();
        // 使用Cursor::new(data)把一段内存字节包装成一个「可读、可写、可 seek 的假文件」
        let mut reader = SliceReader::new(Cursor::new(data), 3, 4).unwrap();

        // 段内坐标 2 → 底层第 5 字节
        reader.seek(SeekFrom::Start(2)).unwrap();// 设置段内游标到 2
        let mut one = [0u8; 1]; // 申请一个字节的缓冲区
        // read_exact 会读取，直到填满 one，否则返回错误
        reader.read_exact(&mut one).unwrap();// 读取一个字节，填满 one
        assert_eq!(one[0], 5);

        // 从末尾往前 1 → 段内位置 3 → 底层第 6 字节
        reader.seek(SeekFrom::End(-1)).unwrap();
        reader.read_exact(&mut one).unwrap();
        assert_eq!(one[0], 6);

        // 相对当前：读完在 len(=4)，往前 4 回到段内 0 → 底层第 3 字节
        reader.seek(SeekFrom::Current(-4)).unwrap();
        reader.read_exact(&mut one).unwrap();
        assert_eq!(one[0], 3);
    }

    /// 验证越界定位返回错误。
    #[test]
    fn seek_out_of_bounds_errors() {
        let data: Vec<u8> = (0..10).collect();
        let mut reader = SliceReader::new(Cursor::new(data), 3, 4).unwrap();

        assert!(reader.seek(SeekFrom::Start(5)).is_err()); // 超过 len=4
        assert!(reader.seek(SeekFrom::Current(-1)).is_err()); // 到段起点之前
    }
}
