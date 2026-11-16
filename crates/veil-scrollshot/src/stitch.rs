//! 文件作用：长截图流程的拼接子模块——把逐帧喂入的 RGB 帧按滚动位移拼进
//! 一张持续变高的长图，处理回滚去重（用户回看时避免重复内容），最终流式
//! 编码为 PNG。
//! 所属业务模块：长截图工具（scrollshot）的拼接层。
//! 核心结构体/函数清单：Stitcher（逐帧拼接器）及其 push（追加一帧）、
//! finish（写出 PNG）、append（写条带）、save_footer（暂存底部固定栏）、
//! choose_seam（接缝选择）、check_size（尺寸上限）。
//! 依赖能力：matching 模块的帧匹配（滚动位移估计）、frame 模块的 RgbFrame、
//! png 编码器、tempfile 临时文件。
//! 业务目标：帧条带先追加到临时文件、完成后再一次性编码 PNG，使内存占用
//! 不随长图高度增长；输出用“临时文件 + 原子改名”保证失败时不留下半张图。
use crate::{Report, RgbFrame, StitchOptions, matching::Features};
use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::Path,
};

/// 逐帧拼接器。RGB 条带暂存到磁盘，内存占用不随长图高度增长。
/// 业务定位：长图拼接的“状态机”——记住当前拼到哪、见过的最远位置、上一帧
/// 特征和底部固定栏内容，据此决定每帧追加多少行或整帧跳过。
pub struct Stitcher {
    /// 拼接参数（裁剪、保留固定栏、尺寸上限、匹配误差上限）。
    options: StitchOptions,
    /// 已拼接正文条带的原始 RGB 暂存文件（临时文件），finish 时被编码进 PNG。
    raw: File,
    /// 上一帧的匹配特征，用于与本帧做帧匹配得出滚动位移。
    previous: Option<Features>,
    /// 帧源尺寸（宽, 高）：所有后续帧必须与首帧一致。
    source_size: (u32, u32),
    /// 当前滚动位置：以“长图行”为单位，本帧滚动位移会累积到它上面。
    position: i64,
    /// 已见过的最大滚动位置：position ≤ furthest 说明在回滚或重访，无需追加。
    furthest: i64,
    /// 当前 raw 文件中的正文行数（不含底部固定栏）。
    rows: u32,
    /// 底部固定栏的最新条带（RGB 字节），finish 时追加到长图末尾。
    footer: Vec<u8>,
    /// 统计报告，push/finish 过程中不断更新。
    report: Report,
}

impl Stitcher {
    /// 为逐帧拼接建立独立状态及磁盘 RGB 条带暂存。
    /// 入参：options（StitchOptions）按值接管裁剪、固定栏、误差与输出上限。
    /// 返回值：Ok(Stitcher)，未接收帧，位置/行数为零，报告记录给定裁剪；失败为 Err。
    /// 异常：配置数值校验或系统临时文件创建失败返回错误，不捕获分配失败等 panic。
    /// 注意事项：crop 需在首帧按实际尺寸另校验；raw 由临时文件机制管理，生命周期结束关闭，不提供暂停后恢复文件路径。
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

    /// 读取当前已暂存的长图行数，供进度展示。
    /// 入参：&self 只读拼接器，无其他参数。
    /// 返回值：u32，已写入 raw 的行数，可包含顶部固定栏，不含尚未追加的底部固定栏。
    /// 异常：不返回错误。
    /// 注意事项：不是最终 PNG 高度；没有内部锁，并发使用由 Rust 借用约束和调用方同步负责。
    pub fn height(&self) -> u32 {
        self.rows
    }

