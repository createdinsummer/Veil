use crate::{RgbFrame, VideoOptions, frame::read_ppm};
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
    process::{Child, ChildStdout, Command, Stdio},
};

pub(crate) struct VideoReader {
    child: Child,
    stdout: BufReader<ChildStdout>,
    stderr: std::fs::File,
    finished: bool,
}

impl VideoReader {
    pub fn open(input: &Path, options: &VideoOptions, probe: bool) -> Result<Self> {
        // 使用绝对路径，文件名即使以 '-' 开头也不会被 FFmpeg 当作参数。
        let input = input.canonicalize().context("无法读取输入录屏")?;
        let stderr = tempfile::tempfile()?;
        let mut command = Command::new(&options.ffmpeg);
        command
            .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
            .arg("-ss")
            .arg(options.start_seconds.to_string())
            .arg("-i")
            .arg(input);
        if let Some(end) = options.end_seconds {
            command
                .arg("-t")
                .arg((end - options.start_seconds).to_string());
        }
        let mut filter = format!("fps={}:round=up", options.sample_fps);
        if probe {
            filter.push_str(",scale='min(320,iw)':-1");
        }
        command
            .args(["-map", "0:v:0", "-an", "-sn", "-dn", "-vf"])
            .arg(filter);
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

    pub fn next(&mut self) -> Result<Option<RgbFrame>> {
        let frame = read_ppm(&mut self.stdout);
        match frame {
            Ok(Some(frame)) => Ok(Some(frame)),
            Ok(None) => {
                self.finish()?;
                Ok(None)
            }
            Err(error) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
                self.finished = true;
                bail!("视频解码中断：{error:#}；{}", self.diagnostics());
            }
        }
    }

    fn finish(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        let status = self.child.wait()?;
        self.finished = true;
        ensure!(status.success(), "FFmpeg 解码失败：{}", self.diagnostics());
        Ok(())
    }

    fn diagnostics(&mut self) -> String {
        let _ = self.stderr.seek(SeekFrom::Start(0));
        let mut message = String::new();
        let _ = (&mut self.stderr).take(8192).read_to_string(&mut message);
        message.trim().to_owned()
    }
}

impl Drop for VideoReader {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
