//! 提示偏好及一次性本机状态。
use crate::{config::HintLevel, storage::app::AppStore};
#[derive(Clone, Copy)]
pub enum HintKind {
    FirstInit,
    FirstPack,
    FirstUnpack,
    LinkRecovery,
}
fn allowed(kind: HintKind, level: HintLevel) -> bool {
    match level {
        HintLevel::Off => false,
        HintLevel::Brief => matches!(kind, HintKind::FirstUnpack | HintKind::LinkRecovery),
        HintLevel::Full => true,
    }
}
pub(crate) fn effective_level(override_value: Option<&str>) -> HintLevel {
    override_value
        .and_then(HintLevel::parse)
        .or_else(|| {
            AppStore::global()
                .ok()?
                .user_setting("hint_level")
                .ok()?
                .as_str()
                .and_then(HintLevel::parse)
        })
        .unwrap_or(HintLevel::Full)
}
pub(crate) fn should_show(kind: HintKind, override_value: Option<&str>) -> bool {
    if !allowed(kind, effective_level(override_value)) {
        return false;
    }
    if matches!(kind, HintKind::LinkRecovery) {
        return true;
    }
    let key = match kind {
        HintKind::FirstInit => "first_init",
        HintKind::FirstPack => "first_pack",
        HintKind::FirstUnpack => "first_unpack",
        HintKind::LinkRecovery => unreachable!(),
    };
    AppStore::global()
        .and_then(|mut s| s.mark_hint(key))
        .unwrap_or(false)
}
