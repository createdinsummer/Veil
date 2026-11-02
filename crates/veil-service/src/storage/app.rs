//! 本机索引、设置与缓存，不允许写入容器密码或文件密钥。
use super::{id, invalid, json, migrate, now, parse};
use crate::error::Result;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

static PLATFORM_DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
/// 移动端在初始化服务前传入应用私有目录；桌面端与 CLI 使用相同默认策略。
pub fn configure_app_data_dir(path: PathBuf) -> Result<()> {
    if !path.is_absolute() {
        return Err(invalid("应用目录必须是绝对路径"));
    }
    PLATFORM_DIRECTORY
        .set(path)
        .map_err(|_| invalid("应用目录已初始化"))
}
pub fn app_data_dir() -> Result<PathBuf> {
    if let Some(path) = PLATFORM_DIRECTORY.get() {
        return Ok(path.clone());
    }
    if let Some(path) = std::env::var_os("VEIL_APP_DATA_DIR") {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(invalid("VEIL_APP_DATA_DIR 必须是绝对路径"));
        }
        return Ok(path);
    }
    #[cfg(test)]
    {
        static TEST_DIRECTORY: OnceLock<tempfile::TempDir> = OnceLock::new();
        return Ok(TEST_DIRECTORY
            .get_or_init(|| tempfile::TempDir::new().expect("测试目录"))
            .path()
            .into());
    }
    #[cfg(not(test))]
    dirs::data_local_dir()
        .map(|p| p.join("Veil"))
        .ok_or_else(|| invalid("无法定位应用数据目录"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Space {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub local_path: Option<PathBuf>,
    pub storage_mode: String,
    pub sort_order: i64,
    pub created_at: i64,
    pub updated_at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registration {
    pub div_id: String,
    pub space_id: String,
    pub cached_name: String,
    pub sort_order: i64,
    pub registered_at: i64,
    pub updated_at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecentContainer {
    pub id: String,
    pub name: String,
    pub path: String,
}

pub struct AppStore {
    pub(crate) conn: Connection,
    pub directory: PathBuf,
}
impl AppStore {
    pub fn global() -> Result<Self> {
        Self::open(&app_data_dir()?)
    }
    pub fn open(directory: &Path) -> Result<Self> {
        crate::fsutil::create_dir_all_durable(directory)?;
        let path = directory.join(veil_core::database::APP_DB);
        let create = !path.try_exists()?;
        let mut conn = veil_core::database::open(&path, None, true)?;
        // 另一个首次启动进程可能已经创建文件、尚未提交结构；只等待空结构，绝不接管未知库。
        if !create {
            let start = std::time::Instant::now();
            loop {
                let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
                let tables: i64 = conn.query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table'",
                    [],
                    |r| r.get(0),
                )?;
                if version != 0
                    || tables != 0
                    || start.elapsed() >= veil_core::database::BUSY_TIMEOUT
                {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        migrate(&mut conn, 0x56415050, include_str!("app.sql"), create)?;
        let mut store = Self {
            conn,
            directory: directory.to_path_buf(),
        };
        let tx = store
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let device: Option<String> = tx
            .query_row(
                "SELECT value_json FROM system_setting WHERE key='device_id'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(value) = device {
            let value: String = parse(&value)?;
            if !veil_core::metadata::is_uuid_v4(&value) {
                return Err(invalid("设备身份为空"));
            }
        } else {
            tx.execute(
                "INSERT INTO system_setting VALUES ('device_id',?1,?2)",
                params![json(&id())?, now()],
            )?;
        }
        tx.commit()?;
        Ok(store)
    }
    pub fn device_id(&self) -> Result<String> {
        self.system("device_id")?
            .ok_or_else(|| invalid("设备身份缺失"))
    }
    pub fn spaces(&self) -> Result<Vec<Space>> {
        let mut stmt=self.conn.prepare("SELECT id,name,description,local_path,storage_mode,sort_order,created_at,updated_at FROM space_info ORDER BY sort_order,id")?;
        Ok(stmt
            .query_map([], |r| {
                Ok(Space {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    description: r.get(2)?,
                    local_path: r.get::<_, Option<String>>(3)?.map(PathBuf::from),
                    storage_mode: r.get(4)?,
                    sort_order: r.get(5)?,
                    created_at: r.get(6)?,
                    updated_at: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn create_space(
        &mut self,
        name: &str,
        path: Option<&Path>,
        dedicated: bool,
    ) -> Result<Space> {
        if name.trim().is_empty() {
            return Err(invalid("空间名称不能为空"));
        }
        let identity = id();
        let path = path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.directory.join("spaces").join(&identity));
        crate::fsutil::create_dir_all_durable(&path)?;
        let path = std::fs::canonicalize(path)?;
        let time = now();
        self.conn.execute(
            "INSERT INTO space_info VALUES (?1,?2,NULL,?3,?4,0,?5,?5)",
            params![
                identity,
                name,
                path.to_str().ok_or_else(|| invalid("路径不是 UTF-8"))?,
                if dedicated { "dedicated" } else { "shared" },
                time
            ],
        )?;
        Ok(Space {
            id: identity,
            name: name.into(),
            description: None,
            local_path: Some(path),
            storage_mode: if dedicated {
                "dedicated".into()
            } else {
                "shared".into()
            },
            sort_order: 0,
            created_at: time,
            updated_at: time,
        })
    }
    pub fn rename_space(&self, identity: &str, name: &str) -> Result<()> {
        if name.trim().is_empty() {
            return Err(invalid("空间名称不能为空"));
        }
        if self.conn.execute(
            "UPDATE space_info SET name=?1,updated_at=?2 WHERE id=?3",
            params![name, now(), identity],
        )? != 1
        {
            return Err(invalid("空间不存在"));
        }
        Ok(())
    }
    pub fn default_space(&mut self) -> Result<Space> {
        if let Some(identity) = self.system::<String>("default_space_id")? {
            return self
                .spaces()?
                .into_iter()
                .find(|s| s.id == identity)
                .ok_or_else(|| invalid("默认空间引用无效"));
        }
        // 同一事务检查并创建，避免多个首次启动各生成默认空间。
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT value_json FROM system_setting WHERE key='default_space_id'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if existing.is_none() {
            let identity = id();
            let path = self.directory.join("spaces").join(&identity);
            crate::fsutil::create_dir_all_durable(&path)?;
            tx.execute(
                "INSERT INTO space_info VALUES (?1,'默认空间',NULL,?2,'shared',0,?3,?3)",
                params![
                    identity,
                    std::fs::canonicalize(path)?
                        .to_str()
                        .ok_or_else(|| invalid("路径不是 UTF-8"))?,
                    now()
                ],
            )?;
            tx.execute(
                "INSERT INTO system_setting VALUES ('default_space_id',?1,?2)",
                params![json(&identity)?, now()],
            )?;
        }
        tx.commit()?;
        self.default_space()
    }
    pub fn set_default_space(&mut self, identity: &str) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM space_info WHERE id=?1)",
            [identity],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(invalid("默认空间不存在"));
        }
        tx.execute("INSERT INTO system_setting VALUES ('default_space_id',?1,?2) ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json,updated_at=excluded.updated_at",params![json(identity)?,now()])?;
        tx.commit()?;
        Ok(())
    }
    pub fn registrations(&self) -> Result<Vec<Registration>> {
        let mut stmt=self.conn.prepare("SELECT div_id,space_id,cached_name,sort_order,registered_at,updated_at FROM space_div ORDER BY space_id,sort_order,div_id")?;
        Ok(stmt
            .query_map([], |r| {
                Ok(Registration {
                    div_id: r.get(0)?,
                    space_id: r.get(1)?,
                    cached_name: r.get(2)?,
                    sort_order: r.get(3)?,
                    registered_at: r.get(4)?,
                    updated_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn register(&mut self, space: &str, div: &str, name: &str) -> Result<()> {
        crate::work::validate_veil_id(div)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mode: String = tx.query_row(
            "SELECT storage_mode FROM space_info WHERE id=?1",
            [space],
            |r| r.get(0),
        )?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT space_id FROM space_div WHERE div_id=?1",
                [div],
                |r| r.get(0),
            )
            .optional()?;
        if existing.as_deref().is_some_and(|s| s != space) {
            return Err(invalid("容器已登记到其他空间；请使用移动流程"));
        }
        if mode == "dedicated"
            && tx.query_row(
                "SELECT count(*) FROM space_div WHERE space_id=?1 AND div_id<>?2",
                params![space, div],
                |r| r.get::<_, i64>(0),
            )? > 0
        {
            return Err(invalid("专属空间只能登记一个容器"));
        }
        tx.execute("INSERT INTO space_div VALUES (?1,?2,?3,0,?4,?4) ON CONFLICT(div_id) DO UPDATE SET cached_name=excluded.cached_name,updated_at=excluded.updated_at",params![div,space,name,now()])?;
        tx.commit()?;
        Ok(())
    }
    pub fn unregister(&self, div: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM space_div WHERE div_id=?1", [div])?;
        Ok(())
    }
    pub(crate) fn cache_container_identity(&self, identity: &str, created_at: &str) -> Result<()> {
        let value = serde_json::json!({"created_at": created_at, "availability": "available"});
        validate_record("div_state", identity, &value)?;
        let changed = self.conn.execute("INSERT INTO local_record VALUES ('div_state',?1,?2,?3) ON CONFLICT(record_type,record_key) DO UPDATE SET value_json=excluded.value_json,updated_at=excluded.updated_at WHERE json_type(local_record.value_json,'$.movement') IS NULL", params![identity,json(&value)?,now()])?;
        if changed != 1 {
            return Err(invalid("容器存在待恢复的移动，不能用展示缓存覆盖移动意图"));
        }
        Ok(())
    }
    pub(crate) fn observe_unavailable(&self, identity: &str) -> Result<()> {
        if let Some(value) = self.record::<serde_json::Value>("div_state", identity)? {
            validate_record("div_state", identity, &value)?;
            // 只更新观察值，不能将旧快照中的其他字段覆盖回当前记录。
            self.conn.execute("UPDATE local_record SET value_json=json_set(value_json,'$.availability','unavailable'),updated_at=?1 WHERE record_type='div_state' AND record_key=?2",params![now(),identity])?;
        }
        Ok(())
    }
    pub fn user_setting(&self, key: &str) -> Result<serde_json::Value> {
        let default = match key {
            "hint_level" => "full",
            "layout" => "grid",
            "view_mode" => "type",
            _ => return Err(invalid("未知用户设置")),
        };
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT value_json FROM user_setting WHERE key=?1",
                [key],
                |r| r.get(0),
            )
            .optional()?;
        let value = value
            .map(|s| parse(&s))
            .transpose()?
            .unwrap_or(serde_json::Value::String(default.into()));
        validate_setting(key, &value)?;
        Ok(value)
    }
    pub fn set_user_setting(&self, key: &str, value: &serde_json::Value) -> Result<()> {
        validate_setting(key, value)?;
        self.conn.execute("INSERT INTO user_setting VALUES (?1,?2,?3) ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json,updated_at=excluded.updated_at",params![key,json(value)?,now()])?;
        Ok(())
    }
    pub fn system<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT value_json FROM system_setting WHERE key=?1",
                [key],
                |r| r.get(0),
            )
            .optional()?;
        value.map(|s| parse(&s)).transpose()
    }
    pub fn record<T: DeserializeOwned>(&self, kind: &str, key: &str) -> Result<Option<T>> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT value_json FROM local_record WHERE record_type=?1 AND record_key=?2",
                params![kind, key],
                |r| r.get(0),
            )
            .optional()?;
        value.map(|s| parse(&s)).transpose()
    }
    pub fn set_record<T: Serialize>(&self, kind: &str, key: &str, value: &T) -> Result<()> {
        if !matches!(
            kind,
            "recent_div" | "link_cache" | "space_access" | "hint_state" | "div_state"
        ) {
            return Err(invalid("未知本机记录类型"));
        }
        validate_record(kind, key, value)?;
        self.conn.execute("INSERT INTO local_record VALUES (?1,?2,?3,?4) ON CONFLICT(record_type,record_key) DO UPDATE SET value_json=excluded.value_json,updated_at=excluded.updated_at",params![kind,key,json(value)?,now()])?;
        Ok(())
    }
    pub fn records<T: DeserializeOwned>(&self, kind: &str, limit: u32) -> Result<Vec<T>> {
        let mut stmt=self.conn.prepare("SELECT value_json FROM local_record WHERE record_type=?1 ORDER BY updated_at DESC,record_key LIMIT ?2")?;
        stmt.query_map(params![kind, limit], |r| r.get::<_, String>(0))?
            .map(|s| parse(&s?))
            .collect()
    }
    pub fn mark_hint(&mut self, key: &str) -> Result<bool> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value:Option<String>=tx.query_row("SELECT value_json FROM local_record WHERE record_type='hint_state' AND record_key=?1",[key],|r|r.get(0)).optional()?;
        if let Some(value) = value {
            let _: bool = parse(&value)?;
            tx.commit()?;
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO local_record VALUES ('hint_state',?1,'true',?2)",
            params![key, now()],
        )?;
        tx.commit()?;
        Ok(true)
    }
}
fn validate_setting(key: &str, value: &serde_json::Value) -> Result<()> {
    let allowed: &[&str] = match key {
        "hint_level" => &["full", "brief", "off"],
        "layout" => &["grid", "list"],
        "view_mode" => &["type", "path"],
        _ => return Err(invalid("未知用户设置")),
    };
    if !value.as_str().is_some_and(|s| allowed.contains(&s)) {
        return Err(invalid("用户设置值无效"));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DivState {
    created_at: String,
    availability: Option<String>,
    movement: Option<Movement>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Movement {
    source: PathBuf,
    destination: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryAccess {
    reference: String,
}
fn validate_record<T: Serialize>(kind: &str, key: &str, value: &T) -> Result<()> {
    let text = json(value)?;
    match kind {
        "recent_div" => {
            let r: RecentContainer = parse(&text)?;
            if r.id != key || r.name.trim().is_empty() || !Path::new(&r.path).is_absolute() {
                return Err(invalid("最近记录无效"));
            }
        }
        "link_cache" => {
            let r: crate::config::LinkRecord = parse(&text)?;
            r.link.validate()?;
            if !r.link_path.is_absolute() {
                return Err(invalid("链接缓存路径无效"));
            }
        }
        "hint_state" => {
            let _: bool = parse(&text)?;
        }
        "div_state" => {
            let r: DivState = parse(&text)?;
            if r.availability
                .as_deref()
                .is_some_and(|s| !matches!(s, "available" | "unavailable" | "unknown"))
            {
                return Err(invalid("容器可用状态无效"));
            }
            chrono::DateTime::parse_from_rfc3339(&r.created_at)
                .map_err(|_| invalid("容器身份缓存时间无效"))?;
            if let Some(m) = r.movement {
                if !m.source.is_absolute() || !m.destination.is_absolute() {
                    return Err(invalid("容器移动意图路径无效"));
                }
            }
        }
        "space_access" => {
            let r: DirectoryAccess = parse(&text)?;
            if r.reference.is_empty() {
                return Err(invalid("目录访问引用为空"));
            }
        }
        _ => return Err(invalid("未知本机记录类型")),
    }
    Ok(())
}
