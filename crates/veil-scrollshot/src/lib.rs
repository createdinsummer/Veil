//! 将垂直向下滚动的录屏转换为 PNG 长截图。
//! 视频解码仅依赖外部 FFmpeg，逐帧拼接接口不依赖视频解码器或 Veil 核心库。

mod frame;
mod matching;
mod stitch;
mod video;

use anyhow::{Context, Result, ensure};
pub use frame::RgbFrame;
use std::path::{Path, PathBuf};
pub use stitch::Stitcher;

/// top/bottom 为只保留一次的固定栏；left/right 为每帧需要移除的边缘。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Crop {
    pub top: u32,
    pub bottom: u32,
    pub left: u32,
    pub right: u32,
}

impl Crop {
    fn validate(self, width: u32, height: u32) -> Result<()> {
        ensure!(
            self.left.checked_add(self.right).is_some_and(|n| n < width),
            "左右裁剪范围超出帧宽"
        );
        ensure!(
            self.top
                .checked_add(self.bottom)
                .is_some_and(|n| n < height),
            "固定栏范围超出帧高"
        );
        ensure!(
            width - self.left - self.right >= 32 && height - self.top - self.bottom >= 64,
            "可拼接区域至少需要 32×64 像素"
        );
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct StitchOptions {
    pub crop: Crop,
    pub keep_fixed_bands: bool,
    pub max_height: u32,
    pub max_pixels: u64,
    /// 归一化像素误差，越小越严格。
    pub max_match_error: f64,
}

impl Default for StitchOptions {
    fn default() -> Self {
        Self {
            crop: Crop::default(),
            keep_fixed_bands: true,
            max_height: 200_000,
            max_pixels: 200_000_000,
            max_match_error: 0.45,
        }
    }
}

impl StitchOptions {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.max_height > 0 && self.max_pixels > 0,
            "输出尺寸限制必须大于 0"
        );
        ensure!(
            self.max_match_error.is_finite()
                && self.max_match_error > 0.0
                && self.max_match_error < 1.0,
            "匹配误差上限必须在 0 到 1 之间"
        );
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct VideoOptions {
    pub ffmpeg: PathBuf,
    pub sample_fps: f64,
    pub start_seconds: f64,
    pub end_seconds: Option<f64>,
    /// None 表示自动识别，Some(0) 表示没有该固定栏。
    pub crop_top: Option<u32>,
    pub crop_bottom: Option<u32>,
    pub stitch: StitchOptions,
    pub overwrite: bool,
}

impl Default for VideoOptions {
    fn default() -> Self {
        Self {
            ffmpeg: PathBuf::from("ffmpeg"),
            sample_fps: 4.0,
            start_seconds: 0.0,
            end_seconds: None,
            crop_top: None,
            crop_bottom: None,
            stitch: StitchOptions::default(),
            overwrite: false,
        }
    }
}

/// 抽样帧总数包含首帧，拼接帧数包含首帧。
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub stitched_frames: u64,
    pub duplicate_frames: u64,
    pub backtrack_frames: u64,
    pub crop: Crop,
    pub max_match_error: f64,
}

/// 转换视频，返回输出尺寸和去重统计。
pub fn convert(
    input: impl AsRef<Path>,
    output: impl AsRef<Path>,
    options: &VideoOptions,
) -> Result<Report> {
    convert_with_progress(input, output, options, |_, _| {})
}

/// 每处理一帧回调一次，参数为抽样帧数、累计长图行数（不含末尾固定栏）。
pub fn convert_with_progress(
    input: impl AsRef<Path>,
    output: impl AsRef<Path>,
    options: &VideoOptions,
    mut progress: impl FnMut(u64, u32),
) -> Result<Report> {
    options.stitch.validate()?;
    ensure!(
        options.sample_fps.is_finite() && (0.25..=60.0).contains(&options.sample_fps),
        "--fps 必须在 0.25 到 60 之间"
    );
    ensure!(
        options.start_seconds.is_finite() && options.start_seconds >= 0.0,
        "起始时间必须为非负有限数值"
    );
    if let Some(end) = options.end_seconds {
        ensure!(
            end.is_finite() && end > options.start_seconds,
            "结束时间必须大于起始时间"
        );
    }
    let input = input.as_ref();
    let output = output.as_ref();
    ensure!(
        input.is_file(),
        "输入录屏不存在或不是文件：{}",
        input.display()
    );
    if output.exists() {
        ensure!(
            input.canonicalize()? != output.canonicalize()?,
            "输出路径不能与输入录屏相同"
        );
        ensure!(
            options.overwrite,
            "输出文件已存在；如需覆盖，请添加 --overwrite"
        );
    }
    let bands = if options.crop_top.is_none() || options.crop_bottom.is_none() {
        let mut probe = video::VideoReader::open(input, options, true)?;
        let mut frames = Vec::new();
        let mut previous: Option<RgbFrame> = None;
        while let Some(frame) = probe.next()? {
            if let Some(before) = previous {
                ensure!(
                    before.width() == frame.width() && before.height() == frame.height(),
                    "录屏中途改变了画面尺寸，无法拼接；请分别转换不同尺寸的片段"
                );
                if matching::probe_has_motion(&before, &frame) {
                    frames.push(before);
                    frames.push(frame.clone());
                }
            }
            previous = Some(frame);
            // 按实际运动收集样本，开头停留较久也能识别固定栏。
            if frames.len() >= 24 {
                break;
            }
        }
        matching::detect_fixed_bands(&frames)
    } else {
        (0.0, 0.0)
    };
    let mut video = video::VideoReader::open(input, options, false)?;
    let first = video.next()?.context("录屏或所选时间范围没有视频帧")?;
    let mut stitch_options = options.stitch.clone();
    stitch_options.crop.top = options
        .crop_top
        .unwrap_or((bands.0 * first.height() as f64).round() as u32);
    stitch_options.crop.bottom = options
        .crop_bottom
        .unwrap_or((bands.1 * first.height() as f64).round() as u32);
    let mut stitcher = Stitcher::new(stitch_options)?;
    stitcher.push(first)?;
    progress(1, stitcher.height());
    let mut count = 1;
    while let Some(frame) = video.next()? {
        let timestamp = options.start_seconds + count as f64 / options.sample_fps;
        stitcher
            .push(frame)
            .with_context(|| format!("在录屏 {timestamp:.2} 秒处拼接失败"))?;
        count += 1;
        progress(count, stitcher.height());
    }
    stitcher.finish(output, options.overwrite)
}
