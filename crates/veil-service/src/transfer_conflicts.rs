//! 文件传输的有界冲突确认队列。
use crate::{
    error::Result,
    session::{SessionImportAction, SessionImportResolution},
    storage::invalid,
};
use serde::Serialize;
use std::sync::Mutex;
use veil_core::{progress, queue::BoundedQueue};
const WAIT_LIMIT: usize = 128;
const DISPLAY_LIMIT: usize = 32;
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictView {
    pub target: String,
    pub same_content: bool,
    pub is_directory: bool,
    pub error: Option<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictBatch {
    pub id: String,
    pub total_files: u64,
    pub conflicts: Vec<ConflictView>,
}
pub(crate) struct Waiting<W> {
    pub work: W,
    pub view: ConflictView,
    pub action: Option<SessionImportAction>,
}
struct Decisions<W> {
    pending: BoundedQueue<Waiting<W>>,
    resolved: BoundedQueue<Waiting<W>>,
    displayed: Option<(String, Vec<Waiting<W>>)>,
}
impl<W> Default for Decisions<W> {
    fn default() -> Self {
        Self {
            pending: BoundedQueue::new(WAIT_LIMIT),
            resolved: BoundedQueue::new(WAIT_LIMIT),
            displayed: None,
        }
    }
}
impl<W> Decisions<W> {
    pub(crate) fn len(&self) -> usize {
        self.pending.len()
            + self.resolved.len()
            + self.displayed.as_ref().map_or(0, |(_, tasks)| tasks.len())
    }
}
/// 队列容量由 core 保证；展示批次、用户选择和版本依据由服务层管理。
pub(crate) struct ConflictQueue<W>(Mutex<Decisions<W>>);
impl<W> Default for ConflictQueue<W> {
    fn default() -> Self {
        Self(Mutex::new(Decisions::default()))
    }
}
impl<W> ConflictQueue<W> {
    pub fn poll(&self, total_files: u64) -> Option<ConflictBatch> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.displayed.is_none() {
            let tasks = state
                .pending
                .try_recv_batch(DISPLAY_LIMIT)
                .unwrap_or_default();
            if tasks.is_empty() {
                return None;
            }
            state.displayed = Some((crate::storage::id(), tasks));
        }
        let (id, tasks) = state.displayed.as_ref().unwrap();
        Some(ConflictBatch {
            id: id.clone(),
            total_files,
            conflicts: tasks.iter().map(|task| task.view.clone()).collect(),
        })
    }
    pub fn resolve(&self, id: &str, resolutions: Vec<SessionImportResolution>) -> Result<()> {
        let mut state = self.0.lock().map_err(|_| invalid("传输选择状态不可用"))?;
        let (_, tasks) = state
            .displayed
            .as_ref()
            .filter(|(token, _)| token == id)
            .ok_or_else(|| invalid("这批传输选择已失效"))?;
        let mut choices = std::collections::HashMap::new();
        for resolution in resolutions {
            if !tasks
                .iter()
                .any(|task| task.view.target == resolution.target)
                || choices
                    .insert(resolution.target, resolution.action)
                    .is_some()
            {
                return Err(invalid("传输选择无效"));
            }
        }
        if choices.len() != tasks.len() {
            return Err(invalid("请处理当前批次的全部冲突"));
        }
        for task in tasks {
            if matches!(
                choices.get(&task.view.target),
                Some(SessionImportAction::Overwrite)
            ) && (task.view.is_directory || task.view.error.is_some())
            {
                return Err(invalid("无法覆盖该目标，请跳过或取消"));
            }
        }
        let (_, tasks) = state.displayed.take().unwrap();
        let tasks = tasks
            .into_iter()
            .map(|mut task| {
                task.action = choices.remove(&task.view.target);
                task
            })
            .collect();
        state
            .resolved
            .try_send_batch(tasks)
            .map_err(|_| invalid("已选择任务队列超出容量"))?;
        Ok(())
    }
    pub(crate) fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
    pub(crate) fn push(&self, task: Waiting<W>) -> Result<()> {
        let state = self.0.lock().map_err(|_| invalid("传输选择状态不可用"))?;
        if state.len() >= WAIT_LIMIT {
            return Err(invalid("待确认任务队列超出容量"));
        }
        state
            .pending
            .try_send(task)
            .map_err(|_| invalid("待确认任务队列不可用"))?;
        progress::set_waiting_files(state.len());
        Ok(())
    }
    pub(crate) fn take_resolved(&self) -> Vec<Waiting<W>> {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let tasks = state
            .resolved
            .try_recv_batch(WAIT_LIMIT)
            .unwrap_or_default();
        progress::set_waiting_files(state.len());
        tasks
    }
}
