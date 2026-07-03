//! # slice_reader —— 把底层 reader 限定到某一段的适配器（Read + Seek）
//!
//! 容器是一个大文件，但每个 blob 只占其中 `[start, start+len)`。`SliceReader`
//! 把底层 reader「裁剪」成只暴露这一段，对外表现得像一个独立的 len 字节小文件：
//! 读不会越界到隔壁 blob，seek 用段内坐标（0..len）。
//! 把它交给 `age::Decryptor` 即可只解密这一个 blob，完全不碰其他数据。

use std::io::{self, Read, Seek, SeekFrom};

/// 把底层 reader `R` 限定到 `[start, start+len)` 一段。
pub struct SliceReader<R> {
    inner: R, // 底层 reader（需可 Seek）
    start: u64, // 段在底层的起始偏移
    len: u64,   // 段长度
    pos: u64, // 段内当前位置：0 表示段起点，len 表示段末尾
}

impl<R: Seek> SliceReader<R> {
    /// 新建：把底层 reader 定位到段起点。
    ///
    /// # 参数
    /// - `inner`: 底层 reader（需可 Seek）
    /// - `start`: 段在底层的起始偏移
    /// - `len`:   段长度
    /// # 返回
    /// - `Ok(self)`：新建的 `SliceReader`
    pub fn new(mut inner: R, start: u64, len: u64) -> io::Result<Self> {
        inner.seek(SeekFrom::Start(start))?; // 底层游标移到段起点
        Ok(Self { inner, start, len, pos: 0 })
    }
}

impl<R: Read> Read for SliceReader<R> {
    /// 从底层 reader 读，但绝不越过段末尾（`[start, start+len)`）。
    ///
    /// `buf` 是调用方递来的**空桶**，`read` 往桶里倒数据、返回倒了几勺（`n`）；
    /// 桶多大（`buf.len()`）决定这次最多倒多少，井干了（段读完）就倒 0 勺（EOF）。
    /// 调用方可以拿同一个桶反复来打水，直到返回 0。
    ///
    /// # 参数
    /// - `buf`: 存放读到字节的缓冲区（调用方提供的“桶”，`read` 只往里写、不分配）
    ///
    /// # 返回
    /// - `Ok(n)`：实际读到 `n` 字节（可能小于 `buf` 长度）。
    ///   `Ok(0)` 表示已到段末尾（EOF）——注意这是正常结束，**不是错误**。
    /// - `Err(e)`：底层 reader 读取出错时才返回。
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // 段内还剩多少没读
        let remaining = self.len - self.pos;
        if remaining == 0 {
            return Ok(0); // 到段末尾 → 返回 0 表示 EOF
        }

        // 这次最多读 min(调用方缓冲大小, 段内剩余)，绝不越界
        let max = remaining.min(buf.len() as u64) as usize;
        let n = self.inner.read(&mut buf[..max])?;
        // 用底层"实际读到的 n"推进段内位置（read 可能短读，n 可能 < max）
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Seek> Seek for SliceReader<R> {
    /// 在「段内坐标系」里定位游标：0 表示段起点，`len` 表示段末尾。
    ///
    /// 流程：根据 `SeekFrom` 选定基点（起点 / 末尾 / 当前）→ 算出目标的段内绝对位置
    /// → 校验不越界 → 把底层游标移到 `start + 段内位置`、并更新段内游标 → 返回段内位置。
    ///
    /// # 参数
    /// - `style`: 定位方式——`Start(n)` 从段起点、`End(n)` 从段末尾、`Current(n)` 从当前位置偏移
    ///
    /// # 返回
    /// - `Ok(new_pos)`：定位后的新位置（相对段起点的字节数），符合 `Seek` 契约。
    /// - `Err`（`InvalidInput`）：越界时返回——定位到段起点之前（结果为负），
    ///   或超过段末尾（`> len`）。允许正好定位到 `len`（段末尾），之后 read 返回 EOF。
    fn seek(&mut self, style: SeekFrom) -> io::Result<u64> {
        // 1) 把三种 SeekFrom 都换算成「段内绝对位置」（可能为负 / 越界，用 i64 中转）
        let new_pos: i64 = match style {
            SeekFrom::Start(offset) => offset as i64, // 从段起点
            SeekFrom::End(offset) => self.len as i64 + offset, // 从段末尾
            SeekFrom::Current(offset) => self.pos as i64 + offset, // 从当前位置
        };

        // 2) 不允许定位到段起点之前（负数）
        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SliceReader: seek 到段起点之前",
            ));
        }
        let new_pos = new_pos as u64;
        // 允许定位到 len（正好段末尾，之后 read 返回 EOF）；但不允许超过 len
        if new_pos > self.len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SliceReader: seek 越过段末尾",
            ));
        }

        // 3) 把底层游标移到 start + 段内偏移，并记录段内位置
        self.inner.seek(SeekFrom::Start(self.start + new_pos))?;
        self.pos = new_pos;
        Ok(self.pos) // 按 Seek 契约，返回新的（段内）位置
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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

    #[test]
    fn seek_out_of_bounds_errors() {
        let data: Vec<u8> = (0..10).collect();
        let mut reader = SliceReader::new(Cursor::new(data), 3, 4).unwrap();

        assert!(reader.seek(SeekFrom::Start(5)).is_err()); // 超过 len=4
        assert!(reader.seek(SeekFrom::Current(-1)).is_err()); // 到段起点之前
    }
}
