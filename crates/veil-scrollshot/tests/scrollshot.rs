//! 滚动截图拼接与视频转换命令的端到端测试。
//! 所属模块：scrollshot 像素处理与命令行入口；构造可预测的彩色帧，验证裁剪、回退、拼接和原子输出。
//! 像素素材由测试生成；常规拼接不依赖真实屏幕，设置 FFMPEG 后的视频回环用例会启动外部编码器并写入临时视频。
//! 核心函数：page_pixel/frame/expected 构造可预测像素，decode 读取输出；各测试比较结果图和报告。
//! 依赖能力：RgbFrame、拼接/转换接口、PNG 解码、临时目录与命令行子进程。
use std::{
    fs,
    io::{BufReader, Write},
    path::Path,
    process::{Command, Stdio},
};
use veil_scrollshot::{Crop, RgbFrame, StitchOptions, Stitcher, VideoOptions, convert};

// 合成帧宽度及正文/固定栏高度，单位均为像素；不同辅助函数共享这些值以生成可比较布局。
const WIDTH: u32 = 128;
const BODY: u32 = 400;
const TOP: u32 = 40;
const BOTTOM: u32 = 30;

/// 生成可重复的合成正文纹理，作为像素级拼接比较基准。
/// 入参：x/y（u32）为虚拟整页坐标，不是某帧裁剪后的局部位置。
/// 返回值：[u8;3]，三通道相同的灰度 RGB 像素；每 31 行中的末 8 行使用固定暗色。
/// 异常：不返回错误，wrapping_mul 明确允许溢出后回绕。
/// 注意事项：整数混合仅为生成纹理，不是加密或随机数；所有测试使用同一坐标规则生成源和期望图。
fn page_pixel(x: u32, y: u32) -> [u8; 3] {
    // 行块模拟文字，行间留白；不同高度的正文具有不同纹理。
    if y % 31 > 22 {
        return [24; 3];
    }
    let mut value = (y / 3).wrapping_mul(0x9e37_79b9) ^ (x / 2).wrapping_mul(0x85eb_ca6b);
    value ^= value >> 16;
    let shade = if value & 3 == 0 {
        (value % 180 + 60) as u8
    } else {
        24
    };
    [shade, shade, shade]
}

/// 构造具有固定顶/底栏及指定正文滚动位置的 RGB24 帧。
/// 入参：position（u32）为本帧顶部正文在虚拟整页中的起始行。
/// 返回值：RgbFrame，宽 WIDTH、高 TOP+BODY+BOTTOM，像素按行、每像素 RGB 三字节排列。
/// 异常：帧校验 unwrap 失败会 panic，极大位置加法可能溢出，分配失败不保证恢复。
/// 注意事项：只用于小范围可信测试位移；顶栏与底栏固定色不随 position 变化，每轮 y 一行、x 一个像素。
fn frame(position: u32) -> RgbFrame {
    let mut pixels = Vec::new();
    for y in 0..TOP + BODY + BOTTOM {
        for x in 0..WIDTH {
            let rgb = if y < TOP {
                [60, 80, 110]
            } else if y >= TOP + BODY {
                [70, 100, 50]
            } else {
                page_pixel(x, position + y - TOP)
            };
            pixels.extend(rgb);
        }
    }
    RgbFrame::new(WIDTH, TOP + BODY + BOTTOM, pixels).unwrap()
}

/// 生成与合成帧固定栏布局对应的拼接配置。
/// 入参：无。
/// 返回值：StitchOptions，top=TOP、bottom=BOTTOM，其余沿用默认值。
/// 异常：不返回错误。
/// 注意事项：不运行自动固定栏识别，也不根据每个样本重新调节误差容差。
fn options() -> StitchOptions {
    StitchOptions {
        crop: Crop {
            top: TOP,
            bottom: BOTTOM,
            ..Crop::default()
        },
        ..StitchOptions::default()
    }
}

