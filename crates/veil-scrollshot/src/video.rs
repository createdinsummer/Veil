//! 文件作用：长截图流程的视频子模块——把 FFmpeg 子进程封装成“按抽样率
//! 逐帧输出 RGB 帧”的迭代器，负责子进程的启动、错误诊断与生命周期管理。
//! 所属业务模块：长截图工具（scrollshot）的视频解码层。
//! 核心结构体/函数清单：VideoReader（FFmpeg 子进程封装）及其 open（启动）、
//! next（读一帧）、finish（等待结束）、diagnostics（收集 stderr 诊断）。
//! 依赖能力：外部 FFmpeg 可执行文件；不依赖 ffprobe——帧尺寸直接从
//! PPM 管道头读取（见 frame::read_ppm）。
//! 业务目标：把“解码”这个外部能力隔离在本模块内，向上层稳定提供
//! `Option<RgbFrame>` 的流式接口，解码失败时给出 FFmpeg 的原始错误信息。
use crate::{RgbFrame, VideoOptions, frame::read_ppm};
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
    process::{Child, ChildStdout, Command, Stdio},
};

/// FFmpeg 子进程封装。
/// 业务定位：一次“录屏 → RGB 帧流”的解码会话；同一个 VideoReader 从头到尾
/// 读取所选时间范围内的全部抽样帧。
pub(crate) struct VideoReader {
    /// FFmpeg 子进程句柄，用于等待退出或强制终止。
    child: Child,
    /// 子进程 stdout（P6 PPM 帧流）的缓冲读取器。
    stdout: BufReader<ChildStdout>,
    /// 子进程 stderr 重定向到的临时文件，出错时读回作为诊断信息。
    stderr: std::fs::File,
    /// 是否已经正常结束（正常退出或被终止），防止重复 wait。
    finished: bool,
}

