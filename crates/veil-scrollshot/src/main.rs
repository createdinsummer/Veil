//! 文件作用：veil-scrollshot 的命令行入口——解析参数、组装转换配置、调用
//! 库接口并把进度与统计结果打印到终端。
//! 所属业务模块：长截图工具（scrollshot）的 CLI 层。
//! 核心结构体/函数清单：Args（命令行参数定义）、run（一次转换的执行体）、
//! main（日志初始化与错误出口）。
//! 依赖能力：clap 负责参数解析，veil_scrollshot 库负责转换，veil_core 日志
//! 负责错误链记录与落盘。
//! 业务目标：让用户在终端用一条命令把录屏转成长截图，失败时给出可定位
//! （时间点、FFmpeg 诊断）的错误信息。
use anyhow::Result;
use clap::Parser;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use veil_scrollshot::{Crop, StitchOptions, VideoOptions, convert_with_progress};

/// 命令行参数集合（由 clap 从命令行生成）。
/// 业务定位：保存一次录屏转换请求，由 clap 解析选项后交给 run 组装配置。
/// 核心成员分为输入/输出位置、时间抽样范围、四边裁剪、输出资源限制及进度开关；
/// 不保存图像像素，不启动 FFmpeg，参数组合有效性由后续转换选项校验。
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

/// 把已解析命令行选项转换为库配置，执行录屏拼接并显示结果。
/// 入参：args（Args）按值取得输入/输出路径、FFmpeg 选择、采样与裁剪参数、覆盖及 quiet 等选项。
/// 返回值：Ok(()) 表示转换和结果打印流程结束；转换错误返回 Err(anyhow::Error)，可能在输出前失败。
/// 异常：参数业务校验、FFmpeg、匹配、临时文件和 PNG 错误由库传播；println!/eprintln! 输出失败可 panic，不在此转 Result。
/// 注意事项：同步占用当前线程；首帧后进度最多每两秒一次，但 quiet 全部关闭进度和详细统计，仍打印最终输出路径。默认输出替换原扩展名为 long.png，不是保留原扩展名后追加。
fn run(args: Args) -> Result<()> {
    // 未指定 -o 时与输入同目录，将原扩展名替换为 long.png，例如 recording.mp4 → recording.long.png。
    let output = args
        .output
        .unwrap_or_else(|| args.input.with_extension("long.png"));
    let options = VideoOptions {
        // FFmpeg 查找顺序：--ffmpeg 参数 → FFMPEG 环境变量 →
        // 可执行程序所在目录（Windows 找 ffmpeg.exe）→ 系统 PATH。
        // 只判断候选是否存在，不在本层验证可执行性；显式路径或环境值无效时不会自动继续回退。
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
            // --content-only 表示只保留正文，即不保留顶部/底部固定栏。
            keep_fixed_bands: !args.content_only,
            max_height: args.max_height,
            max_pixels: args.max_pixels,
            max_match_error: args.max_match_error,
        },
    };
    // 总耗时从进入库转换前开始，包括固定栏探测、正式拼接及输出编码，不含参数解析。
    let started = Instant::now();
    // 最近一次进度打印的单调时刻，仅用于节流；不作为视频时间戳或任务超时。
    let mut updated = Instant::now();
    let report = convert_with_progress(&args.input, &output, &options, |frames, height| {
        // 首帧必打；之后每 2 秒打一次，展示已处理帧数与当前长图高度。
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
        // 统计口径：静止帧与回滚帧都不追加内容；拼接帧数含首帧。
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

/// 初始化诊断日志并执行一次长截图命令，将业务错误转为进程失败状态。
/// 入参：无 Rust 参数；Args::parse 从进程 argv 读取选项。
/// 返回值：ExitCode::SUCCESS 表示 run 成功，业务错误返回 FAILURE；帮助、版本或参数解析错误由 clap 自行退出。
/// 异常：未捕获 panic；参数解析可直接结束进程，错误显示或日志初始化异常不由此兜底。
/// 注意事项：仅 run 返回 Err 的分支请求日志 flush，并忽略刷新错误；不等于所有记录已物理落盘。真实转换依赖外部 FFmpeg 与可写输出父目录。
fn main() -> std::process::ExitCode {
    veil_core::logging::initialize();
    match run(Args::parse()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            veil_core::logging::error_chain("scrollshot", "capture", error.as_ref());
            eprintln!("错误：{error:#}");
            // 尽力等待日志队列刷新，失败忽略；flush 不等于强制物理磁盘同步。
            let _ = veil_core::logging::flush();
            std::process::ExitCode::FAILURE
        }
    }
}