    /// 按相邻帧位移判断是否出现未见正文，并把新内容拼入暂存长图。
    /// 入参：&mut self 独占拼接器；frame（RgbFrame）按值接管本帧，尺寸须与首帧一致，裁剪后正文至少 32×64。
    /// 返回值：Ok(u32) 为净新增行数，静止/回滚或未超越最远位置时为零；首帧值可包含顶部固定栏，不含底栏。
    /// 异常：裁剪/尺寸错误、匹配不可靠、纹理不足、位移/行数溢出、输出规模超限或暂存 I/O 失败返回 Err。
    /// 注意事项：不是事务回滚接口；截短旧尾或追加失败前可能已经修改 raw/计数，错误后应终止本次拼接。只接受同步独占调用，报告帧计数通常在成功路径末尾更新。
    pub fn push(&mut self, frame: RgbFrame) -> Result<u32> {
        // 每帧都校验一次裁剪参数：防止后续帧尺寸变化导致裁剪越界。
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
            // 首帧路径：整帧作为长图开头。
            self.source_size = (frame.width(), frame.height());
            self.report.width = frame.width() - crop.left - crop.right;
            // 保留固定栏时从第 0 行开始（含顶部固定栏）；只输出正文则跳过顶部。
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
        // 帧匹配：得出本帧相对上一帧向上滚动了多少行。
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
        // 新滚动位置 = 旧位置 + 本帧位移；位置可能比之前小（用户回滚）。
        let position = self
            .position
            .checked_add(motion.shift as i64)
            .context("滚动位置溢出")?;
        // 回滚去重的关键：只有超出“已见过的最大位置”的部分才是新内容。
        let added = (position - self.furthest).max(0) as u32;
        if added > 0 {
            let body_height = end - crop.top;
            ensure!(
                added < body_height,
                "相邻帧滚动距离超过可拼接区域；请提高 --fps"
            );
            self.check_size(self.rows.checked_add(added).context("长图高度溢出")?)?;
            // 本帧与长图末尾的重叠行数 = 正文高度 - 新增行数。
            let overlap = body_height - added;
            // 在重叠区域里挑一条“最不易切过文字笔画”的水平线作为接缝。
            let seam = self.choose_seam(&frame, crop.top, overlap);
            // 旧长图尾部需要截掉的部分：重叠区中接缝以下的旧内容被新帧取代。
            let trim = overlap - seam;
            // 把 raw 文件截短到“保留行数”对应的字节位置，实现旧尾部替换。
            let bytes = (self.rows - trim) as u64 * self.report.width as u64 * 3;
            self.raw.set_len(bytes)?;
            self.raw.seek(SeekFrom::Start(bytes))?;
            self.rows -= trim;
            // 从本帧的接缝处开始，把新条带追加到 raw 末尾。
            self.append(&frame, crop.top + seam, end)?;
            self.furthest = position;
            self.report.stitched_frames += 1;
            self.save_footer(&frame);
        } else if motion.shift == 0 {
            // 位移为 0 且没有新内容：画面静止，只刷新统计。
            self.report.duplicate_frames += 1;
            if position == self.furthest {
                // 停在最远位置时，底部固定栏可能有动画，取最新一帧覆盖。
                self.save_footer(&frame);
            }
        } else {
            // 有位移但没超过最远位置：用户回滚或重访旧内容，不重复拼接。
            self.report.backtrack_frames += 1;
        }
        self.position = position;
        self.previous = Some(features);
        self.report.frames += 1;
        Ok(added)
    }

    /// 将暂存条带及可选底栏编码为 PNG，再发布到目标位置。
    /// 入参：self 按值消费；output（impl `AsRef<Path>`）为 PNG 目标，父目录需存在；overwrite（bool）决定替换或拒绝已有目标。
    /// 返回值：Ok(Report) 表示编码、文件 sync_all 及发布成功，含最终高度；失败返回 Err(anyhow::Error)。
    /// 异常：没有输入帧、暂存读写/定位、临时文件创建、PNG 编码或发布失败；不捕获 panic。
    /// 注意事项：同目录临时文件避免直接暴露半成品，overwrite=false 用 persist_noclobber。文件同步不包含父目录同步；发布失败的 PersistError 可持有临时文件直到错误对象释放，不能承诺返回瞬间所有暂存都已消失。
    pub fn finish(mut self, output: impl AsRef<Path>, overwrite: bool) -> Result<Report> {
        ensure!(self.previous.is_some(), "录屏或所选时间范围没有视频帧");
        if self.options.keep_fixed_bands {
            // 底部固定栏只在最后追加一次，保证它只保留一份。
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
        // 临时文件建在输出同目录，保证 rename 是同一文件系统内的原子操作。
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
                // 流式写入：把 raw 文件的字节边读边喂给编码器，不整张载入内存。
                let mut stream = writer.stream_writer()?;
                std::io::copy(&mut self.raw, &mut stream)?;
                stream.finish()?;
            }
            writer.finish()?;
        }
        // 先同步临时文件内容再发布；不在此同步父目录，不能据此保证断电后目录项一定持久。
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

