//! 文件传输的有界冲突确认队列。
//!
//! 导入文件遇到容器内同名目标时，写入不能自动覆盖——需要用户逐项选择
//! 「覆盖/跳过」或在外部取消传输。本模块用两个队列和一个展示批次构成三段状态：待确认区（pending）→ 展示
//! 给用户的批次（displayed）→ 用户选择后的已解决区（resolved）。全程有
//! 上限（WAIT_LIMIT），防止批量导入时冲突项无限堆积占满内存；队列容量
//! 由 core 的有界队列保证，展示批次与选择规则由服务层管理。
use crate::{
    error::Result,
    session::{SessionImportAction, SessionImportResolution},
    storage::invalid,
};
use serde::Serialize;
use std::sync::Mutex;
use veil_core::{progress, queue::BoundedQueue};
// 待确认＋展示中＋已解决的总上限：超过则拒绝继续接收新任务。
const WAIT_LIMIT: usize = 128;
// 每次展示给用户的冲突数量上限（一屏可见的数量）。
const DISPLAY_LIMIT: usize = 32;
/// 单条冲突的展示视图：目标路径、内容是否相同、是否目录、附加错误。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictView {
    /// 导入时为容器内目标路径，导出时为普通文件系统目标位置；用于向用户展示并精确关联选择。
    pub target: String,
    /// 比较阶段观察到双方内容是否相同，供用户决定跳过；实际继续执行前仍需按业务规则再检查源和目标。
    pub same_content: bool,
    /// 冲突目标是否是目录（目录不能选择「覆盖」）。
    pub is_directory: bool,
    /// 冲突之外的附加错误说明（有错误时也不允许覆盖）。
    pub error: Option<String>,
}
/// 一次展示给用户的冲突批次。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictBatch {
    /// 批次令牌：用户提交选择时必须原样带回，防止对旧批次作答。
    pub id: String,
    /// 本次传输的总文件数（用于界面显示进度）。
    pub total_files: u64,
    /// 本批次要用户处理的冲突清单。
    pub conflicts: Vec<ConflictView>,
}
/// 一个等待用户决策的传输任务：工作负载＋展示视图＋用户已做的选择。
pub(crate) struct Waiting<W> {
    /// 被挂起的业务工作 W，导入/导出各自定义来源与目标；收到决定后取回继续检查，不是已提交结果。
    pub work: W,
    /// 展示给用户的冲突信息。
    pub view: ConflictView,
    /// 用户的选择；None 表示尚未决策。
    pub action: Option<SessionImportAction>,
}
/// 冲突决策状态机：三块队列共同组成整个有界流程。
struct Decisions<W> {
    /// 待展示队列：新冲突先到这里排队。
    pending: BoundedQueue<Waiting<W>>,
    /// 已解决队列：用户选完后等传输流水线取回继续执行。
    resolved: BoundedQueue<Waiting<W>>,
    /// 当前展示中的批次：令牌＋任务列表；未决期间不展示新批次。
    displayed: Option<(String, Vec<Waiting<W>>)>,
}
impl<W> Default for Decisions<W> {
    /// 功能：初始化一次传输的冲突决策状态，让待确认和已解决任务分别排队。
    /// 入参：无。
    /// 返回值：两个容量均为 WAIT_LIMIT 的空队列，displayed 为 None。
    /// 异常：不返回业务错误。
    /// 注意事项：总量约束由三段状态合计检查实现，不能简单把两队列容量相加当作业务上限。
    fn default() -> Self {
        Self {
            pending: BoundedQueue::new(WAIT_LIMIT),
            resolved: BoundedQueue::new(WAIT_LIMIT),
            displayed: None,
        }
    }
}
impl<W> Decisions<W> {
    /// 功能：统计本次传输所有仍待恢复执行的冲突任务，供背压计算和界面等待数量使用。
    /// 入参：self: &`Decisions<W>`，包含待展示、展示中及已决待取回三段状态。
    /// 返回值：usize，为 pending 条数、resolved 条数和 displayed 内任务数之和。
    /// 异常：只读取内存队列状态，不返回业务错误。
    /// 注意事项：已选动作但尚未取回的任务也计入数量；调用者应持外层 ConflictQueue 互斥锁才能将这三段视为同一调度状态。
    pub(crate) fn len(&self) -> usize {
        self.pending.len()
            + self.resolved.len()
            + self.displayed.as_ref().map_or(0, |(_, tasks)| tasks.len())
    }
}
/// 队列容量由 core 保证；展示批次、用户选择和版本依据由服务层管理。
pub(crate) struct ConflictQueue<W>(Mutex<Decisions<W>>);
impl<W> Default for ConflictQueue<W> {
    /// 功能：为一次文件传输创建可跨线程共享的冲突决策容器。
    /// 入参：无。
    /// 返回值：用互斥锁包裹全新 Decisions 的 ConflictQueue。
    /// 异常：不返回业务错误。
    /// 注意事项：锁串行保护展示批次、用户选择与排队状态；创建本身不启动后台线程。
    fn default() -> Self {
        Self(Mutex::new(Decisions::default()))
    }
}
impl<W> ConflictQueue<W> {
    /// 功能：为交互端提供当前需要决策的一批冲突，没有展示批次时领取待确认队列的有序前缀。
    /// 入参：self: &`ConflictQueue<W>`；total_files: u64 为调用方当前已知文件总数，供展示，不用于队列完整性检查。
    /// 返回值：有任务时 Some(ConflictBatch)，每批最多 32 条；无任务为 None。已有展示批次复用令牌与冲突列表，总数字段使用本次传值。
    /// 异常：队列接收错误按空列表处理；外层状态锁中毒时取回状态，生成随机批次 ID 依赖底层随机源，失败可 panic。
    /// 注意事项：只是移动到展示态，不执行工作或替用户选择；等待互斥锁可能短暂阻塞，重复轮询不创建第二份展示批次。
    pub fn poll(&self, total_files: u64) -> Option<ConflictBatch> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        // 没有展示中的批次时，从待确认队列取最多 DISPLAY_LIMIT 条形成新批次。
        if state.displayed.is_none() {
            let tasks = state
                .pending
                .try_recv_batch(DISPLAY_LIMIT)
                .unwrap_or_default();
            // 待确认队列为空：没有冲突可展示。
            if tasks.is_empty() {
                return None;
            }
            // 给新批次发放随机令牌，进入「展示中」状态。
            state.displayed = Some((crate::storage::id(), tasks));
        }
        // 返回当前批次（重复轮询返回同一批，直到 resolve 完成）。
        let (id, tasks) = state.displayed.as_ref().unwrap();
        Some(ConflictBatch {
            id: id.clone(),
            total_files,
            conflicts: tasks.iter().map(|task| task.view.clone()).collect(),
        })
    }
    /// 功能：校验用户对当前全部冲突的选择，再把对应挂起工作转入已决队列供流水线继续。
    /// 入参：self: &`ConflictQueue<W>`；id: &str 为当前展示批次令牌；resolutions: `Vec<SessionImportResolution>` 为每个目标的 Skip/Overwrite 选择，取得列表所有权。
    /// 返回值：全部选择合法并入已决队列后 Ok(())。
    /// 异常：状态锁中毒、令牌过期、目标不在批次、重复/漏选、目录或附带错误项选择覆盖、已决队列发送失败均返回 InvalidFormat。
    /// 注意事项：校验完成前保留展示批次；后续真正发送若失败，展示态已被取走，不自动还原或重投工作。
    /// 目标以完整字符串作键，同一批次任务应有互异目标；本方法不取消整传输，也不代替提交时的版本/资源再验证。
    pub fn resolve(&self, id: &str, resolutions: Vec<SessionImportResolution>) -> Result<()> {
        let mut state = self.0.lock().map_err(|_| invalid("传输选择状态不可用"))?;
        // 令牌必须匹配当前展示中的批次，否则视为过期选择。
        let (_, tasks) = state
            .displayed
            .as_ref()
            .filter(|(token, _)| token == id)
            .ok_or_else(|| invalid("这批传输选择已失效"))?;
        // 把用户选择按目标路径建表；目标不重复、且必须都在本批次内。
        // choices 的键为本批目标完整字符串，值为该目标唯一动作；用它同时排除重复回答和检测漏选。
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
        // 必须给本批次每条冲突都做出选择，不允许漏选。
        if choices.len() != tasks.len() {
            return Err(invalid("请处理当前批次的全部冲突"));
        }
        // 「覆盖」只允许用于普通文件且无错误：目录与出错项只能跳过/取消。
        for task in tasks {
            if matches!(
                choices.get(&task.view.target),
                Some(SessionImportAction::Overwrite)
            ) && (task.view.is_directory || task.view.error.is_some())
            {
                return Err(invalid("无法覆盖该目标，请跳过或取消"));
            }
        }
        // 校验全部通过：批次出列，把选择写回每个任务。
        let (_, tasks) = state.displayed.take().unwrap();
        let tasks = tasks
            .into_iter()
            .map(|mut task| {
                task.action = choices.remove(&task.view.target);
                task
            })
            .collect();
        // 转入已解决队列，等待流水线重新核对并继续；此时展示态已取走，若内部发送异常不会把原批次恢复回来。
        state
            .resolved
            .try_send_batch(tasks)
            .map_err(|_| invalid("已选择任务队列超出容量"))?;
        Ok(())
    }
    /// 功能：读取一次传输中尚占用冲突容量的任务总量，供检查阶段决定是否继续接收新冲突。
    /// 入参：self: &`ConflictQueue<W>`。
    /// 返回值：usize，含待确认、已展示与已决待取回三段任务。
    /// 异常：外层互斥锁中毒时取回状态，不返回 Result。
    /// 注意事项：快照返回后并发方可继续改变队列；不能把零条理解为所有文件传输已完成。
    pub(crate) fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
    /// 功能：把遇到目标占用的一项工作挂起，加入等待用户决策的缓冲区。
    /// 入参：self: &`ConflictQueue<W>`；task: `Waiting<W>` 为业务负载、展示事实与尚未填写动作的一条工作，按值转移。
    /// 返回值：全三段数量小于 128 且待确认队列接受时 Ok(())，同时更新当前线程绑定任务的等待量。
    /// 异常：状态锁中毒、总量已满或内部发送失败返回 InvalidFormat。
    /// 注意事项：本队列不因满而等待，不去重目标；上游监听器先用 len 调整背压，避免把容量耗尽变成传输失败。
    /// 不负责保存断点或持久化工作，失败/进程退出后不能靠此内存队列恢复任务。
    pub(crate) fn push(&self, task: Waiting<W>) -> Result<()> {
        let state = self.0.lock().map_err(|_| invalid("传输选择状态不可用"))?;
        // 三段总和已满：拒绝接收，让调用方转为整体失败而不是无限排队。
        if state.len() >= WAIT_LIMIT {
            return Err(invalid("待确认任务队列超出容量"));
        }
        state
            .pending
            .try_send(task)
            .map_err(|_| invalid("待确认任务队列不可用"))?;
        // 更新界面上的等待数量。
        progress::set_waiting_files(state.len());
        Ok(())
    }
    /// 功能：把已收到用户决定的工作交回流水线，允许检查与实际传输继续。
    /// 入参：self: &`ConflictQueue<W>`。
    /// 返回值：`Vec<Waiting<W>>`，最多取 128 条已决任务，保留队内顺序，每项 action 应为用户已确认动作；无可取项为空。
    /// 异常：外层锁中毒时恢复状态，内部批量接收失败转为空列表，不返回 Result。
    /// 注意事项：领取后任务所有权离开决策队列，后续失败不自动重投；进度等待量只更新当前线程已绑定的 Progress。
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
