use std::{
    fs,
    io::{BufReader, Write},
    path::Path,
    process::{Command, Stdio},
};
use veil_scrollshot::{Crop, RgbFrame, StitchOptions, Stitcher, VideoOptions, convert};

const WIDTH: u32 = 128;
const BODY: u32 = 400;
const TOP: u32 = 40;
const BOTTOM: u32 = 30;

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

fn decode(path: &Path) -> (u32, u32, Vec<u8>) {
    let decoder = png::Decoder::new(BufReader::new(fs::File::open(path).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut bytes = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut bytes).unwrap();
    bytes.truncate(info.buffer_size());
    (info.width, info.height, bytes)
}

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
