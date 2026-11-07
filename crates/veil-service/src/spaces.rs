//! 空间与容器登记的客户端视图。
use crate::{
    config::AppConfig,
    error::VeilError,
    storage::app::AppStore,
    workflow::{self, CreateOptions, CreatedContainer, WorkflowError},
};
use rusqlite::{OptionalExtension, TransactionBehavior};
use serde::Serialize;
use std::time::Duration;
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceView {
    pub id: String,
    pub name: String,
    pub path: String,
    pub is_default: bool,
    pub is_available: bool,
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
            let is_available = !path.as_os_str().is_empty() && path.is_dir();
            let containers = if is_available {
                registrations
                    .iter()
                    .filter(|r| r.space_id == s.id)
                    .map(|r| ContainerSummary {
                        id: r.div_id.clone(),
                        name: r.cached_name.clone(),
                        path: path.join(&r.div_id).to_string_lossy().into_owned(),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            SpaceView {
                is_default: s.id == default,
                id: s.id,
                name: s.name,
                path: path.to_string_lossy().into_owned(),
                is_available,
                containers,
            }
        })
        .collect())
}

pub(crate) fn ensure_space_available(identity: &str) -> Result<(), VeilError> {
    let store = AppStore::global()?;
    let matches = store
        .spaces()?
        .into_iter()
        .filter(|space| space.id == identity || space.name == identity)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(VeilError::WorkError("空间不存在或名称不唯一".into()));
    }
    let path = matches[0]
        .local_path
        .as_deref()
        .ok_or_else(|| VeilError::WorkError("空间目录不可用".into()))?;
    if !path.is_dir() {
        return Err(VeilError::WorkError(
            "空间当前不可用，请检查空间目录或重新挂载外置磁盘".into(),
        ));
    }
    Ok(())
}

pub(crate) fn create_space(name: &str, path: Option<&std::path::Path>) -> Result<(), VeilError> {
    if name.trim().is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(VeilError::WorkError("空间名称无效".into()));
    }
    let mut store = AppStore::global()?;
    let spaces = store.spaces()?;
    if spaces.iter().any(|s| s.name == name) {
        return Err(VeilError::WorkError("空间名称已存在".into()));
    }
    if let Some(path) = path {
        if !path.is_absolute() {
            return Err(VeilError::WorkError("空间目录必须是绝对路径".into()));
        }
        if path.try_exists()? {
            if !path.is_dir() {
                return Err(VeilError::WorkError("请选择文件夹作为空间目录".into()));
            }
            let canonical = std::fs::canonicalize(path)?;
            if spaces
                .iter()
                .any(|s| s.local_path.as_ref() == Some(&canonical))
            {
                return Err(VeilError::WorkError(
                    "此目录已用于其他空间，请选择其他目录".into(),
                ));
            }
        }
    }
    store.create_space(name, path, false)?;
    Ok(())
}

/// 物理删除空间目录及其全部内容，并清除空间、容器和本机缓存记录。
pub fn delete_space(identity: &str) -> Result<(), VeilError> {
    delete_space_inner(identity, true)
}

/// 仅移除不可用空间的本机记录，保留磁盘上的全部文件。
pub fn delete_space_record(identity: &str) -> Result<(), VeilError> {
    delete_space_inner(identity, false)
}

fn delete_space_inner(identity: &str, delete_files: bool) -> Result<(), VeilError> {
    let mut store = AppStore::global()?;
    let space_lock = store.acquire_space_exclusive_lock(identity)?;
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stored_path: Option<String> = tx
        .query_row(
            "SELECT local_path FROM space_info WHERE id=?1",
            [identity],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| VeilError::WorkError("空间不存在".into()))?;
    let stored_path = stored_path
        .map(std::path::PathBuf::from)
        .ok_or_else(|| VeilError::WorkError("空间目录不可用".into()))?;
    if !stored_path.is_absolute() {
        return Err(VeilError::WorkError("空间目录必须是绝对路径".into()));
    }
    if delete_files && !stored_path.is_dir() {
        return Err(VeilError::WorkError(
            "空间当前不可用，不能删除磁盘文件；可选择删除空间记录".into(),
        ));
    }
    if !delete_files && stored_path.is_dir() {
        return Err(VeilError::WorkError(
            "空间仍可访问，请使用删除空间以同时删除磁盘文件".into(),
        ));
    }
    let app_directory = std::fs::canonicalize(&store.directory)?;
    if stored_path == app_directory || app_directory.starts_with(&stored_path) {
        return Err(VeilError::WorkError(
            "拒绝删除文件系统根目录或包含 Veil 数据库的目录".into(),
        ));
    }
    let space_ids = if delete_files {
        let mut statement =
            tx.prepare("SELECT id,local_path FROM space_info WHERE local_path IS NOT NULL")?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|(id, path)| {
                std::path::PathBuf::from(path)
                    .starts_with(&stored_path)
                    .then_some(id)
            })
            .collect::<Vec<_>>()
    } else {
        vec![identity.to_owned()]
    };
    if !space_ids.iter().any(|id| id == identity) {
        return Err(VeilError::WorkError("空间目录登记无效".into()));
    }
    if delete_files {
        std::fs::remove_dir_all(&stored_path)?;
    }
    let default_id: Option<String> = tx
        .query_row(
            "SELECT value_json FROM system_setting WHERE key='default_space_id'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let encoded_space_ids = space_ids
        .iter()
        .map(crate::storage::json)
        .collect::<Result<Vec<_>, _>>()?;
    if default_id
        .as_ref()
        .is_some_and(|default| encoded_space_ids.iter().any(|id| id == default))
    {
        tx.execute(
            "DELETE FROM system_setting WHERE key='default_space_id'",
            [],
        )?;
    }
    let space_ids_json = crate::storage::json(&space_ids)?;
    tx.execute(
        "DELETE FROM local_record WHERE record_type IN ('recent_div','div_state') AND record_key IN (SELECT div_id FROM space_div WHERE space_id IN (SELECT value FROM json_each(?1)))",
        [&space_ids_json],
    )?;
    tx.execute(
        "DELETE FROM local_record WHERE record_type='link_cache' AND json_extract(value_json,'$.link.veil.veil_id') IN (SELECT div_id FROM space_div WHERE space_id IN (SELECT value FROM json_each(?1)))",
        [&space_ids_json],
    )?;
    tx.execute(
        "DELETE FROM local_record WHERE record_type='space_access' AND record_key IN (SELECT value FROM json_each(?1))",
        [&space_ids_json],
    )?;
    tx.execute(
        "DELETE FROM space_div WHERE space_id IN (SELECT value FROM json_each(?1))",
        [&space_ids_json],
    )?;
    if tx.execute(
        "DELETE FROM space_info WHERE id IN (SELECT value FROM json_each(?1))",
        [&space_ids_json],
    )? == 0
    {
        return Err(VeilError::WorkError("空间不存在".into()));
    }
    tx.commit()?;
    drop(space_lock);
    Ok(())
}

