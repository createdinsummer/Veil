//! 将垂直向下滚动的录屏转换为 PNG 长截图。
//! 视频解码仅依赖外部 FFmpeg，逐帧拼接接口不依赖视频解码器或 Veil 核心库。
//!
//! 文件作用：本文件是 veil-scrollshot 库的公共入口，汇总各子模块并对外提供
//! “录屏文件 → 长截图”和“已有 RGB 帧 → 长截图”两条业务链路。
//! 所属业务模块：长截图工具（scrollshot）的库层。
//! 核心结构体/函数清单：Crop（裁剪参数）、StitchOptions（拼接参数）、
//! VideoOptions（视频转换参数）、Report（转换统计报告）、convert /
//! convert_with_progress（视频转换高层入口）、Stitcher / RgbFrame（转发导出）。
//! 依赖能力：video 子模块驱动外部 FFmpeg 解码抽帧；frame 子模块承载 RGB24
//! 帧数据；matching 子模块做帧间匹配（找出两帧画面间的对应关系，即滚动位移）
//! 与固定栏识别；stitch 子模块负责拼接、回滚去重（用户回看时避免重复内容）
//! 和 PNG 流式写出。
//! 整体业务目标：把“垂直向下滚动的屏幕录屏”自动拼成一张完整长截图——自动
//! 识别顶部/底部固定栏（滚动时位置不变的 UI 区域，默认只保留一次），通过
//! 图像重叠计算滚动距离，跳过静止画面，回滚或重访已有内容时不重复追加，
//! 输出尺寸上限可配置，内存占用不随长图高度增长。

/// 帧数据子模块：RGB24 帧结构、亮度采样与 FFmpeg PPM 管道的解析。
mod frame;
/// 帧匹配子模块：帧间滚动位移估计、固定栏自动识别、画面运动探测。
mod matching;
/// 拼接子模块：帧条带磁盘暂存、接缝选择、回滚去重与 PNG 流式写出。
mod stitch;
/// 视频子模块：FFmpeg 子进程的启动、生命周期管理与按抽样率逐帧读取。
mod video;

use anyhow::{Context, Result, ensure};
/// 对外再导出帧类型，调用方可用 RgbFrame::new(width, height, pixels) 自建输入帧。
pub use frame::RgbFrame;
use std::path::{Path, PathBuf};
/// 对外再导出拼接器，已有解码器时可跳过 FFmpeg 直接逐帧喂入。
pub use stitch::Stitcher;

/// 每帧的裁剪参数：拼接时从每帧画面中剔除哪些区域。
/// 业务定位：顶部/底部是“只保留一次的固定栏”（滚动时位置不变、内容基本不变
/// 的 UI 区域，如状态栏、导航栏）；左侧/右侧是“每帧都要移除的边缘”（滚动条、
/// 窗口边框、悬浮按钮等会干扰帧匹配的内容）。
/// 使用场景：通过 StitchOptions.crop 配置；同时作为 Report 的组成部分，把最终
/// 实际生效的裁剪值回传给调用方。
/// top/bottom 为只保留一次的固定栏；left/right 为每帧需要移除的边缘。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Crop {
    /// 顶部固定栏高度（像素），0 表示没有顶部固定栏。
    pub top: u32,
    /// 底部固定栏高度（像素），0 表示没有底部固定栏。
    pub bottom: u32,
    /// 每帧需要移除的左边缘宽度（像素）。
    pub left: u32,
    /// 每帧需要移除的右边缘宽度（像素）。
    pub right: u32,
}

