//! 提示显示条件与一次性状态。
use crate::config::{GlobalConfig, HintLevel};

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
        .unwrap_or_else(|| {
            GlobalConfig::load()
                .map(|config| config.user_settings.hint_level)
                .unwrap_or(HintLevel::Full)
        })
}

pub(crate) fn should_show(kind: HintKind, override_value: Option<&str>) -> bool {
    if matches!(kind, HintKind::FirstInit | HintKind::LinkRecovery) {
        return allowed(kind, effective_level(override_value));
    }
    let mut config = match GlobalConfig::load() {
        Ok(config) => config,
        Err(_) => return allowed(kind, effective_level(override_value)),
    };
    let level = override_value
        .and_then(HintLevel::parse)
        .unwrap_or(config.user_settings.hint_level);
    if !allowed(kind, level) {
        return false;
    }
    let shown = match kind {
        HintKind::FirstPack => &mut config.system.pack_hint_shown,
        HintKind::FirstUnpack => &mut config.system.unpack_hint_shown,
        _ => unreachable!(),
    };
    if *shown {
        return false;
    }
    *shown = true;
    let _ = config.save();
    true
}