    /// 把一帧指定行区间的正文像素追加到磁盘暂存条带。
    /// 入参：&mut self 独占拼接器；frame:&RgbFrame 源像素；start/end:u32 为合法半开行区间，须 start<=end<=帧高，裁剪需已校验。
    /// 返回值：Ok(()) 表示所有行写入后 rows 增加 end-start；I/O 失败返回 Err。
    /// 异常：越界切片/行号或非法区间可 panic；写入错误返回后可能已写入部分字节。
    /// 注意事项：rows 只在全部写完后更新，因此失败时文件长度与计数可能暂时不一致；发生错误后不应假定可继续 push 恢复。
    fn append(&mut self, frame: &RgbFrame, start: u32, end: u32) -> Result<()> {
        let left = self.options.crop.left as usize * 3;
        let right = (frame.width() - self.options.crop.right) as usize * 3;
        // 每轮一行源 RGB24 像素，按左右像素数乘 3 得到字节边界，移除边缘后写入 raw。
        for y in start..end {
            self.raw.write_all(&frame.row(y as usize)[left..right])?;
        }
        self.rows += end - start;
        Ok(())
    }

    /// 保存与当前最远正文相配的最新底部固定栏，供完成时只输出一次。
    /// 入参：&mut self 独占拼接器；frame:&RgbFrame，尺寸和裁剪须已验证。
    /// 返回值：()；不保留固定栏时不处理，否则清空 footer 后逐行保存裁剪后的 RGB 字节。
    /// 异常：无 Result，分配失败可中止，非法裁剪/行切片可 panic。
    /// 注意事项：只在首帧、新最远位置或停在最远位置的静止帧更新；回滚位置的底栏不一定覆盖现有值。
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

    /// 在写入新条带前检查预计最终图像规模。
    /// 入参：&self；rows:u32 预计 raw 行数，可能含顶部固定栏，尚不含底栏。
    /// 返回值：满足 max_height/max_pixels 时 Ok(())，加底栏溢出或超限返回 Err。
    /// 异常：只返回参数/规模错误，不访问磁盘。
    /// 注意事项：按 keep_fixed_bands 决定是否加 crop.bottom；宽度来自首帧 report.width，不表示检查临时磁盘可用容量。
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

    /// 在重叠区挑选亮度变化较小的横行，减少接缝切过文字的机会。
    /// 入参：&self；frame:&RgbFrame 当前帧；top:u32 正文起始行；overlap:u32 新旧重叠行数，需来自已验证匹配。
    /// 返回值：u32，重叠区偏移，追加从 top+偏移 开始；没有有效候选时返回 overlap。
    /// 异常：无 Result；非法尺寸或像素访问可 panic，调用方应保证裁剪和重叠范围。
    /// 注意事项：从重叠区末端向前至多 128 行（含端点至多 129 个候选），每候选取 64 列计算上下/左邻亮度差；相同分数保留更靠下的候选，不是文字识别。
    fn choose_seam(&self, frame: &RgbFrame, top: u32, overlap: u32) -> u32 {
        // 在重叠区域内选择文字较少的水平行，同时替换旧帧尾部，避免接缝切过笔画。
        let mut best = overlap;
        let mut best_score = u64::MAX;
        let width = self.report.width as usize;
        let left = self.options.crop.left as usize;
        // 逐个检查靠近重叠区底部的候选缝，排除上下邻行不在正文内的边界。
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
