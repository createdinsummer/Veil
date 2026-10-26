use crate::{Report, RgbFrame, StitchOptions, matching::Features};
use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::Path,
};

/// 逐帧拼接器。RGB 条带暂存到磁盘，内存占用不随长图高度增长。
pub struct Stitcher {
    options: StitchOptions,
    raw: File,
    previous: Option<Features>,
    source_size: (u32, u32),
    position: i64,
    furthest: i64,
    rows: u32,
    footer: Vec<u8>,
    report: Report,
}

impl Stitcher {
    pub fn new(options: StitchOptions) -> Result<Self> {
        options.validate()?;
        Ok(Self {
            report: Report {
                crop: options.crop,
                ..Report::default()
            },
            options,
            raw: tempfile::tempfile()?,
            previous: None,
            source_size: (0, 0),
            position: 0,
            furthest: 0,
            rows: 0,
            footer: Vec::new(),
        })
    }

    pub fn height(&self) -> u32 {
        self.rows
    }

    /// 返回本帧新增加的正文行数；停留和回滚返回 0。
    pub fn push(&mut self, frame: RgbFrame) -> Result<u32> {
        self.options.crop.validate(frame.width(), frame.height())?;
        let crop = self.options.crop;
        let end = frame.height() - crop.bottom;
        if self.previous.is_some() {
            ensure!(
                self.source_size == (frame.width(), frame.height()),
                "录屏中途改变了画面尺寸，无法拼接；请分别转换不同尺寸的片段"
            );
        }
        let features = Features::new(&frame, crop);
        if self.previous.is_none() {
            self.source_size = (frame.width(), frame.height());
            self.report.width = frame.width() - crop.left - crop.right;
            let start = if self.options.keep_fixed_bands {
                0
            } else {
                crop.top
            };
            self.check_size(end - start)?;
            self.append(&frame, start, end)?;
            self.save_footer(&frame);
            self.previous = Some(features);
            self.report.frames = 1;
            self.report.stitched_frames = 1;
            return Ok(end - start);
        }
        let motion = self.previous.as_ref().unwrap().motion_to(&features);
        ensure!(
            motion.error <= self.options.max_match_error,
            "画面没有可靠的垂直重叠（匹配误差 {:.3}，上限 {:.3}）；请提高 --fps、调整固定栏范围，或用 --start/--end 排除切屏",
            motion.error,
            self.options.max_match_error
        );
        ensure!(
            motion.shift == 0 || motion.texture >= 1.0,
            "画面缺少可匹配的纹理，无法确定滚动距离"
        );
        self.report.max_match_error = self.report.max_match_error.max(motion.error);
        let position = self
            .position
            .checked_add(motion.shift as i64)
            .context("滚动位置溢出")?;
        let added = (position - self.furthest).max(0) as u32;
        if added > 0 {
            let body_height = end - crop.top;
            ensure!(
                added < body_height,
                "相邻帧滚动距离超过可拼接区域；请提高 --fps"
            );
            self.check_size(self.rows.checked_add(added).context("长图高度溢出")?)?;
            let overlap = body_height - added;
            let seam = self.choose_seam(&frame, crop.top, overlap);
            let trim = overlap - seam;
            let bytes = (self.rows - trim) as u64 * self.report.width as u64 * 3;
            self.raw.set_len(bytes)?;
            self.raw.seek(SeekFrom::Start(bytes))?;
            self.rows -= trim;
            self.append(&frame, crop.top + seam, end)?;
            self.furthest = position;
            self.report.stitched_frames += 1;
            self.save_footer(&frame);
        } else if motion.shift == 0 {
            self.report.duplicate_frames += 1;
            if position == self.furthest {
                self.save_footer(&frame);
            }
        } else {
            self.report.backtrack_frames += 1;
        }
        self.position = position;
        self.previous = Some(features);
        self.report.frames += 1;
        Ok(added)
    }