/// 读取生成的 PNG 首帧，供尺寸与像素断言使用。
/// 入参：path（&Path）为测试已写出的 PNG 文件。
/// 返回值：(u32,u32,`Vec<u8>`)，依次为宽、高和解码器输出字节，截断到有效 buffer_size。
/// 异常：打开文件、解析信息、取得缓冲区大小或首帧解码失败均 unwrap panic。
/// 注意事项：不额外转换颜色格式；样本编码为 RGB8 才能直接当作按行 RGB 字节比较，不是任意 PNG 通用归一化工具。
fn decode(path: &Path) -> (u32, u32, Vec<u8>) {
    let decoder = png::Decoder::new(BufReader::new(fs::File::open(path).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut bytes = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut bytes).unwrap();
    bytes.truncate(info.buffer_size());
    (info.width, info.height, bytes)
}

/// 直接按虚拟页面坐标生成预期长图字节，避免使用被测拼接器构造期望。
/// 入参：position（u32）为最终新增正文行数；crop（Crop）仅采用 left/right；keep_bands（bool）控制是否包含固定 TOP/BOTTOM 栏。
/// 返回值：`Vec<u8>`，按行排列的 RGB24 期望像素，正文高 BODY+position，保留时上下各加固定栏。
/// 异常：极大高度求和、非法左右裁剪可能溢出或生成异常范围；分配失败不能保证恢复。
/// 注意事项：不使用 crop.top/bottom，依赖其与测试常量一致；每轮先决定是否保留该行，再逐个生成裁剪范围内的像素。
fn expected(position: u32, crop: Crop, keep_bands: bool) -> Vec<u8> {
    let mut pixels = Vec::new();
    for y in 0..TOP + BODY + position + BOTTOM {
        if !keep_bands && (y < TOP || y >= TOP + BODY + position) {
            continue;
        }
        for x in crop.left..WIDTH - crop.right {
            pixels.extend(if y < TOP {
                [60, 80, 110]
            } else if y >= TOP + BODY + position {
                [70, 100, 50]
            } else {
                page_pixel(x, y - TOP)
            });
        }
    }
    pixels
}

#[test]
/// 验证滚动暂停和回退时不会丢帧、重复内容或错位拼接。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn scroll_pause_and_backtrack_preserve_every_pixel() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("long.png");
    let mut stitcher = Stitcher::new(options()).unwrap();
    for position in [0, 0, 53, 145, 101, 200, 200, 333, 270, 400] {
        stitcher.push(frame(position)).unwrap();
    }
    let report = stitcher.finish(&output, false).unwrap();
    let (width, height, pixels) = decode(&output);
    assert_eq!((width, height), (WIDTH, TOP + BODY + 400 + BOTTOM));
    assert_eq!(pixels, expected(400, options().crop, true));
    assert_eq!(report.frames, 10);
    assert_eq!(report.duplicate_frames, 2);
    assert_eq!(report.backtrack_frames, 2);
}

#[test]
/// 验证仅保留内容区和左右裁剪时输出尺寸与像素边界准确。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn content_only_and_side_crop_are_exact() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("content.png");
    let mut opts = options();
    opts.keep_fixed_bands = false;
    opts.crop.left = 5;
    opts.crop.right = 7;
    let mut stitcher = Stitcher::new(opts.clone()).unwrap();
    for position in [0, 120, 240] {
        stitcher.push(frame(position)).unwrap();
    }
    stitcher.finish(&output, false).unwrap();
    let (width, height, pixels) = decode(&output);
    assert_eq!((width, height), (116, 640));
    assert_eq!(pixels, expected(240, opts.crop, false));
}

#[test]
/// 验证视频页面没有滚动时只保留一帧，不制造重复长图。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn stationary_video_retains_a_single_frame() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("still.png");
    let mut stitcher = Stitcher::new(options()).unwrap();
    for _ in 0..3 {
        stitcher.push(frame(0)).unwrap();
    }
    let report = stitcher.finish(&output, false).unwrap();
    assert_eq!(report.stitched_frames, 1);
    assert_eq!(decode(&output).2, frame(0).pixels());
}

#[test]
/// 验证回到已覆盖区域且没有新内容时，输出画布不会被错误拉长。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
/// 异常：测试准备或断言失败会 panic；场景中预期的错误由测试体捕获或核对，不作为生产错误返回。
fn revisit_without_reaching_new_content_does_not_extend_image() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("revisit.png");
    let mut stitcher = Stitcher::new(options()).unwrap();
    for position in [0, 150, 80, 130] {
        stitcher.push(frame(position)).unwrap();
    }
    stitcher.finish(&output, false).unwrap();
    assert_eq!(decode(&output).2, expected(150, options().crop, true));
}

#[test]
/// 验证无效帧、尺寸上限和场景切换都会在拼接阶段被拒绝。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn invalid_frames_limits_and_scene_changes_are_rejected() {
    assert!(RgbFrame::new(2, 2, vec![0; 3]).is_err());
    assert!(RgbFrame::new(u32::MAX, u32::MAX, Vec::new()).is_err());
    let mut opts = options();
    opts.max_height = TOP + BODY + BOTTOM + 50;
    let mut stitcher = Stitcher::new(opts).unwrap();
    stitcher.push(frame(0)).unwrap();
    assert!(
        stitcher
            .push(frame(100))
            .unwrap_err()
            .to_string()
            .contains("高度")
    );
    let mut stitcher = Stitcher::new(options()).unwrap();
    stitcher.push(frame(0)).unwrap();
    let changed = RgbFrame::new(
        WIDTH,
        TOP + BODY + BOTTOM,
        vec![255; (WIDTH * (TOP + BODY + BOTTOM) * 3) as usize],
    )
    .unwrap();
    assert!(stitcher.push(changed).is_err());
    assert!(
        stitcher
            .push(RgbFrame::new(64, 128, vec![0; 64 * 128 * 3]).unwrap())
            .is_err()
    );
    let mut opts = options();
    opts.crop.top = u32::MAX;
    let mut stitcher = Stitcher::new(opts).unwrap();
    assert!(stitcher.push(frame(0)).is_err());
}