impl VideoReader {
    /// 启动一个按时间范围抽样、向管道输出 RGB 帧的 FFmpeg 解码会话。
    /// 入参：input:&Path 视频位置，先 canonicalize；options:&VideoOptions 提供程序路径、采样率和时间范围；probe:bool，true 额外将宽度缩到不超过 320，false 保留源尺寸。
    /// 返回值：Ok(VideoReader) 持有子进程、缓冲 stdout 和诊断临时文件；仅表明启动及管道准备成功。
    /// 异常：路径解析、临时文件/句柄复制、进程启动及管道获取失败返回 Err；FFmpeg 参数或媒体错误通常在后续 next/finish 暴露。
    /// 注意事项：不在本方法校验 fps/时间范围，公共转换入口负责先校验；stderr 重定向到临时文件避免错误管道堵塞，但文件可持续增长。没有超时，调用方应持续读帧或及时释放读取器。
    pub fn open(input: &Path, options: &VideoOptions, probe: bool) -> Result<Self> {
        // 使用绝对路径，文件名即使以 '-' 开头也不会被 FFmpeg 当作参数。
        let input = input.canonicalize().context("无法读取输入录屏")?;
        let stderr = tempfile::tempfile()?;
        let mut command = Command::new(&options.ffmpeg);
        // 关闭横幅、只输出错误级日志、不读取标准输入，避免交互式阻塞。
        command
            .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
            // 将 -ss 放在输入前请求 FFmpeg 定位；实际精度取决于解码器及 FFmpeg 的寻址处理，不由本代码按帧校准。
            .arg("-ss")
            .arg(options.start_seconds.to_string())
            .arg("-i")
            .arg(input);
        if let Some(end) = options.end_seconds {
            // -t 指定时长：把“结束时间”换算成相对起始时间的秒数。
            command
                .arg("-t")
                .arg((end - options.start_seconds).to_string());
        }
        // fps 滤镜按指定速率重定时，round=up 控制时间戳取整；不保证原视频每一帧都被保留。
        let mut filter = format!("fps={}:round=up", options.sample_fps);
        if probe {
            // 探测通道再缩小画面宽度到 ≤320，显著降低固定栏扫描成本。
            filter.push_str(",scale='min(320,iw)':-1");
        }
        // 只取第一条视频流，丢弃音频/字幕/数据流。
        command
            .args(["-map", "0:v:0", "-an", "-sn", "-dn", "-vf"])
            .arg(filter);
        // 输出为连续 P6 PPM 帧流，直接写向管道供本进程边读边解析。
        command
            .args(["-c:v", "ppm", "-f", "image2pipe", "pipe:1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr.try_clone()?));
        let mut child = command.spawn().with_context(|| {
            format!(
                "无法启动 FFmpeg：{}；请安装 FFmpeg，或通过 --ffmpeg 指定路径",
                options.ffmpeg.display()
            )
        })?;
        let stdout = BufReader::new(child.stdout.take().context("无法打开解码管道")?);
        Ok(Self {
            child,
            stdout,
            stderr,
            finished: false,
        })
    }

    /// 从 FFmpeg 的标准输出管道读取并解析下一帧，供逐帧拼接。
    /// 入参：&mut self 独占读取器，无其他参数；依赖仍可读的 PPM 管道。
    /// 返回值：Ok(Some(RgbFrame)) 为一帧，EOF 且子进程成功退出为 Ok(None)，帧解析或进程错误为 Err。
    /// 异常：PPM 格式、像素读取、等待进程或非零退出状态错误；不捕获 panic。
    /// 注意事项：同步读取和 wait 可阻塞，无超时或取消参数；帧解析失败尽力 kill/wait 后标记结束，发生错误后不应继续使用读取器。
    pub fn next(&mut self) -> Result<Option<RgbFrame>> {
        let frame = read_ppm(&mut self.stdout);
        match frame {
            Ok(Some(frame)) => Ok(Some(frame)),
            Ok(None) => {
                // 管道关闭：等待子进程退出并确认退出码为成功。
                self.finish()?;
                Ok(None)
            }
            Err(error) => {
                // 帧流解析失败：终止 FFmpeg，把它的 stderr 拼进错误信息。
                let _ = self.child.kill();
                let _ = self.child.wait();
                self.finished = true;
                bail!("视频解码中断：{error:#}；{}", self.diagnostics());
            }
        }
    }

    /// 确认帧流结束后 FFmpeg 进程的退出状态。
    /// 入参：&mut self 独占读取器，无其他参数。
    /// 返回值：已 finished 或新等待结果成功时 Ok(())，wait 失败或非成功状态返回 Err。
    /// 异常：系统等待错误及 FFmpeg 错误，后者附最多 8192 字节诊断。
    /// 注意事项：wait 无超时；wait 成功后先置 finished 再检查退出状态，因此异常退出后的重复 finish 可能直接 Ok，不重复报告原错误。
    fn finish(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        let status = self.child.wait()?;
        self.finished = true;
        ensure!(status.success(), "FFmpeg 解码失败：{}", self.diagnostics());
        Ok(())
    }

    /// 为解码失败信息读取 FFmpeg 保存的 stderr 摘要。
    /// 入参：&mut self，可变借用用于定位与读取诊断暂存文件。
    /// 返回值：String，最多读取前 8192 字节并去除首尾空白；定位和读取结果被忽略，失败可为空或部分内容。
    /// 异常：不返回 I/O 错误；非 UTF-8 read_to_string 也被忽略，不保证总能得到完整诊断。
    /// 注意事项：尝试回到文件开头但不清空文件，可重复读取；错误文案来自外部程序，可能包含视频路径等业务细节。
    fn diagnostics(&mut self) -> String {
        let _ = self.stderr.seek(SeekFrom::Start(0));
        let mut message = String::new();
        let _ = (&mut self.stderr).take(8192).read_to_string(&mut message);
        message.trim().to_owned()
    }
}

impl Drop for VideoReader {
    /// 在读取器提前释放时尽力终止并等待 FFmpeg，避免后台解码继续运行。
    /// 入参：&mut self，析构时由 Rust 自动调用。
    /// 返回值：()；finished 已为 true 时不重复处理。
    /// 异常：kill/wait 返回错误被忽略，不传播；不捕获其他 panic。
    /// 注意事项：等待可能阻塞，不保证 kill 失败后进程立刻结束；只管理本读取器子进程，临时诊断文件随字段析构清理。
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
