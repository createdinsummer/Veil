use anyhow::Result;
use clap::Parser;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use veil_scrollshot::{Crop, StitchOptions, VideoOptions, convert_with_progress};

#[derive(Parser)]
#[command(
    version,
    about = "将垂直向下滚动的录屏拼接为 PNG 长截图",
    after_help = "示例：veil-scrollshot recording.mp4 -o long.png\n需要安装 FFmpeg，或通过 --ffmpeg /path/to/ffmpeg 指定路径。"
)]
struct Args {
    /// 输入录屏，支持 FFmpeg 能够解码的视频格式
    input: PathBuf,
    /// 输出 PNG；默认在录屏旁生成同名 .long.png 文件
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// 每秒抽样帧数；快速滚动时可提高到 8 或 12
    #[arg(long, default_value_t = 4.0)]
    fps: f64,
    /// 起始时间，单位：秒
    #[arg(long, default_value_t = 0.0)]
    start: f64,
    /// 结束时间，单位：秒
    #[arg(long)]
    end: Option<f64>,
    /// 顶部固定栏高度，单位：原始像素；默认自动识别
    #[arg(long)]
    crop_top: Option<u32>,
    /// 底部固定栏高度，单位：原始像素；默认自动识别
    #[arg(long)]
    crop_bottom: Option<u32>,
    /// 从每帧移除的左边缘宽度
    #[arg(long, default_value_t = 0)]
    crop_left: u32,
    /// 从每帧移除的右边缘宽度
    #[arg(long, default_value_t = 0)]
    crop_right: u32,
    /// 仅输出正文，移除顶部固定栏和底部固定栏
    #[arg(long)]
    content_only: bool,
    /// FFmpeg 路径；也支持 FFMPEG 环境变量、程序旁的 FFmpeg 或 PATH
    #[arg(long)]
    ffmpeg: Option<PathBuf>,
    /// 允许覆盖已有输出文件
    #[arg(long)]
    overwrite: bool,
    /// 输出图片最大高度
    #[arg(long, default_value_t = 200_000)]
    max_height: u32,
    /// 输出图片最大像素总量
    #[arg(long, default_value_t = 200_000_000)]
    max_pixels: u64,
    /// 匹配误差上限，范围 (0, 1)；越小越严格
    #[arg(long, default_value_t = 0.45)]
    max_match_error: f64,
    /// 隐藏处理进度
    #[arg(short, long)]
    quiet: bool,
}

fn run(args: Args) -> Result<()> {
    let output = args
        .output
        .unwrap_or_else(|| args.input.with_extension("long.png"));
    let options = VideoOptions {
        ffmpeg: args
            .ffmpeg
            .or_else(|| std::env::var_os("FFMPEG").map(PathBuf::from))
            .or_else(|| {
                let executable = std::env::current_exe().ok()?;
                let sibling = executable.parent()?.join(if cfg!(windows) {
                    "ffmpeg.exe"
                } else {
                    "ffmpeg"
                });
                sibling.is_file().then_some(sibling)
            })
            .unwrap_or_else(|| PathBuf::from("ffmpeg")),
        sample_fps: args.fps,
        start_seconds: args.start,
        end_seconds: args.end,
        crop_top: args.crop_top,
        crop_bottom: args.crop_bottom,
        overwrite: args.overwrite,
        stitch: StitchOptions {
            crop: Crop {
                left: args.crop_left,
                right: args.crop_right,
                ..Crop::default()
            },
            keep_fixed_bands: !args.content_only,
            max_height: args.max_height,
            max_pixels: args.max_pixels,
            max_match_error: args.max_match_error,
        },
    };
    let started = Instant::now();
    let mut updated = Instant::now();
    let report = convert_with_progress(&args.input, &output, &options, |frames, height| {
        if !args.quiet && (frames == 1 || updated.elapsed() >= Duration::from_secs(2)) {
            eprintln!("已处理 {frames} 帧，长图高度 {height} 像素");
            updated = Instant::now();
        }
    })?;
    println!(
        "输出：{}（{}×{}）",
        output.display(),
        report.width,
        report.height
    );
    if !args.quiet {
        println!(
            "抽样 {} 帧，拼接 {} 帧，静止 {} 帧，回滚/重访 {} 帧；耗时 {:.1} 秒",
            report.frames,
            report.stitched_frames,
            report.duplicate_frames,
            report.backtrack_frames,
            started.elapsed().as_secs_f64()
        );
        println!(
            "固定栏：顶部 {} 像素，底部 {} 像素；最大匹配误差 {:.3}",
            report.crop.top, report.crop.bottom, report.max_match_error
        );
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run(Args::parse()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("错误：{error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
