//! 提示偏好及一次性本机状态。
//!
//! 提示系统由两层共同决定某条提示是否展示：
//! 1. 提示级别（hint_level，本机用户设置）：full 全部提示、brief 只提示关键
//!    安全场景（首次解包、链接恢复）、off 全部关闭；
//! 2. 一次性状态（app.db 的 local_record/hint_state 表）：非关键的首次提示
//!    首次允许显示时先写标记，再把显示决定返回调用方，下次不再重复。
//! 链接恢复类提示不做一次性标记，每次触发都提示。
use crate::{config::HintLevel, storage::app::AppStore};
/// 提示场景：每种提示的业务触发点。
#[derive(Clone, Copy)]
pub enum HintKind {
    /// 首次初始化容器时的入门提示。
    FirstInit,
    /// 首次打包（导出 .veil 文件）时的提示。
    FirstPack,
    /// 首次解包（从 .veil 文件导入）时的安全提示。
    FirstUnpack,
    /// 链接缺失后通过本机缓存恢复等恢复入口的提示；此枚举本身不要求重新输入密码。
    LinkRecovery,
}
/// 功能：按用户提示策略筛选一条场景是否有资格显示。
/// 入参：kind: HintKind 为初始化/打包/解包/链接恢复场景；level: HintLevel 为 Full、Brief、Off。
/// 返回值：Off 全 false，Full 全 true，Brief 仅 FirstUnpack 和 LinkRecovery 为 true。
/// 异常：纯枚举匹配，不返回错误。
/// 注意事项：不读取或修改“已展示”状态，true 仅表示策略允许，还需 should_show 决定一次性提示。
fn allowed(kind: HintKind, level: HintLevel) -> bool {
    match level {
        // off：全部关闭。
        HintLevel::Off => false,
        // brief：只放行关键安全场景（首次解包、链接恢复）。
        HintLevel::Brief => matches!(kind, HintKind::FirstUnpack | HintKind::LinkRecovery),
        // full：全部放行。
        HintLevel::Full => true,
    }
}
/// 功能：合并本次命令覆盖值与用户设置，确定实际提示策略。
/// 入参：override_value: `Option<&str>` 为本次覆盖文本，按 HintLevel::parse 接受 full/brief/off 及大小写、首尾空白变体。
/// 返回值：HintLevel，有效覆盖值优先，其次本机 hint_level，所有路径无可用值时 Full。
/// 异常：不返回 Result；数据库或设置读取错误被忽略并进入默认回退。
/// 注意事项：非法覆盖文本不是报错，而是继续读设置；打开 AppStore 可能初始化本机库与默认空间。
pub(crate) fn effective_level(override_value: Option<&str>) -> HintLevel {
    override_value
        .and_then(HintLevel::parse)
        .or_else(|| {
            // 从 app.db 的用户设置读取 hint_level；任何一步失败都退回默认值。
            AppStore::global()
                .ok()?
                .user_setting("hint_level")
                .ok()?
                .as_str()
                .and_then(HintLevel::parse)
        })
        .unwrap_or(HintLevel::Full)
}
/// 功能：决定此刻是否向用户显示提示，并为非链接场景预先登记已展示状态。
/// 入参：kind: HintKind 为触发场景；override_value: `Option<&str>` 为本次提示级别覆盖文本。
/// 返回值：策略拒绝为 false；LinkRecovery 每次策略允许即 true；其他场景仅 mark_hint 首次成功登记时为 true。
/// 异常：不向外返回数据库错误，打开库或标记失败都以 false 隐藏该提示。
/// 注意事项：这是带写入的判断，标记发生在调用方真正渲染之前；后续未显示也不会自动撤销，不应重复调用它只为读取状态。
pub(crate) fn should_show(kind: HintKind, override_value: Option<&str>) -> bool {
    // 提示级别不允许直接不展示。
    if !allowed(kind, effective_level(override_value)) {
        return false;
    }
    // 链接恢复提示每次触发都展示，不落一次性标记。
    if matches!(kind, HintKind::LinkRecovery) {
        return true;
    }
    // 其余提示映射到本机记录的一次性标记键；首次调用会写入标记并返回 true。
    let key = match kind {
        HintKind::FirstInit => "first_init",
        HintKind::FirstPack => "first_pack",
        HintKind::FirstUnpack => "first_unpack",
        // 上面已提前返回，此分支逻辑上不可达。
        HintKind::LinkRecovery => unreachable!(),
    };
    AppStore::global()
        .and_then(|mut s| s.mark_hint(key))
        .unwrap_or(false)
}