#[test]
/// 验证输出先安全写入临时目标，且覆盖已有图片必须明确允许。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn output_is_atomic_and_overwrite_requires_opt_in() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("exists.png");
    fs::write(&output, b"keep me").unwrap();
    let mut stitcher = Stitcher::new(options()).unwrap();
    stitcher.push(frame(0)).unwrap();
    assert!(stitcher.finish(&output, false).is_err());
    assert_eq!(fs::read(&output).unwrap(), b"keep me");
    let mut stitcher = Stitcher::new(options()).unwrap();
    stitcher.push(frame(0)).unwrap();
    stitcher.finish(&output, true).unwrap();
    assert_eq!(decode(&output).2, frame(0).pixels());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
/// 验证无效时间范围在启动解码器前就被拒绝。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn invalid_time_range_fails_before_starting_decoder() {
    let opts = VideoOptions {
        sample_fps: f64::NAN,
        ..VideoOptions::default()
    };
    assert!(
        convert("missing.mp4", "out.png", &opts)
            .unwrap_err()
            .to_string()
            .contains("fps")
    );
    let opts = VideoOptions {
        start_seconds: 5.0,
        end_seconds: Some(3.0),
        ..VideoOptions::default()
    };
    assert!(
        convert("missing.mp4", "out.png", &opts)
            .unwrap_err()
            .to_string()
            .contains("结束时间")
    );
}

#[test]
/// 验证命令行遇到输入错误时显示诊断并返回非零退出码。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
/// 异常：测试准备或断言失败会 panic；场景中预期的错误由测试体捕获或核对，不作为生产错误返回。
fn cli_reports_errors_with_nonzero_exit_code() {
    let result = Command::new(env!("CARGO_BIN_EXE_veil-scrollshot"))
        .arg("does-not-exist.mp4")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("输入录屏不存在"));
}

/// 设置 FFMPEG 后运行真实视频解码、时间范围和 CLI 的端到端测试。
#[test]
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn ffmpeg_video_roundtrip() {
    let Some(ffmpeg) = std::env::var_os("FFMPEG") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("录屏 sample.mkv");
    let mut child = Command::new(&ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "image2pipe",
            "-c:v",
            "ppm",
            "-framerate",
            "4",
            "-i",
            "pipe:0",
            "-c:v",
            "ffv1",
        ])
        .arg(&video)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for position in
            std::iter::repeat_n(0, 20).chain([0, 0, 53, 145, 101, 200, 200, 333, 270, 400])
        {
            let frame = frame(position);
            write!(stdin, "P6\n{} {}\n255\n", frame.width(), frame.height()).unwrap();
            stdin.write_all(frame.pixels()).unwrap();
        }
    }
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let opts = VideoOptions {
        ffmpeg: ffmpeg.clone().into(),
        crop_top: Some(TOP),
        crop_bottom: Some(BOTTOM),
        ..VideoOptions::default()
    };
    let output = dir.path().join("long.png");
    let report = convert(&video, &output, &opts).unwrap();
    assert_eq!(report.frames, 30);
    assert_eq!(decode(&output).2, expected(400, options().crop, true));
    let auto_output = dir.path().join("auto.png");
    let auto_opts = VideoOptions {
        crop_top: None,
        crop_bottom: None,
        ..opts.clone()
    };
    let report = convert(&video, &auto_output, &auto_opts).unwrap();
    assert!(report.crop.top >= TOP && report.crop.bottom >= BOTTOM);
    assert_eq!(decode(&auto_output).2, expected(400, options().crop, true));
    let result = Command::new(env!("CARGO_BIN_EXE_veil-scrollshot"))
        .arg(&video)
        .arg("-o")
        .arg(dir.path().join("range.png"))
        .args([
            "--start",
            "5.5",
            "--end",
            "6.0",
            "--crop-top",
            "40",
            "--crop-bottom",
            "30",
            "--quiet",
        ])
        .env("FFMPEG", &ffmpeg)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        decode(&dir.path().join("range.png")).1,
        TOP + BODY + BOTTOM + 92
    );
    let mut opts = opts;
    opts.start_seconds = 100.0;
    assert!(convert(&video, dir.path().join("empty.png"), &opts).is_err());
    assert!(!dir.path().join("empty.png").exists());
}
