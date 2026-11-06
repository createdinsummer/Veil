//! 当前文件传输任务的进度；查询不依赖会话锁。
use serde::Serialize;
use std::{
    cell::RefCell,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Default, Serialize)]
pub struct Snapshot {
    pub operation: &'static str,
    pub phase: &'static str,
    pub processed_bytes: u64,
    pub total_bytes: Option<u64>,
    pub completed_files: u64,
    pub pending_files: u64,
    pub skipped_files: u64,
    pub total_files: u64,
    pub current_file: Option<String>,
    pub cancel_requested: bool,
    pub parallel_workers: u64,
}

#[derive(Clone, Default)]
pub struct Progress(Arc<Mutex<Snapshot>>, Arc<AtomicBool>);

thread_local! { static CURRENT: RefCell<Option<Progress>> = const { RefCell::new(None) }; }

pub fn current() -> Option<Progress> {
    CURRENT.with(|current| current.borrow().clone())
}
pub fn worker_count(files: usize) -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(2)
        .min(4)
        .min(files)
}

pub struct Scope {
    previous: Option<Progress>,
    active: Progress,
}
/// 将已有任务绑定到工作线程，不改变任务状态或所有权。
pub struct Attachment(Option<Progress>);
impl Drop for Attachment {
    fn drop(&mut self) {
        CURRENT.with(|current| *current.borrow_mut() = self.0.take());
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        let mut snapshot = self.active.0.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(
            snapshot.phase,
            "preparing"
                | "processing"
                | "verifying"
                | "snapshot"
                | "packing"
                | "unpacking"
                | "finalizing"
        ) {
            snapshot.phase = "failed";
        }
        drop(snapshot);
        CURRENT.with(|current| *current.borrow_mut() = self.previous.take());
    }
}
impl Progress {
    pub fn attach(&self) -> Attachment {
        Attachment(CURRENT.with(|current| current.replace(Some(self.clone()))))
    }
    pub fn snapshot(&self) -> Snapshot {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn begin(&self, operation: &'static str) -> Scope {
        self.1.store(false, Ordering::Release);
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Snapshot {
            operation,
            phase: "preparing",
            ..Snapshot::default()
        };
        Scope {
            previous: CURRENT.with(|current| current.replace(Some(self.clone()))),
            active: self.clone(),
        }
    }
    pub fn finish(&self, success: bool) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).phase =
            if success { "completed" } else { "failed" };
    }
    /// 只接受仍可安全中止的文件导入任务。
    pub fn request_cancel(&self) -> bool {
        let mut snapshot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if snapshot.operation != "import"
            || !matches!(snapshot.phase, "preparing" | "processing" | "verifying")
        {
            return false;
        }
        snapshot.cancel_requested = true;
        self.1.store(true, Ordering::Release);
        true
    }
    pub fn cancellation_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.1)
    }
    pub fn finish_cancelled(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).phase = "cancelled";
    }
}
fn update(f: impl FnOnce(&mut Snapshot)) {
    CURRENT.with(|current| {
        if let Some(progress) = current.borrow().as_ref() {
            f(&mut progress.0.lock().unwrap_or_else(|e| e.into_inner()));
        }
    });
}
pub fn start(total: Option<u64>) {
    update(|p| {
        p.total_bytes = total;
        p.phase = "processing";
    });
}
/// 切换到独立阶段；百分比仅表示当前阶段的字节处理进度。
pub fn stage(phase: &'static str, total: Option<u64>) {
    update(|p| {
        p.phase = phase;
        p.total_bytes = total;
        p.processed_bytes = 0;
        p.current_file = None;
        p.completed_files = 0;
        p.pending_files = 0;
        p.skipped_files = 0;
        p.total_files = 0;
        p.parallel_workers = 0;
    });
}

pub fn copy(
    reader: &mut impl std::io::Read,
    writer: &mut impl std::io::Write,
) -> std::io::Result<u64> {
    struct Counted<'a, W> {
        writer: &'a mut W,
        pending: u64,
    }
    impl<W: std::io::Write> std::io::Write for Counted<'_, W> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let written = self.writer.write(bytes)?;
            self.pending += written as u64;
            if self.pending >= 1024 * 1024 {
                advance(self.pending);
                self.pending = 0;
            }
            Ok(written)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.writer.flush()
        }
    }
    let mut counted = Counted { writer, pending: 0 };
    let result = std::io::copy(reader, &mut counted);
    advance(counted.pending);
    result
}

#[derive(Debug)]
struct CopyCancelled;
impl std::fmt::Display for CopyCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("操作已中止")
    }
}
impl std::error::Error for CopyCancelled {}

