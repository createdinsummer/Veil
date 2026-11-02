//! SQLCipher 容器业务表，不维护目录树内存缓存。
use super::{id, invalid, json, migrate, now, parse};
use crate::error::{Result, VeilError};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use veil_core::metadata::{FileEntry, MetaData};
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone, Serialize, Deserialize)]
pub struct DataKey {
    pub key_id: String,
    pub key_material: String,
    pub status: String,
    pub created_at: i64,
}
impl std::fmt::Debug for DataKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataKey")
            .field("key_id", &self.key_id)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}
impl Drop for DataKey {
    fn drop(&mut self) {
        self.key_material.zeroize();
    }
}
impl DataKey {
    pub fn generate() -> Self {
        let mut bytes = Zeroizing::new([0; 32]);
        getrandom::getrandom(bytes.as_mut()).expect("无法生成文件密钥");
        Self {
            key_id: id(),
            key_material: hex::encode(*bytes),
            status: "active".into(),
            created_at: now(),
        }
    }
    pub fn material(&self) -> Result<Zeroizing<[u8; 32]>> {
        let bytes = Zeroizing::new(
            hex::decode(&self.key_material).map_err(|_| invalid("文件密钥编码无效"))?,
        );
        Ok(Zeroizing::new(
            bytes
                .as_slice()
                .try_into()
                .map_err(|_| invalid("文件密钥长度无效"))?,
        ))
    }
}
pub fn validate_keys(keys: &[DataKey]) -> Result<()> {
    if keys.is_empty()
        || keys.len() > 2
        || keys.iter().filter(|k| k.status == "active").count() != 1
        || keys
            .iter()
            .any(|k| !matches!(k.status.as_str(), "active" | "retiring"))
    {
        return Err(invalid("文件密钥轮换状态无效"));
    }
    if keys.len() == 2
        && (keys[0].key_id == keys[1].key_id || keys[0].key_material == keys[1].key_material)
    {
        return Err(invalid("文件密钥重复"));
    }
    for key in keys {
        if !veil_core::metadata::is_uuid_v4(&key.key_id) {
            return Err(invalid("文件密钥 ID 无效"));
        }
        if key
            .key_material
            .bytes()
            .any(|byte| byte.is_ascii_uppercase())
        {
            return Err(invalid("文件密钥必须使用统一的小写十六进制编码"));
        }
        key.material()?;
    }
    Ok(())
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub kind: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub cipher_name: Option<String>,
    pub size: Option<u64>,
    pub key_id: Option<String>,
    pub nonce: Option<Vec<u8>>,
    pub encrypted_at: Option<i64>,
    pub version_history: Option<Vec<String>>,
}
impl Node {
    pub fn versions(&self) -> Result<&[String]> {
        self.version_history
            .as_deref()
            .ok_or_else(|| invalid("节点不是文件"))
    }
    pub fn nonce(&self) -> Result<[u8; 12]> {
        self.nonce
            .as_deref()
            .ok_or_else(|| invalid("缺少 nonce"))?
            .try_into()
            .map_err(|_| invalid("nonce 长度无效"))
    }
    pub fn cipher(&self) -> Result<&str> {
        let name = self
            .cipher_name
            .as_deref()
            .ok_or_else(|| invalid("节点不是文件"))?;
        validate_cipher(name)?;
        Ok(name)
    }
    pub fn entry(&self, path: String) -> Result<FileEntry> {
        Ok(FileEntry {
            file_id: self.id.clone(),
            version_history: self.versions()?.to_vec(),
            encrypted_name: self.cipher()?.into(),
            original_name: path,
            size: self.size.ok_or_else(|| invalid("文件大小缺失"))?,
            nonce: self.nonce()?,
            encrypted_at: timestamp(self.encrypted_at.ok_or_else(|| invalid("加密时间缺失"))?)?,
            hash: None,
        })
    }
}
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Node> {
    let versions: Option<String> = r.get(12)?;
    Ok(Node {
        id: r.get(0)?,
        parent_id: r.get(1)?,
        name: r.get(2)?,
        kind: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
        cipher_name: r.get(6)?,
        size: r
            .get::<_, Option<i64>>(7)?
            .map(|s| {
                u64::try_from(s).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        7,
                        rusqlite::types::Type::Integer,
                        Box::new(e),
                    )
                })
            })
            .transpose()?,
        key_id: r.get(8)?,
        nonce: r.get(9)?,
        encrypted_at: r.get(10)?,
        version_history: versions
            .map(|v| {
                serde_json::from_str(&v).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        12,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })
            })
            .transpose()?,
    })
}
// 显式列顺序让 JOIN / 递归查询共用同一映射；保留第 11 列用于路径。
const COLUMNS: &str = "id,parent_id,name,kind,created_at,updated_at,cipher_name,size,key_id,nonce,encrypted_at,NULL,version_history";
pub struct ContainerDb {
    pub(crate) conn: Connection,
    pub directory: PathBuf,
}
impl ContainerDb {
    pub fn create(directory: &Path, identity: &str, name: &str, password: &str) -> Result<Self> {
        crate::work::validate_veil_id(identity)?;
        if name.trim().is_empty() {
            return Err(invalid("容器名称不能为空"));
        }
        crate::fsutil::create_dir_all_durable(directory)?;
        let path = directory.join(veil_core::database::CONTAINER_DB);
        // 原子占位，禁止覆盖已有数据库或接受旧格式。
        if directory.join(".veil-info").try_exists()? {
            return Err(invalid("不支持旧容器格式"));
        }
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        drop(file);
        let mut conn = veil_core::database::open(&path, Some(password), true)?;
        migrate(&mut conn, 0x56444956, include_str!("container.sql"), true)?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO div_info VALUES(1,?1,?2,?3,1,?4)",
            params![identity, name, now(), json(&vec![DataKey::generate()])?],
        )?;
        tx.execute("INSERT INTO file_info(id,parent_id,name,kind,created_at,updated_at) VALUES(?1,NULL,'','directory',?2,?2)",params![id(),now()])?;
        tx.commit()?;
        crate::fsutil::sync_directory(directory)?;
        Ok(Self {
            conn,
            directory: directory.into(),
        })
    }
    pub fn open(directory: &Path, password: &str, expected: Option<&str>) -> Result<Self> {
        Self::open_with_timeout(
            directory,
            password,
            expected,
            veil_core::database::BUSY_TIMEOUT,
        )
    }
    pub(crate) fn open_with_timeout(
        directory: &Path,
        password: &str,
        expected: Option<&str>,
        timeout: std::time::Duration,
    ) -> Result<Self> {
        let mut conn = veil_core::database::open_with_timeout(
            &directory.join(veil_core::database::CONTAINER_DB),
            Some(password),
            false,
            timeout,
        )?;
        migrate(&mut conn, 0x56444956, include_str!("container.sql"), false)?;
        let db = Self {
            conn,
            directory: directory.into(),
        };
        let (identity, _, _, version) = db.info()?;
        crate::work::validate_veil_id(&identity)?;
        if version != 1 {
            return Err(invalid("不支持的容器业务结构版本"));
        }
        if expected.is_some_and(|e| e != identity) {
            return Err(VeilError::VeilIdConflict("容器实际身份与预期不一致".into()));
        }
        db.keys()?;
        let count: i64 = db.conn.query_row(
            "SELECT count(*) FROM file_info WHERE parent_id IS NULL AND kind='directory'",
            [],
            |r| r.get(0),
        )?;
        if count != 1 {
            return Err(invalid("容器根目录缺失"));
        }
        Ok(db)
    }
    pub fn info(&self) -> Result<(String, String, i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT veil_id,name,created_at,schema_version FROM div_info WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?)
    }
    pub fn keys(&self) -> Result<Vec<DataKey>> {
        keys(&self.conn)
    }
    pub fn root(&self) -> Result<Node> {
        Ok(self.conn.query_row(
            &format!("SELECT {COLUMNS} FROM file_info WHERE parent_id IS NULL"),
            [],
            row,
        )?)
    }
    pub fn node(&self, identity: &str) -> Result<Option<Node>> {
        node(&self.conn, identity)
    }
    pub fn resolve(&self, path: &str) -> Result<Option<Node>> {
        resolve(&self.conn, path)
    }
    pub fn children(&self, parent: &str, offset: u32, limit: u32) -> Result<Vec<Node>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM file_info WHERE parent_id=?1 ORDER BY name LIMIT ?2 OFFSET ?3"
        ))?;
        Ok(stmt
            .query_map(params![parent, limit, offset], row)?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub fn subtree(&self, identity: &str) -> Result<Vec<Node>> {
        subtree(&self.conn, identity)
    }
    pub fn metadata(&self) -> Result<MetaData> {
        let (identity, name, created, _) = self.info()?;
        let keys = self.keys()?;
        let mut stmt=self.conn.prepare("WITH RECURSIVE tree(id,path) AS (SELECT id,'' FROM file_info WHERE parent_id IS NULL UNION ALL SELECT f.id,CASE WHEN t.path='' THEN f.name ELSE t.path||'/'||f.name END FROM file_info f JOIN tree t ON f.parent_id=t.id) SELECT id,path FROM tree ORDER BY path")?;
        let paths = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let count: i64 = self
            .conn
            .query_row("SELECT count(*) FROM file_info", [], |r| r.get(0))?;
        if paths.len() as i64 != count {
            return Err(invalid("目录树存在孤立节点或环"));
        }
        let mut files = Vec::new();
        let mut directories = Vec::new();
        for (identity, path) in paths {
            let n = self
                .node(&identity)?
                .ok_or_else(|| invalid("目录树变化，请重试"))?;
            if n.kind == "file" {
                if !keys.iter().any(|k| Some(&k.key_id) == n.key_id.as_ref()) {
                    return Err(invalid("文件引用未知密钥"));
                }
                files.push(n.entry(path)?);
            } else if !path.is_empty() {
                directories.push(veil_core::metadata::DirectoryEntry {
                    id: n.id.clone(),
                    path,
                });
            }
        }
        let mut ordered: Vec<_> = keys.iter().collect();
        ordered.sort_by_key(|k| k.status != "active");
        Ok(MetaData {
            veil_id: identity,
            veil_name: name,
            created_at: timestamp(created)?,
            files,
            directories,
            data_keys: ordered
                .into_iter()
                .map(|k| k.material().map(|m| *m))
                .collect::<Result<_>>()?,
        })
    }
}
pub(crate) fn keys(conn: &Connection) -> Result<Vec<DataKey>> {
    let text: String = conn.query_row("SELECT data_keys FROM div_info WHERE id=1", [], |r| {
        r.get(0)
    })?;
    let keys: Vec<DataKey> = parse(&text)?;
    validate_keys(&keys)?;
    Ok(keys)
}
pub(crate) fn node(conn: &Connection, identity: &str) -> Result<Option<Node>> {
    let n = conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM file_info WHERE id=?1"),
            [identity],
            row,
        )
        .optional()?;
    if let Some(ref n) = n {
        validate_node(n)?;
    }
    Ok(n)
}
pub(crate) fn resolve(conn: &Connection, path: &str) -> Result<Option<Node>> {
    let mut current: Node = conn.query_row(
        &format!("SELECT {COLUMNS} FROM file_info WHERE parent_id IS NULL"),
        [],
        row,
    )?;
    if path.is_empty() || path == "." || path == "/" {
        return Ok(Some(current));
    }
    let path = crate::veil_ops::normalize_veil_dir(path)?;
    for name in path.split('/') {
        let next = conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM file_info WHERE parent_id=?1 AND name=?2"),
                params![current.id, name],
                row,
            )
            .optional()?;
        match next {
            Some(n) => current = n,
            None => return Ok(None),
        }
    }
    validate_node(&current)?;
    Ok(Some(current))
}
pub(crate) fn ensure_directory(conn: &Connection, path: &str) -> Result<Node> {
    let mut current = resolve(conn, "")?.ok_or_else(|| invalid("根目录缺失"))?;
    if path.is_empty() {
        return Ok(current);
    }
    let mut prefix = String::new();
    for name in path.split('/') {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(name);
        match resolve(conn, &prefix)? {
            Some(n) if n.kind == "directory" => current = n,
            Some(_) => return Err(invalid("父路径是文件")),
            None => {
                let identity = id();
                conn.execute("INSERT INTO file_info(id,parent_id,name,kind,created_at,updated_at) VALUES(?1,?2,?3,'directory',?4,?4)",params![identity,current.id,name,now()])?;
                current = node(conn, &identity)?.unwrap();
            }
        }
    }
    Ok(current)
}
pub(crate) fn subtree(conn: &Connection, identity: &str) -> Result<Vec<Node>> {
    let mut stmt=conn.prepare(&format!("WITH RECURSIVE tree(x) AS (SELECT id FROM file_info WHERE id=?1 UNION SELECT f.id FROM file_info f JOIN tree t ON f.parent_id=t.x) SELECT {COLUMNS} FROM file_info WHERE id IN (SELECT x FROM tree) ORDER BY id"))?;
    Ok(stmt
        .query_map([identity], row)?
        .collect::<rusqlite::Result<_>>()?)
}
pub(crate) fn validate_node(node: &Node) -> Result<()> {
    if node.id.is_empty() || !matches!(node.kind.as_str(), "file" | "directory") {
        return Err(invalid("节点身份或类型无效"));
    }
    if node.parent_id.is_none() {
        if node.kind != "directory" || !node.name.is_empty() {
            return Err(invalid("根目录字段无效"));
        }
    } else if node.name.is_empty()
        || matches!(node.name.as_str(), "." | "..")
        || node.name.contains(['/', '\\', '\0'])
    {
        return Err(invalid("节点名称无效"));
    }
    if node.kind == "file" {
        if node.size.is_none()
            || node.encrypted_at.is_none()
            || !node
                .key_id
                .as_deref()
                .is_some_and(veil_core::metadata::is_uuid_v4)
        {
            return Err(invalid("文件专用字段缺失或无效"));
        }
        veil_core::version::compare_version_history(node.versions()?, node.versions()?)?;
        node.cipher()?;
        node.nonce()?;
    } else if node.cipher_name.is_some()
        || node.size.is_some()
        || node.key_id.is_some()
        || node.nonce.is_some()
        || node.encrypted_at.is_some()
        || node.version_history.is_some()
    {
        return Err(invalid("目录包含文件专用字段"));
    }
    Ok(())
}
pub fn validate_cipher(name: &str) -> Result<()> {
    let base = name
        .strip_suffix(".enc")
        .ok_or_else(|| invalid("密文名称无效"))?;
    let mut segments = base.split('_');
    if !segments.next().is_some_and(veil_core::metadata::is_uuid_v4)
        || !segments.next().is_some_and(veil_core::metadata::is_uuid_v4)
        || segments.next().is_some()
    {
        return Err(invalid("密文名称无效"));
    }
    Ok(())
}
pub(crate) fn timestamp(millis: i64) -> Result<String> {
    chrono::DateTime::from_timestamp_millis(millis)
        .map(|t| t.to_rfc3339())
        .ok_or_else(|| invalid("时间戳超出范围"))
}
