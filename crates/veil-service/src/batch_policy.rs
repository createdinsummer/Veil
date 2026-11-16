//! 文件传输的队列推送阈值配置；调度和回调由 core 队列执行。
//!
//! 本模块把本机用户设置换算成 core 队列的「批量推送策略」（BatchPolicy）：
//! 导入任务先把已准备密文的提交单元投进有界队列；core 监听器按累计权重或首项等待时间触发业务回调，
//! 回调再统一写入文件记录，减少逐文件数据库事务。阈值本身
//! 保存在 app.db 的 user_setting 表中，桌面端设置窗口可调。
use crate::error::Result;
use std::time::Duration;
// 从 core 再导出队列策略与监听控制类型，供导入流水线等模块统一引用。
pub(crate) use veil_core::queue::{BatchPolicy, ListenControl};
/// 功能：从本机偏好读取导入提交批次阈值，让流水线在数量与时延之间折中。
/// 入参：无，打开当前全局 AppStore 并读取 import_commit_count 与 import_commit_interval_seconds。
/// 返回值：Ok(BatchPolicy)，count 默认为 8、合法范围 1～100，interval 默认为 1 秒、合法范围 1～60 秒。
/// 异常：应用库定位/打开、设置解析或范围校验失败传播 VeilError。
/// 注意事项：只生成策略，不启动监听或提交事务；时间从批次首消息被取入尾批开始计算，背压、慢回调与线程调度仍可能延后实际提交。
pub(crate) fn load_policy() -> Result<BatchPolicy> {
    // 打开全局应用库（app.db），它是本机索引与设置的数据源。
    let settings = crate::storage::app::AppStore::global()?;
    Ok(BatchPolicy {
        // count：累积多少条文件记录就触发一次合并提交（默认 8，上限 100）。
        count: settings.import_commit_count()?,
        // interval：本批首条消息进入批次后等待多少秒可触发推送（默认 1 秒，上限 60 秒）。
        interval: Duration::from_secs(settings.import_commit_interval_seconds()?),
    })
}
