//! 空间与容器登记的客户端视图。
use crate::{
    config::AppConfig,
    error::VeilError,
    storage::app::AppStore,
    workflow::{self, CreateOptions, CreatedContainer, WorkflowError},
};
use rusqlite::{OptionalExtension, TransactionBehavior};
use serde::Serialize;
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
    let mut phase = None;
    let result = delete_space_staged(identity, delete_files, &mut phase);
    diagnose_partial_mutation("delete_space", identity, phase, &result);
    result
}

fn delete_space_staged(
    identity: &str,
    delete_files: bool,
    phase: &mut Option<&'static str>,
) -> Result<(), VeilError> {
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
    if space_has_active_movement(&tx, &space_ids, &stored_path)? {
        return Err(VeilError::WorkError(
            "空间中有容器正在移动，不能删除空间".into(),
        ));
    }
    if delete_files {
        *phase = Some("remove_files");
        std::fs::remove_dir_all(&stored_path)?;
        *phase = Some("commit_registration");
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

fn space_has_active_movement(
    tx: &rusqlite::Transaction<'_>,
    space_ids: &[String],
    root: &std::path::Path,
) -> Result<bool, VeilError> {
    let mut statement = tx.prepare(
        "SELECT value_json FROM local_record WHERE record_type='div_state' AND json_type(value_json,'$.movement') IS NOT NULL",
    )?;
    let states = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for raw in states {
        let state: serde_json::Value = crate::storage::parse(&raw)?;
        let Some(movement) = state.get("movement") else {
            continue;
        };
        let references_space = ["source_space_id", "target_space_id"]
            .into_iter()
            .filter_map(|key| movement.get(key).and_then(serde_json::Value::as_str))
            .any(|id| space_ids.iter().any(|space| space == id));
        let includes_path = ["source", "destination"]
            .into_iter()
            .filter_map(|key| movement.get(key).and_then(serde_json::Value::as_str))
            .map(std::path::Path::new)
            .any(|path| path.starts_with(root));
        if references_space || includes_path {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 删除已登记容器的加密目录，并清除本机登记和最近记录。
pub fn delete_container(identity: &str) -> Result<(), VeilError> {
    let mut phase = None;
    let result = delete_container_staged(identity, &mut phase);
    diagnose_partial_mutation("delete_container", identity, phase, &result);
    result
}

fn delete_container_staged(
    identity: &str,
    phase: &mut Option<&'static str>,
) -> Result<(), VeilError> {
    let mut store = AppStore::global()?;
    delete_container_in_store(&mut store, identity, phase)
}

fn delete_container_in_store(
    store: &mut AppStore,
    identity: &str,
    phase: &mut Option<&'static str>,
) -> Result<(), VeilError> {
    crate::work::validate_veil_id(identity)?;
    let _container_lock = store.acquire_container_delete_lock(identity)?;
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

    // 排他锁保护整个删除过程；事务只清除 app.db 记录。
    *phase = Some("commit_registration");
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    check_container_delete_registration(&tx, identity, &registration.space_id)?;
    tx.execute("DELETE FROM space_div WHERE div_id=?1", [identity])?;
    tx.execute("DELETE FROM local_record WHERE record_key=?1 AND record_type IN ('recent_div','div_state')", [identity])?;
    tx.execute("DELETE FROM local_record WHERE record_type='link_cache' AND json_extract(value_json,'$.link.veil.veil_id')=?1", [identity])?;
    tx.commit()?;

    // 直接删除整个目录，容器 DB 和密文文件一起移除，不改名、不恢复目录。
    debug_assert!(store.conn.is_autocommit());
    veil_core::database::invalidate(&container_path.join(veil_core::database::CONTAINER_DB));
    *phase = Some("remove_files");
    std::fs::remove_dir_all(&container_path).map_err(|error| {
        VeilError::WorkError(format!(
            "容器登记已删除，但容器目录删除失败（{}）：{error}",
            container_path.display()
        ))
    })?;
    *phase = Some("sync_removed_directory");
    veil_core::fsutil::sync_directory(&root)?;
    Ok(())
}
fn check_container_delete_registration(
    conn: &rusqlite::Connection,
    identity: &str,
    expected_space: &str,
) -> Result<(), VeilError> {
    use crate::storage::invalid;
    let moving: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_record WHERE record_type='div_state' AND record_key=?1 AND json_type(value_json,'$.movement') IS NOT NULL)",
        [identity], |row| row.get(0),
    )?;
    if moving {
        return Err(invalid("容器正在移动，不能删除"));
    }
    let current_space: Option<String> = conn
        .query_row(
            "SELECT space_id FROM space_div WHERE div_id=?1",
            [identity],
            |row| row.get(0),
        )
        .optional()?;
    if current_space.as_deref() != Some(expected_space) {
        return Err(invalid("容器归属已变化，请刷新后重试"));
    }
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

/// 复制为独立的新容器；源登记、文件和当前会话保持不变。
pub fn move_container(identity: &str, target_space: &str, password: &str) -> Result<(), VeilError> {
    use crate::storage::{
        container::ContainerDb,
        invalid,
        locks::{self, LockManager, LockMode},
    };
    crate::work::validate_veil_id(identity)?;
    let store = AppStore::global()?;
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
    let _source_space_lock = store.acquire_space_shared_lock(&source_space.id)?;
    let _target_space_lock = store.acquire_space_shared_lock(target_space)?;
    ensure_space_available(target_space)?;
    let source = source_space
        .local_path
        .as_ref()
        .ok_or_else(|| invalid("原空间未绑定"))?
        .join(identity);
    ensure_regular_container_database(&source)?;
    let source_db = ContainerDb::open(&source, password, Some(identity))?;
    let name = source_db.info()?.1;
    let link = AppConfig::config_path()?
        .parent()
        .unwrap()
        .join("links")
        .join(format!("{}.veil-link", crate::storage::id()));
    // 创建计划生成新 ID，并检查目标容器不存在；此时不创建空数据库或登记。
    let plan = workflow::prepare_create(CreateOptions {
        target: &name,
        link_output: link.to_str(),
        work_name: Some(target_space),
        work_root: None,
        dedicated: false,
        portable: false,
    })
    .map_err(|error| invalid(&error.to_string()))?;
    let destination = plan.directory().to_path_buf();
    let _source_lifecycle_lock = store.acquire_container_shared_lock(identity)?;
    let source_guard = LockManager::new(&source, password).acquire_container(LockMode::Read)?;
    let mut target_guard = None;
    let mut owned_target = false;
    let result = workflow::commit_create_with_initializer(
        plan,
        password,
        |directory, id, name, password| {
            owned_target = true;
            for credential in source_guard.credentials()? {
                locks::validate(&source_db.conn, &credential, Some(LockMode::Read))?;
            }
            // 先复制 DB 并修正新身份、清除源锁，再对该目标 DB 加锁。
            copy_container_database(&source_db, directory, id, name, password)?;
            let guard = LockManager::new(directory, password).acquire_container(LockMode::Write)?;
            copy_container_tree(&source, directory)?;
            verify_container_copy(directory, password, id)?;
            let target = ContainerDb::open(directory, password, Some(id))?;
            for credential in source_guard.credentials()? {
                locks::validate(&source_db.conn, &credential, Some(LockMode::Read))?;
            }
            for credential in guard.credentials()? {
                locks::validate(&target.conn, &credential, Some(LockMode::Write))?;
            }
            let metadata = target.metadata()?;
            target_guard = Some(guard);
            Ok(metadata)
        },
    );
    if let Err(error) = result {
        drop(target_guard.take());
        // 初始化失败由创建流程回收；登记阶段失败则清理本次已拥有的目标。
        if owned_target && destination.try_exists()? {
            let target_id = destination.file_name().unwrap().to_string_lossy();
            let cleanup = if store.registrations()?.iter().any(|r| r.div_id == target_id) {
                delete_container(&target_id)
            } else {
                veil_core::database::invalidate(
                    &destination.join(veil_core::database::CONTAINER_DB),
                );
                std::fs::remove_dir_all(&destination).map_err(VeilError::from)
            };
            if let Err(cleanup) = cleanup {
                return Err(invalid(&format!(
                    "复制失败：{error}；目标残留 {} 清理失败：{cleanup}",
                    destination.display()
                )));
            }
        }
        return Err(invalid(&error.to_string()));
    }
    if let Some(guard) = target_guard {
        guard.release()?;
    }
    source_guard.release()?;
    Ok(())
}

/// 先直接复制文件，再修改目标身份和清除源操作锁；调用方随后获取目标排他锁。
fn copy_container_database(
    source: &crate::storage::container::ContainerDb,
    destination: &std::path::Path,
    identity: &str,
    name: &str,
    password: &str,
) -> Result<(), VeilError> {
    use crate::storage::{invalid, now};
    crate::work::validate_veil_id(identity)?;
    let path = destination.join(veil_core::database::CONTAINER_DB);
    // 禁止覆盖任何已有目标 DB；目录由创建流程原子占位。
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    drop(file);
    copy_database_file(source, &path)?;
    let mut target = veil_core::database::open(&path, Some(password), false)?;
    let integrity: String = target.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(invalid("容器数据库文件副本完整性校验失败"));
    }
    let tx = target.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "UPDATE div_info SET veil_id=?1,name=?2,created_at=?3 WHERE id=1",
        rusqlite::params![identity, name, now()],
    )?;
    tx.execute("DELETE FROM operation_locks", [])?;
    tx.commit()?;
    drop(target);
    veil_core::database::invalidate(&path);
    veil_core::fsutil::sync_file(&std::fs::File::open(&path)?)?;
    veil_core::fsutil::sync_directory(destination)?;
    Ok(())
}

/// 调用方持有源容器读锁；直接复制文件，不开启数据库事务。
fn copy_database_file(
    source: &crate::storage::container::ContainerDb,
    destination: &std::path::Path,
) -> Result<(), VeilError> {
    let source_path = source.directory.join(veil_core::database::CONTAINER_DB);
    let expected = std::fs::metadata(&source_path)?.len();
    let copied = std::fs::copy(&source_path, destination)?;
    if copied != expected {
        return Err(crate::storage::invalid("容器数据库文件复制长度不一致"));
    }
    veil_core::fsutil::sync_file(&std::fs::File::open(destination)?)?;
    Ok(())
}

fn ensure_regular_container_database(directory: &std::path::Path) -> Result<(), VeilError> {
    let metadata = std::fs::symlink_metadata(directory.join(veil_core::database::CONTAINER_DB))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(crate::storage::invalid("容器数据库文件无效"));
    }
    Ok(())
}

fn copy_container_tree(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> Result<(), VeilError> {
    use crate::storage::invalid;
    use std::fs;

    fn copy_directory(
        source: &std::path::Path,
        destination: &std::path::Path,
        root: bool,
    ) -> Result<(), VeilError> {
        use crate::storage::invalid;
        use std::{fs, io};
        let mut entries = fs::read_dir(source)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let source_path = entry.path();
            let name = entry.file_name();
            if root && name == veil_core::database::CONTAINER_DB {
                continue;
            }
            if root
                && ["veil.db-journal", "veil.db-wal", "veil.db-shm"]
                    .contains(&name.to_string_lossy().as_ref())
            {
                continue;
            }
            let destination_path = destination.join(name);
            let metadata = fs::symlink_metadata(&source_path)?;
            if metadata.file_type().is_symlink() {
                return Err(invalid("容器目录中不能包含符号链接"));
            }
            if metadata.is_dir() {
                fs::create_dir(&destination_path)?;
                veil_core::fsutil::sync_directory(
                    destination_path
                        .parent()
                        .unwrap_or(std::path::Path::new(".")),
                )?;
                copy_directory(&source_path, &destination_path, false)?;
                fs::set_permissions(&destination_path, metadata.permissions())?;
                veil_core::fsutil::sync_directory(&destination_path)?;
            } else if metadata.is_file() {
                let mut input = fs::File::open(&source_path)?;
                let mut output = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&destination_path)?;
                let copied = io::copy(&mut input, &mut output)?;
                if copied != metadata.len() {
                    return Err(invalid("容器文件复制长度不一致"));
                }
                fs::set_permissions(&destination_path, metadata.permissions())?;
                veil_core::fsutil::sync_file(&output)?;
                veil_core::fsutil::sync_directory(destination)?;
            } else {
                return Err(invalid("容器目录中包含不支持的文件类型"));
            }
        }
        veil_core::fsutil::sync_directory(destination)?;
        Ok(())
    }

    let source_metadata = fs::symlink_metadata(source)?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
        return Err(invalid("原容器目录无效"));
    }
    copy_directory(source, destination, true)?;
    fs::set_permissions(destination, source_metadata.permissions())?;
    veil_core::fsutil::sync_directory(destination)?;
    Ok(())
}

