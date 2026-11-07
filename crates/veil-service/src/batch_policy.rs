//! 文件传输的队列推送阈值配置；调度和回调由 core 队列执行。
use crate::error::Result;
use std::time::Duration;
pub(crate) use veil_core::queue::{BatchPolicy, ListenControl};
pub(crate) fn load_policy() -> Result<BatchPolicy> {
    let settings = crate::storage::app::AppStore::global()?;
    Ok(BatchPolicy {
        count: settings.import_commit_count()?,
        interval: Duration::from_secs(settings.import_commit_interval_seconds()?),
    })
}