impl Crop {
    /// 确认本帧裁剪后仍有足够的可拼接正文，避免后续像素取样越界。
    /// 入参：self（Crop）按值复制裁剪量；width/height（u32）为源帧像素宽高。
    /// 返回值：Ok(()) 表示裁剪和不溢出、左右剩余至少 32 像素且上下剩余至少 64 行；否则 Err。
    /// 异常：checked_add 检测裁剪求和溢出并按非法范围返回错误，不执行文件或像素读取。
    /// 注意事项：只校验给定宽高，不判断帧数据长度，也不识别实际固定栏；合法的手工裁剪仍可能影响匹配质量。
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

/// 逐帧拼接的配置参数。
/// 业务定位：控制长图如何从帧序列生成——保留哪些固定栏、输出尺寸上限、
/// 匹配严格程度。不经过视频解码、直接使用 Stitcher 时用它构造拼接器，
/// 视频路径则把它嵌在 VideoOptions.stitch 中复用。
#[derive(Clone, Debug)]
pub struct StitchOptions {
    /// 裁剪参数：固定栏高度与左右边缘宽度。
    pub crop: Crop,
    /// 是否在长图中保留顶部/底部固定栏；false 时只输出正文区域。
    pub keep_fixed_bands: bool,
    /// 输出长图最大高度（像素）；限制可接受的图像行数，不等于对整个进程内存或临时磁盘的硬配额。
    pub max_height: u32,
    /// 输出长图最大像素总量（宽×高）；限制编码输入规模，压缩后 PNG 文件体积仍取决于内容。
    pub max_pixels: u64,
    /// 归一化像素误差，越小越严格。
    pub max_match_error: f64,
}

impl Default for StitchOptions {
    /// 给逐帧调用方提供一套可开始使用的拼接默认配置。
    /// 入参：无。
    /// 返回值：StitchOptions，无裁剪、保留固定栏、高度上限 200000、像素上限 200000000、匹配误差上限 0.45。
    /// 异常：不返回错误，不分配文件资源。
    /// 注意事项：这是固定默认值，不根据录屏尺寸或主机容量自动调优；直接逐帧 API 不会自动探测固定栏。
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
    /// 在拼接器开始分配暂存资源前校验输出上限和匹配容差。
    /// 入参：&self 为 StitchOptions，无其他参数。
    /// 返回值：max_height/max_pixels 均大于 0，且 max_match_error 为 (0,1) 内有限数时 Ok(())，否则 Err。
    /// 异常：仅返回配置校验错误，不访问磁盘、不捕获 panic。
    /// 注意事项：不校验 crop 与具体帧是否匹配，不保证机器有相应内存或磁盘容量；裁剪在 push 时按帧尺寸另行检查。
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

/// 视频转换的配置参数（命令行参数与库接口共用）。
/// 业务定位：描述“从哪段录屏、以什么频率抽帧、如何裁剪与拼接、写到哪”。
#[derive(Clone, Debug)]
pub struct VideoOptions {
    /// FFmpeg 可执行文件路径；查找失败时在启动阶段报错。
    pub ffmpeg: PathBuf,
    /// 每秒抽样帧数；越高越能应对快速滚动，但匹配计算量也越大。
    pub sample_fps: f64,
    /// 转换起始时间（秒，相对于录屏开头）。
    pub start_seconds: f64,
    /// 转换结束时间（秒）；None 表示一直处理到视频结尾。
    pub end_seconds: Option<f64>,
    /// None 表示自动识别，Some(0) 表示没有该固定栏。
    /// 顶部固定栏：自动识别的比例会在转换流程中换算成像素并写入 StitchOptions.crop.top。
    pub crop_top: Option<u32>,
    /// 底部固定栏：同上；None 自动识别，Some(0) 表示没有底部固定栏。
    pub crop_bottom: Option<u32>,
    /// 拼接参数（含左右裁剪、保留固定栏与否、输出上限、匹配误差）。
    pub stitch: StitchOptions,
    /// 输出文件已存在时是否覆盖；false 时直接报错，避免误毁旧结果。
    pub overwrite: bool,
}

impl Default for VideoOptions {
    /// 为录屏转换提供默认解码和拼接参数。
    /// 入参：无。
    /// 返回值：VideoOptions，ffmpeg 相对命令路径、4 fps、从 0 秒到结尾、上下固定栏自动探测、默认拼接配置且不覆盖目标。
    /// 异常：不返回 Result；路径值构造涉及分配，内存不足不能保证恢复。
    /// 注意事项：仅提供配置，未检查 FFmpeg 安装或输入视频；库直接使用配置中的路径，CLI 才额外解析 FFMPEG 环境变量和同目录程序。
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

/// 转换结果统计报告。
/// 业务定位：向调用方汇报长图尺寸、处理了多少帧、有多少帧被去重或判定为回滚。
/// 抽样帧总数包含首帧，拼接帧数包含首帧。
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// 输出长图宽度（像素），等于帧宽减去左右裁剪。
    pub width: u32,
    /// 输出长图高度（像素），包含固定栏（若保留）。
    pub height: u32,
    /// 读到的抽样帧总数（含首帧）。
    pub frames: u64,
    /// 实际拼接进长图的帧数（含首帧）。
    pub stitched_frames: u64,
    /// 静止帧数（与上一帧几乎相同、未增加新内容）。
    pub duplicate_frames: u64,
    /// 回滚/重访帧数（滚回已见过的内容，不重复拼接）。
    pub backtrack_frames: u64,
    /// 最终实际生效的裁剪参数。
    pub crop: Crop,
    /// 本次拼接中出现的最大帧间匹配误差，反映匹配质量。
    pub max_match_error: f64,
}

/// 把指定录屏转换为一张 PNG 长截图，返回尺寸及去重统计。
/// 入参：input（impl `AsRef<Path>`）为现存视频文件；output（同类型约束）为 PNG 目标；options（&VideoOptions）包含采样、裁剪和输出策略。
/// 返回值：成功 Ok(Report)；校验、解码、匹配、暂存或输出失败为 Err(anyhow::Error)，不提供进度回调。
/// 异常：库错误通过 Result 返回；分配失败、回调以外的 panic 等不在此捕获。
/// 注意事项：同步调用会占用当前线程，需可执行 FFmpeg 和可写临时目录；输出存在时须 overwrite，输出不应与输入指向同一文件；部分路径检查存在检查后被改动的时间窗口。
pub fn convert(
    input: impl AsRef<Path>,
    output: impl AsRef<Path>,
    options: &VideoOptions,
) -> Result<Report> {
    convert_with_progress(input, output, options, |_, _| {})
}

/// 将录屏抽帧、估计固定栏并逐帧拼接为长截图，同时报告正式拼接进度。
/// 入参：input（impl `AsRef<Path>`）现存视频文件；output（impl `AsRef<Path>`）PNG 目标；options（&VideoOptions）要求 fps 在 0.25..=60、开始时间为非负有限数、可选结束时间大于开始时间；progress（impl FnMut(u64,u32)）接收已成功处理抽样帧数及累计行数。
/// 返回值：Ok(Report) 含最终尺寸、有效裁剪及各类帧计数；失败返回带上下文的 Err(anyhow::Error)。
/// 异常：参数/路径校验、FFmpeg 启动或读取、尺寸变化、匹配、暂存及输出错误；回调 panic 不会被转换为 Result。
/// 注意事项：回调在调用此函数的同一线程同步执行，不是单独工作线程；探测阶段不回调，正式每帧成功 push 后回调。累计行数可含顶部固定栏但不含末尾固定栏，最后回调不代表 PNG 写出成功。
pub fn convert_with_progress(
    input: impl AsRef<Path>,
    output: impl AsRef<Path>,
    options: &VideoOptions,
    mut progress: impl FnMut(u64, u32),
) -> Result<Report> {
    // 先校验拼接参数与时间参数，把用户配置错误挡在解码之前。
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
        // 输出与输入是同一文件时，覆盖会把原始录屏毁掉，必须提前拦截。
        ensure!(
            input.canonicalize()? != output.canonicalize()?,
            "输出路径不能与输入录屏相同"
        );
        ensure!(
            options.overwrite,
            "输出文件已存在；如需覆盖，请添加 --overwrite"
        );
    }
    // 固定栏探测：当顶部或底部固定栏设为自动识别时，先用缩小画面扫一遍视频，
    // 收集“实际发生过滚动的相邻帧对”作为样本。
    let bands = if options.crop_top.is_none() || options.crop_bottom.is_none() {
        // 探测通道的帧会缩小到宽≤320，降低扫描成本；probe=true 开启缩小滤镜。
        let mut probe = video::VideoReader::open(input, options, true)?;
        // 收集有运动的相邻帧对，供固定栏识别算法统计“哪些行在变、哪些行不变”。
        // 每两项是一对有运动的相邻探测帧；最多收集 24 项，同一画面可因相邻配对重复，非全视频帧列表。
        let mut frames = Vec::new();
        let mut previous: Option<RgbFrame> = None;
        while let Some(frame) = probe.next()? {
            if let Some(before) = previous {
                ensure!(
                    before.width() == frame.width() && before.height() == frame.height(),
                    "录屏中途改变了画面尺寸，无法拼接；请分别转换不同尺寸的片段"
                );
                if matching::probe_has_motion(&before, &frame) {
                    // 相邻两帧都收下，识别算法按帧对统计行差异。
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
        // 返回 (顶部固定栏比例, 底部固定栏比例)，都是相对帧高的 0~1 比值。
        matching::detect_fixed_bands(&frames)
    } else {
        // 两个固定栏都已手动指定时无需探测，占位为 0。
        (0.0, 0.0)
    };
    // 正式转换通道：不开缩小滤镜，按原始分辨率逐帧读取。
    let mut video = video::VideoReader::open(input, options, false)?;
    let first = video.next()?.context("录屏或所选时间范围没有视频帧")?;
    let mut stitch_options = options.stitch.clone();
    // 把探测得到的“比例”换算成首帧的“像素高度”；手动指定时直接采用像素值。
    stitch_options.crop.top = options
        .crop_top
        .unwrap_or((bands.0 * first.height() as f64).round() as u32);
    stitch_options.crop.bottom = options
        .crop_bottom
        .unwrap_or((bands.1 * first.height() as f64).round() as u32);
    let mut stitcher = Stitcher::new(stitch_options)?;
    // 首帧整体作为长图开头（含顶部固定栏或直接从正文开始）。
    stitcher.push(first)?;
    progress(1, stitcher.height());
    // 已成功推入正式拼接器的帧数，首帧已经计入；探测通道读过的帧不计入此进度。
    let mut count = 1;
    // 逐帧喂给拼接器；拼接器内部完成帧匹配、滚动距离估计与回滚去重。
    while let Some(frame) = video.next()? {
        // 用抽样帧序号反推该帧对应的时间点，拼进错误信息方便用户定位问题片段。
        let timestamp = options.start_seconds + count as f64 / options.sample_fps;
        stitcher
            .push(frame)
            .with_context(|| format!("在录屏 {timestamp:.2} 秒处拼接失败"))?;
        count += 1;
        progress(count, stitcher.height());
    }
    stitcher.finish(output, options.overwrite)
}