fn verify_container_copy(
    directory: &std::path::Path,
    password: &str,
    identity: &str,
) -> Result<(), VeilError> {
    use crate::storage::{container::ContainerDb, invalid};

    fn validate_tree(directory: &std::path::Path) -> Result<(), VeilError> {
        use crate::storage::invalid;
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(invalid("容器目录中不能包含符号链接"));
            }
            if metadata.is_dir() {
                validate_tree(&path)?;
            } else if !metadata.is_file() {
                return Err(invalid("容器目录中包含不支持的文件类型"));
            }
        }
        Ok(())
    }

    let directory_metadata = std::fs::symlink_metadata(directory)?;
    if directory_metadata.file_type().is_symlink() || !directory_metadata.is_dir() {
        return Err(invalid("目标容器目录无效"));
    }
    validate_tree(directory)?;
    ensure_regular_container_database(directory)?;
    let db = ContainerDb::open(directory, password, Some(identity))?;
    let metadata = db.metadata()?;
    for file in &metadata.files {
        let path = directory.join(&file.encrypted_name);
        let entry = std::fs::symlink_metadata(&path)?;
        if entry.file_type().is_symlink() || !entry.is_file() {
            return Err(invalid("目标容器缺少有效的密文文件"));
        }
        let node = db
            .node(&file.file_id)?
            .ok_or_else(|| invalid("文件身份缺失"))?;
        let key = db
            .keys()?
            .into_iter()
            .find(|key| Some(&key.key_id) == node.key_id.as_ref())
            .ok_or_else(|| invalid("文件密钥缺失"))?;
        veil_core::file_ops::decrypt_file_to_writer(
            &path,
            &mut std::io::sink(),
            &*key.material()?,
            &file.nonce,
            file.size,
        )?;
    }
    Ok(())
}
/// 在事务和空间锁释放后记录跨存储操作的部分结果，避免延长临界区。
fn diagnose_partial_mutation<T>(
    operation: &'static str,
    identity: &str,
    phase: Option<&'static str>,
    result: &Result<T, VeilError>,
) {
    if let (Some(phase), Err(error)) = (phase, result) {
        use veil_core::logging::{self, Field, Level};
        logging::failure(
            Level::Error,
            "service",
            operation,
            "STORAGE_MUTATION_INCOMPLETE",
            error,
            &[
                ("resource", Field::Resource(identity)),
                ("phase", Field::Label(phase)),
                ("outcome", Field::Label("verify_storage_and_registration")),
            ],
        );
    }
}