    /// 原子写出 PNG。默认拒绝覆盖已有文件，失败时清理暂存文件。
    pub fn finish(mut self, output: impl AsRef<Path>, overwrite: bool) -> Result<Report> {
        ensure!(self.previous.is_some(), "录屏或所选时间范围没有视频帧");
        if self.options.keep_fixed_bands {
            self.raw.write_all(&self.footer)?;
            self.rows += self.options.crop.bottom;
        }
        self.report.height = self.rows;
        self.raw.seek(SeekFrom::Start(0))?;
        let output = output.as_ref();
        let parent = output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).context("无法创建输出暂存文件")?;
        {
            let mut encoder =
                png::Encoder::new(temporary.as_file_mut(), self.report.width, self.rows);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_compression(png::Compression::Balanced);
            let mut writer = encoder.write_header()?;
            {
                let mut stream = writer.stream_writer()?;
                std::io::copy(&mut self.raw, &mut stream)?;
                stream.finish()?;
            }
            writer.finish()?;
        }
        temporary.as_file().sync_all()?;
        if overwrite {
            temporary.persist(output).context("无法保存 PNG 文件")?;
        } else {
            temporary
                .persist_noclobber(output)
                .context("无法保存 PNG 文件（目标可能已存在）")?;
        }
        Ok(self.report)
    }

    fn append(&mut self, frame: &RgbFrame, start: u32, end: u32) -> Result<()> {
        let left = self.options.crop.left as usize * 3;
        let right = (frame.width() - self.options.crop.right) as usize * 3;
        for y in start..end {
            self.raw.write_all(&frame.row(y as usize)[left..right])?;
        }
        self.rows += end - start;
        Ok(())
    }

    fn save_footer(&mut self, frame: &RgbFrame) {
        if !self.options.keep_fixed_bands {
            return;
        }
        let crop = self.options.crop;
        self.footer.clear();
        for y in frame.height() - crop.bottom..frame.height() {
            let row = frame.row(y as usize);
            self.footer.extend_from_slice(
                &row[crop.left as usize * 3..(frame.width() - crop.right) as usize * 3],
            );
        }
    }

    fn check_size(&self, rows: u32) -> Result<()> {
        let footer = if self.options.keep_fixed_bands {
            self.options.crop.bottom
        } else {
            0
        };
        let height = rows.checked_add(footer).context("长图高度溢出")?;
        ensure!(
            height <= self.options.max_height,
            "长图高度将超过 {} 像素；请缩短时间范围或调整 --max-height",
            self.options.max_height
        );
        ensure!(
            height as u64 * self.report.width as u64 <= self.options.max_pixels,
            "长图将超过 {} 像素的总量限制；请缩短时间范围或调整 --max-pixels",
            self.options.max_pixels
        );
        Ok(())
    }

    fn choose_seam(&self, frame: &RgbFrame, top: u32, overlap: u32) -> u32 {
        // 在重叠区域内选择文字较少的水平行，同时替换旧帧尾部，避免接缝切过笔画。
        let mut best = overlap;
        let mut best_score = u64::MAX;
        let width = self.report.width as usize;
        let left = self.options.crop.left as usize;
        for seam in (overlap.saturating_sub(128)..=overlap).rev() {
            let y = (top + seam) as usize;
            if y == 0 || y + 1 >= (frame.height() - self.options.crop.bottom) as usize {
                continue;
            }
            let score: u64 = (0..64)
                .map(|i| {
                    let x = left + width * (i + 1) / 65;
                    let value = frame.luma(x, y);
                    value.abs_diff(frame.luma(x, y - 1)) as u64
                        + value.abs_diff(frame.luma(x, y + 1)) as u64
                        + value.abs_diff(frame.luma(x.saturating_sub(1), y)) as u64
                })
                .sum();
            if score < best_score {
                best_score = score;
                best = seam;
            }
        }
        best
    }
}
