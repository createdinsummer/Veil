//! 空间与容器登记的客户端视图。
use crate::{
    config::AppConfig,
    error::VeilError,
    storage::app::AppStore,
    workflow::{self, CreateOptions, CreatedContainer, WorkflowError},
};
use serde::Serialize;
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceView {
    pub id: String,
    pub name: String,
    pub path: String,
    pub is_default: bool,
    pub containers: Vec<ContainerSummary>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerSummary {
    pub id: String,
    pub name: String,
    pub path: String,
}
pub(crate) fn list_spaces() -> Result<Vec<SpaceView>, VeilError> {
    let mut store = AppStore::global()?;
    let default = store.default_space()?.id;
    let registrations = store.registrations()?;
    Ok(store
        .spaces()?
        .into_iter()
        .map(|s| {
            let path = s.local_path.unwrap_or_default();
            let containers = registrations
                .iter()
                .filter(|r| r.space_id == s.id)
                .map(|r| ContainerSummary {
                    id: r.div_id.clone(),
                    name: r.cached_name.clone(),
                    path: path.join(&r.div_id).to_string_lossy().into_owned(),
                })
                .collect();
            SpaceView {
                is_default: s.id == default,
                id: s.id,
                name: s.name,
                path: path.to_string_lossy().into_owned(),
                containers,
            }
        })
        .collect())
}
pub(crate) fn create_space(name: &str) -> Result<(), VeilError> {
    if name.trim().is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(VeilError::WorkError("空间名称无效".into()));
    }
    let mut store = AppStore::global()?;
    if store.spaces()?.iter().any(|s| s.name == name) {
        return Err(VeilError::WorkError("空间名称已存在".into()));
    }
    store.create_space(name, None, false)?;
    Ok(())
}
pub(crate) fn create_container(
    space: &str,
    name: &str,
    password: &str,
) -> Result<CreatedContainer, WorkflowError> {
    if name.trim().is_empty()
        || name.chars().count() > 100
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
    {
        return Err(WorkflowError::InvalidName);
    }
    let link = AppConfig::config_path()?
        .parent()
        .unwrap()
        .join("links")
        .join(format!("{}.veil-link", crate::storage::id()));
    let plan = workflow::prepare_create(CreateOptions {
        target: name,
        link_output: link.to_str(),
        work_name: Some(space),
        work_root: None,
        dedicated: false,
        portable: false,
    })?;
    workflow::commit_create(plan, password)
}

/// 同设备原子移动；意图保存在本机库，进程中断后可用同一请求恢复登记。
/// 跨文件系统由系统明确拒绝，调用方可使用一致打包和独立导入。
pub fn move_container(identity: &str, target_space: &str, password: &str) -> Result<(), VeilError> {
    use crate::storage::{container::ContainerDb, invalid, json, now};
    use rusqlite::{TransactionBehavior, params};
    let mut store = AppStore::global()?;
    let registration = store
        .registrations()?
        .into_iter()
        .find(|r| r.div_id == identity)
        .ok_or_else(|| invalid("容器尚未登记"))?;
    let spaces = store.spaces()?;
    let source_space = spaces
        .iter()
        .find(|s| s.id == registration.space_id)
        .ok_or_else(|| invalid("原空间不存在"))?;
    let target = spaces
        .iter()
        .find(|s| s.id == target_space)
        .ok_or_else(|| invalid("目标空间不存在"))?;
    if source_space.id == target.id {
        return Ok(());
    }
    let source = source_space
        .local_path
        .as_ref()
        .ok_or_else(|| invalid("原空间未绑定"))?
        .join(identity);
    let destination = target
        .local_path
        .as_ref()
        .ok_or_else(|| invalid("目标空间未绑定"))?
        .join(identity);
    let mut state: serde_json::Value = store
        .record("div_state", identity)?
        .ok_or_else(|| invalid("请先解锁登记容器身份"))?;
    if let Some(movement) = state.get("movement") {
        if movement["source"] != source.to_string_lossy().as_ref()
            || movement["destination"] != destination.to_string_lossy().as_ref()
        {
            return Err(invalid("存在待恢复的移动，请先恢复原移动请求"));
        }
    }
    let actual = match (source.is_dir(), destination.is_dir()) {
        (true, false) => &source,
        (false, true) if state.get("movement").is_some() => &destination,
        (true, true) => return Err(VeilError::VeilIdConflict("原位置与目标位置同时存在".into())),
        _ => return Err(invalid("无法确认容器实际位置")),
    };
    let mut db = ContainerDb::open(actual, password, Some(identity))?;
    let container_tx = db
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let count: i64 =
        container_tx.query_row("SELECT count(*) FROM operation_locks", [], |r| r.get(0))?;
    if count != 0 {
        return Err(invalid("容器存在文件操作，请结束操作或清理失效锁后再移动"));
    }
    let app_tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current: String = app_tx.query_row(
        "SELECT space_id FROM space_div WHERE div_id=?1",
        [identity],
        |r| r.get(0),
    )?;
    if current != source_space.id {
        return Err(invalid("容器归属已变化"));
    }
    if target.storage_mode == "dedicated"
        && app_tx.query_row(
            "SELECT count(*) FROM space_div WHERE space_id=?1",
            [target_space],
            |r| r.get::<_, i64>(0),
        )? > 0
    {
        return Err(invalid("专属空间已有容器"));
    }
    state["movement"] = serde_json::json!({"source":source,"destination":destination});
    app_tx.execute("UPDATE local_record SET value_json=?1,updated_at=?2 WHERE record_type='div_state' AND record_key=?3",params![json(&state)?,now(),identity])?;
    app_tx.commit()?;
    if actual == &source {
        if let Err(error) = crate::fsutil::rename_no_replace(&source, &destination) {
            if source.is_dir() && !destination.exists() {
                state.as_object_mut().unwrap().remove("movement");
                store.set_record("div_state", identity, &state)?;
            }
            return Err(error.into());
        }
        crate::fsutil::sync_directory(source.parent().unwrap())?;
        crate::fsutil::sync_directory(destination.parent().unwrap())?;
    }
    #[cfg(feature = "fault-injection")]
    veil_core::fsutil::fault_point("after_container_move");
    // 无业务更新的容器事务在移动后结束；不清空其他进程的锁。
    container_tx.commit()?;
    drop(db);
    ContainerDb::open(&destination, password, Some(identity))?;
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    if target.storage_mode == "dedicated"
        && tx.query_row(
            "SELECT count(*) FROM space_div WHERE space_id=?1",
            [target_space],
            |r| r.get::<_, i64>(0),
        )? > 0
    {
        return Err(invalid("目标专属空间发生登记竞争，请恢复移动"));
    }
    tx.execute(
        "UPDATE space_div SET space_id=?1,updated_at=?2 WHERE div_id=?3 AND space_id=?4",
        params![target_space, now(), identity, source_space.id],
    )?;
    state.as_object_mut().unwrap().remove("movement");
    tx.execute("UPDATE local_record SET value_json=?1,updated_at=?2 WHERE record_type='div_state' AND record_key=?3",params![json(&state)?,now(),identity])?;
    let recent = crate::storage::app::RecentContainer {
        id: identity.into(),
        name: registration.cached_name,
        path: destination.to_string_lossy().into_owned(),
    };
    tx.execute("INSERT INTO local_record VALUES ('recent_div',?1,?2,?3) ON CONFLICT(record_type,record_key) DO UPDATE SET value_json=excluded.value_json,updated_at=excluded.updated_at",params![identity,json(&recent)?,now()])?;
    tx.commit()?;
    Ok(())
}
