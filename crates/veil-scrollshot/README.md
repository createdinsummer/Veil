# veil-scrollshot

独立的 Rust 库和命令行程序，将垂直向下滚动的录屏拼接成 PNG 长截图，不依赖 `veil-core`。

自动识别顶部固定栏、自动识别底部固定栏；固定栏默认只保留一次。通过图像重叠计算滚动距离，跳过静止画面，回滚或重访已有内容时不重复追加。RGB 条带暂存到临时文件，PNG 流式编码，内存占用不随长图高度增长。

## 运行

需要 Rust 工具链和 FFmpeg。FFmpeg 查找顺序为 `--ffmpeg`、`FFMPEG` 环境变量、可执行程序所在目录、系统 `PATH`。支持 Windows、macOS 和 Linux；各平台需要相应的 FFmpeg 可执行文件。

```sh
cargo build --release -p veil-scrollshot
target/release/veil-scrollshot "ScreenRecording_10-22-2026 16-28-02_1.MP4" -o target/long.png
```

省略 `-o` 时，在录屏旁生成同名 `.long.png` 文件。已有输出默认不覆盖，显式添加 `--overwrite` 才会覆盖。转换失败时不输出半张图片。

```sh
# 快速滚动时提高抽样频率，并指定需要转换的时间范围（秒）
target/release/veil-scrollshot recording.mp4 -o long.png --fps 8 --start 2 --end 60

# 手动指定固定栏高度（原始像素）；只保留正文
target/release/veil-scrollshot recording.mp4 --crop-top 450 --crop-bottom 260 --content-only

# 使用指定的 FFmpeg
target/release/veil-scrollshot recording.mp4 --ffmpeg /path/to/ffmpeg
```

`--crop-top 0 --crop-bottom 0` 可关闭固定栏识别。`--crop-left`、`--crop-right` 会移除每帧的对应边缘。默认每秒抽样 4 帧，输出高度最多 200000 像素，输出像素总量最多 200000000；可通过 `--max-height`、`--max-pixels` 调整。完整参数见 `--help`。

自动识别会先以缩小画面收集实际运动样本，因此支持开头长时间停留；对于整段静止视频，会先扫描视频，再输出单帧。

## 库接口

```rust,no_run
use veil_scrollshot::{convert, VideoOptions};

fn main() -> anyhow::Result<()> {
    let report = convert("recording.mp4", "long.png", &VideoOptions::default())?;
    println!("{}×{}", report.width, report.height);
    Ok(())
}
```

`convert_with_progress` 提供逐帧进度回调。`VideoOptions.ffmpeg` 指定视频解码器；库接口默认使用系统 `PATH` 中的 `ffmpeg`。视频转换的顶部固定栏通过 `VideoOptions.crop_top` 设置，底部固定栏通过 `VideoOptions.crop_bottom` 设置；`None` 为自动识别，`Some(0)` 为关闭对应固定栏识别。左右裁剪通过 `VideoOptions.stitch.crop.left`、`VideoOptions.stitch.crop.right` 设置。

已有解码器时，可直接使用 `Stitcher::new(StitchOptions)`、`push(RgbFrame)`、`finish(output, overwrite)`，此路径无需 FFmpeg。`RgbFrame::new(width, height, pixels)` 接收按行排列的 RGB24 数据；`StitchOptions.crop` 设置固定栏和边缘裁剪，`keep_fixed_bands = false` 仅保留正文。

## 验证与边界

```sh
cargo test -p veil-scrollshot
# 设置 FFMPEG 后额外执行真实视频的端到端测试
FFMPEG=/path/to/ffmpeg cargo test -p veil-scrollshot
cargo clippy -p veil-scrollshot --all-targets -- -D warnings
```

适用于保持画面尺寸的垂直向下滚动录屏，支持中途回滚去重。相邻抽样帧需要至少约 20% 的正文重叠；切换页面、横向移动、缩放画面、纹理不足或严重模糊可能导致匹配失败，错误会指出对应时间。固定栏识别可能需要手动调整；纯重复图案无法保证唯一定位。超出输入画面范围的文字不会补全，录屏中未展示的内容不会生成。

当前示例录屏以默认参数输出 `1206×93295`，覆盖列表开头到第 500 首歌曲；具体耗时受机器性能和 PNG 编码影响。