pub fn is_cancelled_io(error: &std::io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|source| source.is::<CopyCancelled>())
}

/// 不累计进度：用于已经计过加密字节数的密文发布复制。
pub fn copy_cancellable(
    reader: &mut impl std::io::Read,
    writer: &mut impl std::io::Write,
) -> std::io::Result<u64> {
    struct Cancellable<'a, W> {
        writer: &'a mut W,
        since_check: usize,
    }
    impl<W: std::io::Write> std::io::Write for Cancellable<'_, W> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.since_check >= 1024 * 1024 {
                if check_cancelled().is_err() {
                    return Err(std::io::Error::other(CopyCancelled));
                }
                self.since_check = 0;
            }
            let limit = (1024 * 1024 - self.since_check).min(bytes.len());
            let written = self.writer.write(&bytes[..limit])?;
            self.since_check += written;
            Ok(written)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.writer.flush()
        }
    }
    if check_cancelled().is_err() {
        return Err(std::io::Error::other(CopyCancelled));
    }
    let result = std::io::copy(
        reader,
        &mut Cancellable {
            writer,
            since_check: 0,
        },
    );
    if result.is_ok() && check_cancelled().is_err() {
        return Err(std::io::Error::other(CopyCancelled));
    }
    result
}
pub fn set_file_count(total: usize) {
    update(|p| p.total_files = total as u64);
}
pub fn set_parallel_workers(count: usize) {
    update(|p| p.parallel_workers = count as u64);
}
pub fn begin_file(name: &str) {
    update(|p| {
        p.phase = "processing";
        p.current_file = Some(name.to_owned());
    });
}
pub fn file_done(skipped: bool) {
    update(|p| {
        p.completed_files += 1;
        if skipped {
            p.skipped_files += 1;
        }
    });
}
/// 记录加密完成、尚未提交的文件。
pub fn file_queued() {
    update(|p| p.pending_files += 1);
}
pub fn file_dequeued() {
    update(|p| p.pending_files = p.pending_files.saturating_sub(1));
}
pub fn verifying() {
    update(|p| p.phase = "verifying");
}
pub fn advance(bytes: u64) {
    update(|p| p.processed_bytes = p.processed_bytes.saturating_add(bytes));
}
pub fn check_cancelled() -> crate::error::Result<()> {
    let cancelled = CURRENT.with(|current| {
        current
            .borrow()
            .as_ref()
            .is_some_and(|p| p.snapshot().cancel_requested)
    });
    if cancelled {
        Err(crate::error::VeilError::Cancelled)
    } else {
        Ok(())
    }
}
pub fn finalizing() {
    update(|p| p.phase = "finalizing");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_only_accepted_before_finalizing_an_import() {
        let progress = Progress::default();
        assert!(!progress.request_cancel());
        {
            let _scope = progress.begin("export");
            start(Some(1));
            assert!(!progress.request_cancel());
        }
        {
            let _scope = progress.begin("import");
            start(Some(1));
            finalizing();
            assert!(!progress.request_cancel());
        }
    }

    #[test]
    fn cancellation_interrupts_a_staged_copy() {
        struct StopWriter {
            progress: Progress,
            written: usize,
        }
        impl std::io::Write for StopWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.written += bytes.len();
                self.progress.request_cancel();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let progress = Progress::default();
        let _scope = progress.begin("import");
        start(Some(2 * 1024 * 1024));
        let mut reader = std::io::Cursor::new(vec![1; 2 * 1024 * 1024]);
        let mut writer = StopWriter {
            progress,
            written: 0,
        };
        let error = copy_cancellable(&mut reader, &mut writer).unwrap_err();
        assert!(is_cancelled_io(&error));
        assert!(writer.written < 2 * 1024 * 1024);
    }

    #[test]
    fn progress_can_be_read_from_another_thread() {
        let progress = Progress::default();
        let worker = progress.clone();
        let (ready, observe) = std::sync::mpsc::channel();
        let (resume, wait) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _scope = worker.begin("import");
            start(Some(10));
            advance(4);
            ready.send(()).unwrap();
            wait.recv().unwrap();
            advance(6);
            finalizing();
            worker.finish(true);
        });
        observe.recv().unwrap();
        let snapshot = progress.snapshot();
        assert_eq!(
            (
                snapshot.phase,
                snapshot.processed_bytes,
                snapshot.total_bytes
            ),
            ("processing", 4, Some(10))
        );
        resume.send(()).unwrap();
        thread.join().unwrap();
        let snapshot = progress.snapshot();
        assert_eq!(
            (snapshot.phase, snapshot.processed_bytes),
            ("completed", 10)
        );
    }
}