#[cfg(test)]
mod database_copy_tests {
    use super::{copy_container_database, copy_database_file};
    use crate::storage::{
        container::ContainerDb,
        locks::{LockManager, LockMode, LockOptions},
    };
    use std::time::Duration;

    #[test]
    fn database_file_copy_is_byte_for_byte_without_source_transaction() {
        let root = tempfile::tempdir().unwrap();
        let id = veil_core::metadata::generate_veil_id();
        let directory = root.path().join(&id);
        let source = ContainerDb::create(&directory, &id, "source", "pw").unwrap();
        let original = std::fs::read(directory.join(veil_core::database::CONTAINER_DB)).unwrap();
        let destination = root.path().join("copied.db");
        copy_database_file(&source, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), original);
        assert!(source.conn.is_autocommit());
        assert!(copy_database_file(&source, root.path()).is_err());
        assert!(source.conn.is_autocommit());
        assert_eq!(
            std::fs::read(directory.join(veil_core::database::CONTAINER_DB)).unwrap(),
            original
        );
    }

    #[test]
    fn database_is_copied_and_reidentified_before_target_lock() {
        let root = tempfile::tempdir().unwrap();
        let source_id = veil_core::metadata::generate_veil_id();
        let target_id = veil_core::metadata::generate_veil_id();
        let source_path = root.path().join(&source_id);
        let target_path = root.path().join(&target_id);
        let source = ContainerDb::create(&source_path, &source_id, "source", "pw").unwrap();
        assert!(!target_path.exists());
        std::fs::create_dir(&target_path).unwrap();
        let source_guard = LockManager::new(&source_path, "pw")
            .acquire_container(LockMode::Read)
            .unwrap();
        let manager = LockManager::new(&target_path, "pw").with_options(LockOptions {
            timeout: Some(Duration::from_millis(50)),
            cancellation: None,
        });
        copy_container_database(&source, &target_path, &target_id, "target", "pw").unwrap();
        let copied = ContainerDb::open(&target_path, "pw", Some(&target_id)).unwrap();
        assert_eq!(
            copied
                .conn
                .query_row("SELECT count(*) FROM operation_locks", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(copied);
        let target_guard = manager.acquire_container(LockMode::Write).unwrap();
        let original_lock = target_guard.credentials().unwrap()[0].lock_id.clone();
        assert!(source.conn.is_autocommit());
        let target = ContainerDb::open(&target_path, "pw", Some(&target_id)).unwrap();
        assert_eq!(target.info().unwrap().0, target_id);
        assert_eq!(target.info().unwrap().1, "target");
        assert_eq!(target.root().unwrap(), source.root().unwrap());
        assert_eq!(
            target.keys().unwrap()[0].key_material,
            source.keys().unwrap()[0].key_material
        );
        let locks: Vec<String> = target
            .conn
            .prepare("SELECT lock_id FROM operation_locks")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(locks, vec![original_lock]);
        let before = std::fs::read(target_path.join(veil_core::database::CONTAINER_DB)).unwrap();
        assert!(
            copy_container_database(&source, &target_path, &target_id, "target", "pw").is_err()
        );
        assert_eq!(
            std::fs::read(target_path.join(veil_core::database::CONTAINER_DB)).unwrap(),
            before
        );
        assert!(manager.acquire_container(LockMode::Read).is_err());
        assert!(manager.acquire_container(LockMode::Write).is_err());
        target_guard.release().unwrap();
        manager
            .acquire_container(LockMode::Write)
            .unwrap()
            .release()
            .unwrap();
        source_guard.release().unwrap();
        assert_eq!(std::fs::read_dir(&target_path).unwrap().count(), 1);
    }
}

#[cfg(test)]
mod container_delete_tests {
    use super::delete_container_in_store;
    use crate::storage::app::AppStore;

    fn fixture() -> (tempfile::TempDir, AppStore, String, std::path::PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let mut store = AppStore::open(&root.path().join("app")).unwrap();
        let space = store.create_space("delete", None, false).unwrap();
        let id = veil_core::metadata::generate_veil_id();
        let directory = space.local_path.unwrap().join(&id);
        std::fs::create_dir(&directory).unwrap();
        // 删除不依赖容器密码、数据库是否完整或业务结构版本。
        std::fs::write(directory.join("veil.db"), b"broken encrypted database").unwrap();
        std::fs::write(directory.join("cipher.enc"), b"ciphertext").unwrap();
        store.register(&space.id, &id, "delete").unwrap();
        store
            .conn
            .execute(
                "INSERT INTO local_record VALUES('recent_div',?1,'{}',1)",
                [&id],
            )
            .unwrap();
        (root, store, id, directory)
    }

    #[test]
    fn delete_uses_lifecycle_lock_and_leaves_other_container_available() {
        let (_root, mut store, id, directory) = fixture();
        let shared = store.acquire_container_shared_lock(&id).unwrap();
        let mut phase = None;
        assert!(delete_container_in_store(&mut store, &id, &mut phase).is_err());
        assert!(phase.is_none());
        assert!(directory.is_dir());
        assert_eq!(store.registrations().unwrap().len(), 1);
        drop(shared);
        let other_id = veil_core::metadata::generate_veil_id();
        let _other_lock = store.acquire_container_shared_lock(&other_id).unwrap();
        delete_container_in_store(&mut store, &id, &mut phase).unwrap();
        assert!(store.conn.is_autocommit());
        assert!(!directory.exists());
        assert!(store.registrations().unwrap().is_empty());
        assert_eq!(
            store
                .conn
                .query_row(
                    "SELECT count(*) FROM local_record WHERE record_key=?1",
                    [&id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            std::fs::read_dir(directory.parent().unwrap())
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn registration_failure_keeps_directory_untouched() {
        let (_root, mut store, id, directory) = fixture();
        store.conn.execute_batch("CREATE TEMP TRIGGER reject_delete BEFORE DELETE ON main.space_div BEGIN SELECT RAISE(ABORT,'injected delete failure'); END;").unwrap();
        let mut phase = None;
        assert!(delete_container_in_store(&mut store, &id, &mut phase).is_err());
        assert_eq!(phase, Some("commit_registration"));
        assert_eq!(
            std::fs::read(directory.join("veil.db")).unwrap(),
            b"broken encrypted database"
        );
        assert!(store.conn.is_autocommit());
        assert_eq!(
            std::fs::read(directory.join("cipher.enc")).unwrap(),
            b"ciphertext"
        );
        assert_eq!(store.registrations().unwrap().len(), 1);
        assert_eq!(
            std::fs::read_dir(directory.parent().unwrap())
                .unwrap()
                .count(),
            1
        );
        store
            .conn
            .execute_batch("DROP TRIGGER reject_delete;")
            .unwrap();
        delete_container_in_store(&mut store, &id, &mut phase).unwrap();
        assert!(!directory.exists());
    }
    #[cfg(unix)]
    #[test]
    fn directory_removal_failure_does_not_restore_deleted_registration() {
        use std::os::unix::fs::PermissionsExt;
        let (_root, mut store, id, directory) = fixture();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();
        let mut phase = None;
        let result = delete_container_in_store(&mut store, &id, &mut phase);
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let error = result.unwrap_err();
        assert_eq!(phase, Some("remove_files"));
        assert!(error.to_string().contains(directory.to_str().unwrap()));
        assert!(store.conn.is_autocommit());
        assert!(store.registrations().unwrap().is_empty());
        assert!(directory.is_dir());
        assert_eq!(
            std::fs::read_dir(directory.parent().unwrap())
                .unwrap()
                .count(),
            1
        );
        assert!(store.acquire_container_delete_lock(&id).is_ok());
    }
}