/// 删除已登记容器的加密目录，并清除本机登记和最近记录。
pub fn delete_container(identity: &str) -> Result<(), VeilError> {
    crate::work::validate_veil_id(identity)?;
    let mut store = AppStore::global()?;
    let registration = store
        .registrations()?
        .into_iter()
        .find(|registration| registration.div_id == identity)
        .ok_or_else(|| VeilError::WorkError("容器未登记到空间".into()))?;
    // 与空间删除的独占锁互斥，避免空间目录正在移除时继续删除容器。
    let _space_lock = store.acquire_space_shared_lock(&registration.space_id)?;
    let space = store
        .spaces()?
        .into_iter()
        .find(|space| space.id == registration.space_id)
        .ok_or_else(|| VeilError::WorkError("容器所属空间不存在".into()))?;
    let root = space
        .local_path
        .ok_or_else(|| VeilError::WorkError("空间目录不可用".into()))?;
    let root = std::fs::canonicalize(root)?;
    let container_path = root.join(identity);
    let metadata = std::fs::symlink_metadata(&container_path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(VeilError::WorkError("容器目录无效".into()));
    }
    let container_path = std::fs::canonicalize(container_path)?;
    if container_path.parent() != Some(root.as_path())
        || container_path.file_name().and_then(|name| name.to_str()) != Some(identity)
    {
        return Err(VeilError::WorkError("容器目录超出空间范围".into()));
    }

    // 删除容器只依据空间登记和目录路径，不读取加密库，也不验证容器版本或密码。
    let tombstone = root.join(format!(
        ".veil-deleting-{}-{}",
        identity,
        crate::storage::id()
    ));
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current_space: Option<String> = tx
        .query_row(
            "SELECT space_id FROM space_div WHERE div_id=?1",
            [identity],
            |row| row.get(0),
        )
        .optional()?;
    if current_space.as_deref() != Some(registration.space_id.as_str()) {
        return Err(VeilError::WorkError("容器归属已变化，请刷新后重试".into()));
    }

    tx.execute("DELETE FROM space_div WHERE div_id=?1", [identity])?;
    tx.execute(
        "DELETE FROM local_record WHERE record_key=?1 AND record_type IN ('recent_div','div_state')",
        [identity],
    )?;
    tx.execute(
        "DELETE FROM local_record WHERE record_type='link_cache' AND json_extract(value_json,'$.link.veil.veil_id')=?1",
        [identity],
    )?;
    veil_core::fsutil::rename_no_replace(&container_path, &tombstone)?;
    if let Err(error) = veil_core::fsutil::sync_directory(&root) {
        let _ = veil_core::fsutil::rename_no_replace(&tombstone, &container_path);
        return Err(error.into());
    }
    if let Err(error) = tx.commit() {
        let _ = veil_core::fsutil::rename_no_replace(&tombstone, &container_path);
        let _ = veil_core::fsutil::sync_directory(&root);
        return Err(error.into());
    }

    std::fs::remove_dir_all(&tombstone).map_err(|error| {
        VeilError::WorkError(format!(
            "容器已从空间移除，但临时数据清理失败（{}）：{error}",
            tombstone.display()
        ))
    })?;
    veil_core::fsutil::sync_directory(&root)?;
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
    ensure_space_available(space)?;
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
    let _source_space_lock = store.acquire_space_shared_lock(&source_space.id)?;
    let _target_space_lock = store.acquire_space_shared_lock(&target.id)?;
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
