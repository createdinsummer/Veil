//! SQLCipher 容器业务表，不维护目录树内存缓存。
//!
//! 本模块是加密容器库 veil.db（SQLCipher 整库加密）的数据访问层，管理
//! 此处主要访问三张内容业务表（operation_locks 的协议由 locks 模块维护）：
//! - div_info：容器身份（veil_id、权威名称、创建时间、结构版本）与文件
//!   数据密钥集合（data_keys，双密钥轮换状态）；
//! - file_info：目录树与文件信息（父子关系、名称、类型、当前密文引用、
//!   明文大小、密钥引用、nonce、加密时间、内容版本链）；
//! - file_hash：文件明文内容的 BLAKE3 指纹（展示用缓存）。
//! 本访问对象不持久缓存目录树，完整路径按父子关系查询生成；导入任务可另在 DirectoryCache 缓存锁覆盖的已提交目录。权威信息
//! 只存在数据库里。文件内容（密文）留在磁盘上独立文件，不进数据库。
use super::{container_upgrade, id, invalid, json, now, parse};
use crate::error::{Result, VeilError};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use veil_core::metadata::{FileEntry, MetaData};
use zeroize::{Zeroize, Zeroizing};

/// 文件数据密钥：一条密钥记录对应轮换中的一把密钥。
///
/// 正常情况下只有一把 active；完整改密期间允许一把 active＋一把 retiring，
/// 最多两把。密钥材料以统一的小写十六进制编码存入 div_info.data_keys，
/// 整体由 SQLCipher 整库加密保护，绝不能复制到本机应用库或日志。
#[derive(Clone, Serialize, Deserialize)]
pub struct DataKey {
    /// 密钥稳定 ID（UUID）。
    pub key_id: String,
    /// 密钥材料（32 字节，小写十六进制编码）。
    pub key_material: String,
    /// 轮换状态：active（当前新写入使用）或 retiring（待移除的旧密钥）。
    pub status: String,
    /// 密钥生成时间（UTC 毫秒）。
    pub created_at: i64,
}
impl std::fmt::Debug for DataKey {
    /// 功能：为排查密钥轮换问题提供不含密钥材料的调试输出。
    /// 入参：self 为 DataKey；f 为标准库调试格式化输出器。
    /// 返回值：成功写出 key_id、status 后返回 Ok(())。
    /// 异常：输出端失败返回 fmt::Error。
    /// 注意事项：有意省略 key_material；该保证只覆盖此 Debug 实现，不覆盖 Serialize 或手动打印字段。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 只输出 ID 与状态，绝不在 Debug 里泄露密钥材料。
        f.debug_struct("DataKey")
            .field("key_id", &self.key_id)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}
impl Drop for DataKey {
    /// 功能：释放一条文件数据密钥记录时清零当前对象持有的十六进制密钥材料。
    /// 入参：self: &mut DataKey，即将析构的密钥记录。
    /// 返回值：无；key_material 的当前字符串存储由 zeroize 清零。
    /// 异常：不返回错误。
    /// 注意事项：Clone、序列化或其他临时缓冲区中的副本有各自生命周期，不会随本对象一并清零。
    fn drop(&mut self) {
        // 结构体析构时把密钥材料字符串清零，减少驻留内存的敏感数据。
        self.key_material.zeroize();
    }
}
impl DataKey {
    /// 生成一把新的活动密钥：32 字节随机材料，统一十六进制编码。
    /// 入参：无。
    /// 返回值：新的 DataKey，包含随机 key_id、32 字节材料的十六进制文本、active 状态与当前创建时间。
    /// 异常：系统随机数生成失败时 expect 触发 panic。
    /// 注意事项：只生成内存对象，不自动写入 div_info；调用方负责校验轮换集合并事务提交。
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
    /// 解出 32 字节原始密钥材料（Zeroizing 保证离开作用域后清零）。
    /// 入参：self: &DataKey，包含 key_material 十六进制文本的记录。
    /// 返回值：Ok(`Zeroizing<[u8; 32]>`) 为解码后受析构清零保护的原始文件密钥。
    /// 异常：十六进制无效或解码长度不是 32 字节时返回 InvalidFormat。
    /// 注意事项：此方法不检查 key_id、状态或大小写规范，完整记录校验应使用 validate_keys。
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
/// 校验密钥集合的轮换状态：1～2 把、恰好一把 active、ID/材料不重复、格式合法。
/// 入参：keys: &[DataKey] 为容器的完整文件密钥列表，每项代表一把 active 或 retiring 密钥。
/// 返回值：恰有一把 active、最多两把且全部格式合法时返回 Ok(())。
/// 异常：空集合、超量、状态非法、重复 ID/材料、UUID 非法或材料编码/长度异常时返回 InvalidFormat。
/// 注意事项：逐条解码只用于校验，不改写数据，也不负责确认数据库内当前活动密钥是否已变化。
pub fn validate_keys(keys: &[DataKey]) -> Result<()> {
    // 轮换不变量：非空、最多两把、active 恰好一把、状态只能是两种。
    if keys.is_empty()
        || keys.len() > 2
        || keys.iter().filter(|k| k.status == "active").count() != 1
        || keys
            .iter()
            .any(|k| !matches!(k.status.as_str(), "active" | "retiring"))
    {
        return Err(invalid("文件密钥轮换状态无效"));
    }
    // 双密钥状态：ID 与材料都不能相同。
    if keys.len() == 2
        && (keys[0].key_id == keys[1].key_id || keys[0].key_material == keys[1].key_material)
    {
        return Err(invalid("文件密钥重复"));
    }
    for key in keys {
        // 密钥 ID 必须是合法 UUID。
        if !veil_core::metadata::is_uuid_v4(&key.key_id) {
            return Err(invalid("文件密钥 ID 无效"));
        }
        // 统一小写十六进制编码：大写出现说明编码不统一。
        if key
            .key_material
            .bytes()
            .any(|byte| byte.is_ascii_uppercase())
        {
            return Err(invalid("文件密钥必须使用统一的小写十六进制编码"));
        }
        // 材料必须能解出 32 字节。
        key.material()?;
    }
    Ok(())
}
/// 目录树节点：file_info 表的一行，可以是目录或文件。
///
/// 完整路径不存数据库，沿 parent_id 递归组合；节点的文件专用字段
/// （cipher_name/size/key_id/nonce/encrypted_at/version_history）只在
/// kind=file 时存在，目录节点这些字段必须为空。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Node {
    /// 节点稳定 ID（UUID）：改名、移动、重新加密都不变。
    pub id: String,
    /// 父目录节点 ID；只有根目录为 None。
    pub parent_id: Option<String>,
    /// 当前名称；根节点允许空字符串，其余节点非空。
    pub name: String,
    /// 节点类型：directory 或 file。
    pub kind: String,
    /// 节点创建时间（UTC 毫秒）。
    pub created_at: i64,
    /// 名称/父关系或内容最后修改时间（UTC 毫秒）。
    pub updated_at: i64,
    /// 当前密文文件名（如“锁UUID_随机UUID.enc”）；目录为 None，内容更新或重加密可更换引用。
    pub cipher_name: Option<String>,
    /// 明文大小（字节，非负）；目录为 None。
    pub size: Option<u64>,
    /// 引用的文件密钥 ID（对应 div_info.data_keys 里的密钥项）。
    pub key_id: Option<String>,
    /// 当前密文的 12 字节基础 nonce；目录为 None。
    pub nonce: Option<Vec<u8>>,
    /// 当前密文生成时间（UTC 毫秒）；目录为 None。
    pub encrypted_at: Option<i64>,
    /// 内容版本链（有序 UUID 列表）；目录为 None。
    pub version_history: Option<Vec<String>>,
}
impl Node {
    /// 功能：从文件节点取出其声明的内容版本链，供比较和展示使用。
    /// 入参：self: &Node，为调用方持有的节点快照。
    /// 返回值：有 version_history 时 Ok(&[String]) 借用该列表，字段缺失时失败。
    /// 异常：字段为 None 返回 InvalidFormat“节点不是文件”。
    /// 注意事项：这里只看字段是否存在，不单独检查 kind、链非空或 UUID 合法性；完整检查走 validate_node。
    pub fn versions(&self) -> Result<&[String]> {
        self.version_history
            .as_deref()
            .ok_or_else(|| invalid("节点不是文件"))
    }
    /// 功能：为文件分块解密提供固定长度的基础 nonce。
    /// 入参：self: &Node，从可选 nonce 字节向量中读取。
    /// 返回值：Ok(`[u8; 12]`)，为恰好 12 字节的独立副本。
    /// 异常：字段缺失或长度不为 12 返回 InvalidFormat。
    /// 注意事项：不证明随机性、是否曾重复使用或与当前密文是否对应；这些关联来自上层持久化版本协议。
    pub fn nonce(&self) -> Result<[u8; 12]> {
        self.nonce
            .as_deref()
            .ok_or_else(|| invalid("缺少 nonce"))?
            .try_into()
            .map_err(|_| invalid("nonce 长度无效"))
    }
    /// 功能：取得并验证一份文件当前引用的密文名称，避免把任意路径当作容器密文位置。
    /// 入参：self: &Node，为保存可选 cipher_name 的节点。
    /// 返回值：Ok(&str)，借用满足两个小写 UUID v4 加 .enc 规则的名称。
    /// 异常：字段缺失或 validate_cipher 检查失败返回 InvalidFormat。
    /// 注意事项：不读磁盘、不检查文件存在或认证；名称对应当前密文，跨重加密/内容更新可变化。
    pub fn cipher(&self) -> Result<&str> {
        let name = self
            .cipher_name
            .as_deref()
            .ok_or_else(|| invalid("节点不是文件"))?;
        validate_cipher(name)?;
        Ok(name)
    }
    /// 功能：把一份节点数据组装成客户端文件条目，附上从父链计算的展示路径。
    /// 入参：self: &Node 为文件记录；path: String 为调用方已计算的容器内完整相对路径，按值保存。
    /// 返回值：Ok(FileEntry)，包含稳定 ID、版本链、密文名、大小、nonce、RFC 3339 加密时间，hash 初始为 None。
    /// 异常：版本/密文/大小/nonce/时间字段缺失、格式不合规或时间越界返回 InvalidFormat。
    /// 注意事项：不自行递归计算 path，不读取哈希或物理文件；versions 仅取字段，若需整节点语义检查应先 validate_node。
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
/// 功能：将明确列布局的 SQL 查询结果转成文件树节点，统一普通、递归及联表查询的字段映射。
/// 入参：r: &rusqlite::Row 为按 COLUMNS 布局返回的行，索引 11 为保留位置，索引 12 为版本历史 JSON。
/// 返回值：Ok(Node)，size 从可空有符号整数转换为非负 u64，版本 JSON 转为可选字符串列表。
/// 异常：缺列、列类型不匹配、负大小或版本 JSON 无法解码返回 rusqlite 错误。
/// 注意事项：只做字段解码，不调用 validate_node；根、分页和子树等调用是否进一步验证需按各自入口约定判断。
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
        // size 存 INTEGER 且约定非负，读到负数按转换失败处理。
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
        // 版本链以 JSON 文本存储，读出时反序列化。
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
// 第 11 列恒为 NULL：递归查询会在该位置放入组合出的完整路径，普通查询
// 则填空，两种查询共用同一个 row 映射函数。
const COLUMNS: &str = "id,parent_id,name,kind,created_at,updated_at,cipher_name,size,key_id,nonce,encrypted_at,NULL,version_history";
/// 加密容器内容访问对象：conn 持有已解锁的池化 SQLCipher 连接，directory 定位数据库及独立密文文件。
/// 提供身份、密钥、节点、分页和快照查询；不自行缓存整树，资源租约与跨步骤事务边界由调用方编排。
pub struct ContainerDb {
    /// 已解锁的 SQLCipher 连接（密码由 core 的原生接口设置）。
    pub(crate) conn: veil_core::database::Connection,
    /// 容器目录（veil.db 与密文文件所在目录）。
    pub directory: PathBuf,
}
impl ContainerDb {
    /// 功能：初始化一份新的加密容器数据库、身份数据和根目录，建立后续文件树操作的起点。
    /// 入参：directory: &Path 为容器目录，可已存在但不能已有 veil.db 或旧 .veil-info；identity/name/password: &str 分别为安全单段容器 ID、非空展示名和非空数据库口令。
    /// 返回值：Ok(ContainerDb)，含已解锁连接；首把活动文件密钥与唯一根目录已在同一业务事务提交。
    /// 异常：身份/名称/旧格式、已存在库、目录/权限、口令/结构、随机密钥、SQL 或同步失败传播 VeilError，随机源失败可能 panic。
    /// 注意事项：不会要求目录整体不存在，也不清理其他既有文件；目录和空数据库占位先创建，后续失败不自动撤销它们。
    /// 业务身份/根提交与先前建表、最终目录同步不是一个可整体回滚的事务，返回 Err 时可能已经存在完整可打开容器。
    pub fn create(directory: &Path, identity: &str, name: &str, password: &str) -> Result<Self> {
        crate::work::validate_veil_id(identity)?;
        if name.trim().is_empty() {
            return Err(invalid("容器名称不能为空"));
        }
        crate::fsutil::create_dir_all_durable(directory)?;
        let path = directory.join(veil_core::database::CONTAINER_DB);
        // 原子占位，禁止覆盖已有数据库或接受旧格式。
        // 旧容器格式（.veil-info 元数据文件）不再支持，明确拒绝。
        if directory.join(".veil-info").try_exists()? {
            return Err(invalid("不支持旧容器格式"));
        }
        // create_new：veil.db 已存在则直接失败，不覆盖别人的数据。
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
        // 用密码打开新库并建表（写入结构版本 1）。
        let mut conn = veil_core::database::open(&path, Some(password), true)?;
        container_upgrade::check_and_upgrade(&mut conn, true)?;
        // 某些文件系统在空文件首次写入后改变文件标识；刷新连接并重新校验结构。
        drop(conn);
        let mut conn = veil_core::database::open(&path, Some(password), false)?;
        container_upgrade::check_and_upgrade(&mut conn, false)?;
        // 单事务写入容器身份与根目录：要么都有、要么都没有。
        let tx = conn.transaction()?;
        // div_info 固定只有一行（id=1）：身份、名称、创建时间、结构版本、
        // 首把活动文件密钥集合。
        tx.execute(
            "INSERT INTO div_info VALUES(1,?1,?2,?3,1,?4)",
            params![identity, name, now(), json(&vec![DataKey::generate()])?],
        )?;
        // 根目录节点：parent_id 为 NULL、名称为空字符串（唯一允许空名的节点）。
        tx.execute("INSERT INTO file_info(id,parent_id,name,kind,created_at,updated_at) VALUES(?1,NULL,'','directory',?2,?2)",params![id(),now()])?;
        tx.commit()?;
        // 请求同步新目录项；实际断电持久性仍取决于平台、文件系统与设备。
        crate::fsutil::sync_directory(directory)?;
        Ok(Self {
            conn,
            directory: directory.into(),
        })
    }
    /// 功能：打开已存在的容器库并核对口令、当前结构、身份、密钥集合及根目录数量。
    /// 入参：directory: &Path 为容器目录；password: &str 为口令；expected: `Option<&str>` 为调用方预期稳定 ID，None 不做比对。
    /// 返回值：Ok(ContainerDb)，使用默认数据库忙等待策略取得的池化加密连接。
    /// 异常：库缺失/空、解锁或结构不支持、身份不符、密钥/根数据非法及 I/O/SQL 失败传播 VeilError。
    /// 注意事项：只是打开及基础完整性检查，不申请文件租约、不扫描每个节点或密文标签，不能当作全容器内容认证。
    pub fn open(directory: &Path, password: &str, expected: Option<&str>) -> Result<Self> {
        Self::open_with_timeout(
            directory,
            password,
            expected,
            veil_core::database::BUSY_TIMEOUT,
        )
    }
    /// 功能：在调用方给定数据库等待策略下解锁并验证容器，用于短轮询等锁等流程。
    /// 入参：directory: &Path 为容器根；password: &str 为口令；expected: `Option<&str>` 为需精确匹配的容器 ID；timeout: Duration 为数据库/池相关等待参数。
    /// 返回值：Ok(ContainerDb)，结构版本、身份、密钥轮换集合与唯一目录根数量已通过检查。
    /// 异常：数据库访问/密码/结构、身份冲突、密钥集合或根数量检查失败返回 VeilError。
    /// 注意事项：timeout 不是整个函数总耗时硬上限；各项分次读取，不自动提供调用期间的全树稳定快照或资源租约。
    pub(crate) fn open_with_timeout(
        directory: &Path,
        password: &str,
        expected: Option<&str>,
        timeout: std::time::Duration,
    ) -> Result<Self> {
        // 密码由 SQLCipher 原生接口设置，随后读表实际验证解锁；错误密码
        // 不会生成明文替代库。
        let mut conn = veil_core::database::open_with_timeout(
            &directory.join(veil_core::database::CONTAINER_DB),
            Some(password),
            false,
            timeout,
        )?;
        // 验证结构版本（解锁之后才读业务表）。
        container_upgrade::check_and_upgrade(&mut conn, false)?;
        let db = Self {
            conn,
            directory: directory.into(),
        };
        // 读容器身份（解锁后核对真实 veil_id）。
        let (identity, _, _, version) = db.info()?;
        crate::work::validate_veil_id(&identity)?;
        if version != 1 {
            return Err(invalid("不支持的容器业务结构版本"));
        }
        // 调用方预期身份与实际不符：报告冲突，不能随便用一个副本。
        if expected.is_some_and(|e| e != identity) {
            return Err(VeilError::VeilIdConflict("容器实际身份与预期不一致".into()));
        }
        // 校验密钥集合轮换状态。
        db.keys()?;
        // 根目录必须恰好一个：缺失或重复都视为数据异常。
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
    /// 功能：读取加密库中的容器权威身份和展示信息，供本机登记缓存及包恢复核对。
    /// 入参：self: &ContainerDb，连接应已完成解锁。
    /// 返回值：Ok((String,String,i64,i64))，依次为 veil_id、name、UTC 毫秒创建时间和业务 schema_version，来自 div_info 的 id=1 行。
    /// 异常：该行不存在、字段转换或 SQL 失败传播 VeilError。
    /// 注意事项：不再次校验名称、时间语法或身份路径规则；不返回本机空间归属或口令。
    pub fn info(&self) -> Result<(String, String, i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT veil_id,name,created_at,schema_version FROM div_info WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?)
    }
    /// 功能：读取并校验容器当前完整文件密钥集合，供内容读写和轮换流程选钥。
    /// 入参：self: &ContainerDb，为已解锁容器连接。
    /// 返回值：Ok(`Vec<DataKey>`)，含 1～2 把状态合法且唯一的文件密钥记录，顺序沿数据库 JSON，不在此按 active 排序。
    /// 异常：缺少 div_info、JSON/状态/ID/材料校验或 SQL 失败传播 VeilError。
    /// 注意事项：记录含可解码的明文密钥材料，DataKey 析构只清零其自有字符串；不更新轮换状态或清理其他序列化副本。
    pub fn keys(&self) -> Result<Vec<DataKey>> {
        keys(&self.conn)
    }
    /// 功能：取得文件树的顶层节点，作为相对路径解析起点。
    /// 入参：self: &ContainerDb。
    /// 返回值：Ok(Node)，来自 parent_id IS NULL 的记录，正常结构中应为唯一目录根。
    /// 异常：无记录、查询或字段解码失败传播 VeilError。
    /// 注意事项：该入口只 row 解码，不额外调用 validate_node；唯一性和目录形状主要由结构约束及 open 的基础检查保证。
    pub fn root(&self) -> Result<Node> {
        Ok(self.conn.query_row(
            &format!("SELECT {COLUMNS} FROM file_info WHERE parent_id IS NULL"),
            [],
            row,
        )?)
    }
    /// 功能：用稳定身份读取一个节点并核对其自身字段规则，供文件操作定位使用。
    /// 入参：self: &ContainerDb；identity: &str 为文件或目录节点 ID。
    /// 返回值：找到并校验成功为 Ok(Some(Node))，没有该 ID 为 Ok(None)。
    /// 异常：SQL、字段解码或节点校验失败传播 VeilError。
    /// 注意事项：委托同文件的 node 连接工具，不核验父链或磁盘内容，也不凭读取结果自动建立访问租约。
    pub fn node(&self, identity: &str) -> Result<Option<Node>> {
        node(&self.conn, identity)
    }
    /// 功能：按用户可见相对路径定位容器节点，供查看、导入/导出和目录操作使用。
    /// 入参：self: &ContainerDb；path: &str，空串、.、/ 表示根，其他按 normalize_veil_dir 规则解释。
    /// 返回值：路径各段均找到则 Ok(Some(Node))，某层未找到为 Ok(None)。
    /// 异常：根查询、路径格式、逐层 SQL/解码或最终节点校验失败传播 VeilError。
    /// 注意事项：委托连接级 resolve，不自行开快照事务或申请锁；与目录结构变更并发时调用方须提供稳定读取边界。
    pub fn resolve(&self, path: &str) -> Result<Option<Node>> {
        resolve(&self.conn, path)
    }
    /// 功能：按父节点分页列出直接子项，供客户端浏览目录使用。
    /// 入参：self: &ContainerDb；parent: &str 为父节点 ID；offset: u32 为跳过条数；limit: u32 为最多返回条数，0 返回空集合。
    /// 返回值：Ok(`Vec<Node>`)，每项是一个直接子节点，按 name 升序；没有匹配可为空。
    /// 异常：查询、字段解码或版本 JSON 读取失败传播 VeilError。
    /// 注意事项：不验证父节点是否真实目录，也不逐项调用 validate_node；跨页稳定性须由调用方事务/租约保证，并发变化会影响 OFFSET 结果。
    pub fn children(&self, parent: &str, offset: u32, limit: u32) -> Result<Vec<Node>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM file_info WHERE parent_id=?1 ORDER BY name LIMIT ?2 OFFSET ?3"
        ))?;
        Ok(stmt
            .query_map(params![parent, limit, offset], row)?
            .collect::<rusqlite::Result<_>>()?)
    }
    /// 功能：以名称游标逐页遍历直接子项，避免反复扫描较大的 OFFSET。
    /// 入参：self: &ContainerDb；parent: &str 为目录节点 ID；after: `Option<&str>` 为前页末名称，None 从头；limit: u32 为页大小。
    /// 返回值：Ok(`Vec<Node>`)，按 name 升序取 name>after 的至多 limit 项，0 或无匹配为空。
    /// 异常：SQL 或 row 字段/JSON 解码失败传播 VeilError。
    /// 注意事项：入口不自行取得目录读锁，也不校验每个节点；调用方须在稳定目录范围租约/事务下使用，名称精确按数据库比较规则解释。
    pub(crate) fn children_after(
        &self,
        parent: &str,
        after: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Node>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM file_info WHERE parent_id=?1 AND (?2 IS NULL OR name>?2) ORDER BY name LIMIT ?3"
        ))?;
        Ok(stmt
            .query_map(params![parent, after, limit], row)?
            .collect::<rusqlite::Result<_>>()?)
    }
    /// 功能：按稳定节点身份取得自身及全部后代，供删除、移动或范围计算使用。
    /// 入参：self: &ContainerDb；identity: &str 为起点节点 ID，可指文件或目录。
    /// 返回值：Ok(`Vec<Node>`)，按节点 ID 升序；起点不存在时为空列表。
    /// 异常：递归查询或字段解码失败传播 VeilError。
    /// 注意事项：委托连接工具，不调用逐节点完整校验，也不锁定结果，实际修改需上层写事务和范围租约协调。
    pub fn subtree(&self, identity: &str) -> Result<Vec<Node>> {
        subtree(&self.conn, identity)
    }
    /// 功能：把数据库里的容器身份、树关系、密钥和内容摘要缓存组合成客户端/备份可用的完整快照。
    /// 入参：self: &ContainerDb，调用方应保护读取期间的业务状态不被不兼容写入改变。
    /// 返回值：Ok(MetaData)，文件和非根目录按展示路径组装，文件可含缓存哈希；原始文件密钥按 active 在前放入 data_keys。
    /// 异常：树遍历数量不符、节点消失、未知密钥引用、节点字段/时间/密钥非法或任一 SQL 失败传播 VeilError。
    /// 注意事项：本函数没有自开一致读事务；递归路径查询、数量检查和逐节点读取分步进行，调用方需提供读租约或事务边界。
    /// 会把整份清单和解码后的文件密钥放入内存，不读取所有密文认证；返回模型含敏感密钥，应避免日志或明文持久化。
    pub fn metadata(&self) -> Result<MetaData> {
        let (identity, name, created, _) = self.info()?;
        let keys = self.keys()?;
        // 递归 CTE：从根出发沿父子链组合完整路径（根路径为空字符串）。
        let mut stmt=self.conn.prepare("WITH RECURSIVE tree(id,path) AS (SELECT id,'' FROM file_info WHERE parent_id IS NULL UNION ALL SELECT f.id,CASE WHEN t.path='' THEN f.name ELSE t.path||'/'||f.name END FROM file_info f JOIN tree t ON f.parent_id=t.id) SELECT id,path FROM tree ORDER BY path")?;
        let paths = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // 递归结果必须覆盖全部节点：数量不符说明有环或孤立节点。
        let count: i64 = self
            .conn
            .query_row("SELECT count(*) FROM file_info", [], |r| r.get(0))?;
        if paths.len() as i64 != count {
            return Err(invalid("目录树存在孤立节点或环"));
        }
        let mut files = Vec::new();
        let mut directories = Vec::new();
        // 逐节点组装条目（按路径排序后的稳定顺序）。
        for (identity, path) in paths {
            let n = self
                .node(&identity)?
                .ok_or_else(|| invalid("目录树变化，请重试"))?;
            if n.kind == "file" {
                // 文件引用的密钥必须在密钥集合里，否则数据异常。
                if !keys.iter().any(|k| Some(&k.key_id) == n.key_id.as_ref()) {
                    return Err(invalid("文件引用未知密钥"));
                }
                let mut entry = n.entry(path)?;
                // 顺带读取明文内容的 BLAKE3 指纹（展示用缓存，可缺失）。
                entry.hash = self
                    .conn
                    .query_row(
                        "SELECT blake3 FROM file_hash WHERE file_id=?1",
                        [&n.id],
                        |row| row.get(0),
                    )
                    .optional()?;
                files.push(entry);
            } else if !path.is_empty() {
                // 根目录（空路径）不进目录清单，其余目录按路径列出。
                directories.push(veil_core::metadata::DirectoryEntry {
                    id: n.id.clone(),
                    path,
                });
            }
        }
        // 元数据槽位 0 表示当前写入密钥，因此把 active 排在前；这只改变返回快照顺序，不改数据库轮换状态。
        let mut ordered: Vec<_> = keys.iter().collect();
        ordered.sort_by_key(|k| k.status != "active");
        Ok(MetaData {
            veil_id: identity,
            veil_name: name,
            created_at: timestamp(created)?,
            files,
            directories,
            // 把密钥材料解成原始字节随元数据返回（调用方负责清零）。
            data_keys: ordered
                .into_iter()
                .map(|k| k.material().map(|m| *m))
                .collect::<Result<_>>()?,
        })
    }
}
/// 功能：为使用原生事务连接的调用方读取并验证 div_info.data_keys。
/// 入参：conn: &Connection 为已解锁连接或调用方事务借用。
/// 返回值：Ok(`Vec<DataKey>`)，为 id=1 行的完整 JSON 密钥记录，经过 validate_keys。
/// 异常：查询、JSON 解码、密钥数量/状态/ID/材料规则失败传播 VeilError。
/// 注意事项：不自动申请资源租约或固定后续查询快照；返回含敏感材料，JSON 临时文本也有自己的内存生命周期。
pub(crate) fn keys(conn: &Connection) -> Result<Vec<DataKey>> {
    let text: String = conn.query_row("SELECT data_keys FROM div_info WHERE id=1", [], |r| {
        r.get(0)
    })?;
    let keys: Vec<DataKey> = parse(&text)?;
    validate_keys(&keys)?;
    Ok(keys)
}
/// 功能：在调用方连接/事务中按 ID 查询并验证一条文件树节点。
/// 入参：conn: &Connection 为已解锁连接或事务；identity: &str 为精确匹配 ID。
/// 返回值：无记录为 Ok(None)，有记录并通过 validate_node 为 Ok(Some(Node))。
/// 异常：SQL、列/JSON 转换或字段语义非法传播 VeilError。
/// 注意事项：不把不存在当作错误，也不查完整父关系或密文标签；后续修改需同事务或租约边界防止读后变化。
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
/// 功能：在已知节点 ID 集合上批量读取和校验记录，减少逐条查询用于提交或比较的开销。
/// 入参：conn: &Connection 为调用方连接/事务；identities: &[String] 为节点 ID 列表，每项代表一个文件或目录，允许重复/空列表。
/// 返回值：Ok(`HashMap<String,Node>`)，键为找到的节点 ID，值为校验通过的节点；缺失 ID 不生成占位，重复 ID 合并。
/// 异常：任一 SQL、字段读取或 validate_node 失败传播 VeilError。
/// 注意事项：每 128 个 ID 分批执行，函数不自行开启事务；跨批一致性必须由调用方事务/租约保证，不以“批量”本身保证快照。
pub(crate) fn nodes_batch(
    conn: &Connection,
    identities: &[String],
) -> Result<std::collections::HashMap<String, Node>> {
    // 结果映射：键是节点 ID，值是校验后的节点。
    let mut nodes = std::collections::HashMap::new();
    for batch in identities.chunks(128) {
        // 生成与批大小相同的占位符列表（如 ?,?,?）。
        let placeholders = vec!["?"; batch.len()].join(",");
        let mut query = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM file_info WHERE id IN ({placeholders})"
        ))?;
        let rows = query.query_map(rusqlite::params_from_iter(batch), row)?;
        for node in rows {
            let node = node?;
            validate_node(&node)?;
            nodes.insert(node.id.clone(), node);
        }
    }
    Ok(nodes)
}

/// 功能：从根沿相对名称逐级查询节点，统一容器路径解析和缺失判断。
/// 入参：conn: &Connection 为已解锁连接/事务；path: &str 允许空串、.、/ 特指根，其他须满足规范相对路径规则。
/// 返回值：完整匹配为 Ok(Some(Node))，中途找不到下一段为 Ok(None)。
/// 异常：根缺失、路径非法、SQL/字段解码或最终 validate_node 失败传播 VeilError。
/// 注意事项：先查根再解释路径，根特殊分支直接返回 row 解码结果，不额外 validate_node；普通路径只在最后校验节点。
/// 不自行包事务，查询期间父关系可能改变；规范化会去掉 . 和末尾斜杠，但拒绝 ..、空分段及非法分隔，不是消解任意路径穿越。
pub(crate) fn resolve(conn: &Connection, path: &str) -> Result<Option<Node>> {
    // 起点是根目录。
    let mut current: Node = conn.query_row(
        &format!("SELECT {COLUMNS} FROM file_info WHERE parent_id IS NULL"),
        [],
        row,
    )?;
    // 空路径/`.`/`/` 都指根目录本身。
    if path.is_empty() || path == "." || path == "/" {
        return Ok(Some(current));
    }
    // 规范化仅去掉 . 和末尾斜杠等允许表示；..、内部空分段、绝对形式或反斜杠会被拒绝，不会被悄悄折叠。
    let path = crate::veil_ops::normalize_veil_dir(path)?;
    // 逐级查找：任何一级找不到即返回 None（路径不存在）。
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
/// 功能：一次解析多个容器路径，返回精确节点或最深已匹配祖先，支持冲突核对和缺失目录处理。
/// 入参：conn: &Connection 为调用方连接/事务；paths: &[String] 为已规范化相对路径列表，根应传空字符串。
/// 返回值：Ok(`HashMap<String,(Node,Option<String>,bool)>`)，键为原请求文本，元组依次为节点、可空 BLAKE3 缓存、是否完整匹配；空输入为空映射。
/// 异常：SQL、字段解码或节点验证失败传播 VeilError；超过 128 条递归分批再合并结果。
/// 注意事项：不调用 normalize_veil_dir；请求含特殊段或非规范分隔时按原文本逐段匹配，根缺失也可能使请求没有结果。
/// 不自动开事务，多个批次和后续写入的一致性由调用方保证；exact=false 时不能把返回祖先当作请求本身已存在。
pub(crate) fn resolve_batch(
    conn: &Connection,
    paths: &[String],
) -> Result<std::collections::HashMap<String, (Node, Option<String>, bool)>> {
    // 超过 128 条分批递归处理，控制单语句参数数量。
    if paths.len() > 128 {
        let mut nodes = std::collections::HashMap::new();
        for batch in paths.chunks(128) {
            nodes.extend(resolve_batch(conn, batch)?);
        }
        return Ok(nodes);
    }
    // 空请求直接返回空结果。
    if paths.is_empty() {
        return Ok(Default::default());
    }
    let values = (0..paths.len())
        .map(|_| "(?)")
        .collect::<Vec<_>>()
        .join(",");
    let columns = COLUMNS
        .split(',')
        .map(|column| {
            if column == "NULL" {
                "NULL".to_owned()
            } else {
                format!("n.{column}")
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("WITH RECURSIVE requested(path) AS (VALUES {values}),
        walk(path,rest,id) AS (
          SELECT path,path||'/',id FROM requested CROSS JOIN file_info WHERE parent_id IS NULL
          UNION ALL
          SELECT w.path,substr(w.rest,instr(w.rest,'/')+1),n.id FROM walk w
          JOIN file_info n ON n.parent_id=w.id AND n.name=substr(w.rest,1,instr(w.rest,'/')-1)
          WHERE w.rest<>''
        )
        SELECT {columns},w.path,h.blake3,w.rest='' FROM walk w JOIN file_info n ON n.id=w.id
        LEFT JOIN file_hash h ON h.file_id=n.id
        WHERE w.rest='' OR NOT EXISTS (SELECT 1 FROM file_info c WHERE c.parent_id=w.id AND c.name=substr(w.rest,1,instr(w.rest,'/')-1))");
    let mut query = conn.prepare(&sql)?;
    let rows = query.query_map(rusqlite::params_from_iter(paths), |r| {
        Ok((
            r.get::<_, String>(13)?,
            (
                row(r)?,
                r.get::<_, Option<String>>(14)?,
                r.get::<_, bool>(15)?,
            ),
        ))
    })?;
    // result 按原请求路径关联解析结论；一个元组同时保留最深节点、摘要及精确标志，供调用方区分存在目标与仅存在父级。
    let mut result = std::collections::HashMap::new();
    for row in rows {
        let (path, (node, hash, exact)) = row?;
        validate_node(&node)?;
        result.insert(path, (node, hash, exact));
    }
    Ok(result)
}

/// 功能：在调用方写事务中沿相对路径创建缺少的目录节点，供导入准备父级使用。
/// 入参：conn: &Connection 为已有写事务/连接；path: &str 应为已规范化目录路径，空字符串表示根。
/// 返回值：Ok(Node)，为最终目录，所有新节点 parent_id 接到前一层，类型 directory，创建/修改时间取当前毫秒。
/// 异常：根缺失、文件充当父级、路径解析或 SQL 失败返回 VeilError；插入后节点意外读不到时 unwrap panic。
/// 注意事项：不自行开事务、申请目录租约或提交，调用方须保护同一资源范围并提供可回滚写边界；不创建磁盘上的明文目录。
pub(crate) fn ensure_directory(conn: &Connection, path: &str) -> Result<Node> {
    let mut current = resolve(conn, "")?.ok_or_else(|| invalid("根目录缺失"))?;
    if path.is_empty() {
        return Ok(current);
    }
    // prefix 累积已经处理到的目录路径，每轮代表从根到当前层的完整业务路径，缺失时才插入一个节点。
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
/// 功能：在导入任务的可信目录缓存帮助下补齐数据库父目录，避免重复查询整条父链。
/// 入参：conn: &Connection 为调用方写事务；path: &str 为规范化相对目录；known: &`HashMap<String,Node>` 只含已提交且锁范围覆盖的目录；
/// pending: &mut `HashMap<String,Node>` 保存当前事务新增/确认的目录，键均为逻辑目录路径，根为零长度字符串。
/// 返回值：Ok(Node)，优先复制 pending/known 命中，否则递归确保父级、查询或插入目录，并存入 pending。
/// 异常：根或新节点缺失、父路径为文件、节点非法、SQL 或递归处理失败传播 VeilError。
/// 注意事项：缓存命中不重查数据库或 kind；调用者须保证缓存来源和租约有效。只有确认事务提交后才能把 pending 合入共享 known，回滚时丢弃。
pub(crate) fn ensure_directory_cached(
    conn: &Connection,
    path: &str,
    known: &std::collections::HashMap<String, Node>,
    pending: &mut std::collections::HashMap<String, Node>,
) -> Result<Node> {
    // 先查当前事务内确认项，再查已提交项，避免把回滚产生的节点提前共享给其他工作线程。
    if let Some(node) = pending.get(path).or_else(|| known.get(path)) {
        return Ok(node.clone());
    }
    let node = if path.is_empty() {
        resolve(conn, "")?.ok_or_else(|| invalid("根目录缺失"))?
    } else {
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        let parent = ensure_directory_cached(conn, parent, known, pending)?;
        match conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM file_info WHERE parent_id=?1 AND name=?2"),
                params![parent.id, name],
                row,
            )
            .optional()?
        {
            Some(node) if node.kind == "directory" => {
                validate_node(&node)?;
                node
            }
            Some(_) => return Err(invalid("父路径是文件")),
            None => {
                let identity = id();
                conn.execute("INSERT INTO file_info(id,parent_id,name,kind,created_at,updated_at) VALUES(?1,?2,?3,'directory',?4,?4)",params![identity,parent.id,name,now()])?;
                node(conn, &identity)?.ok_or_else(|| invalid("新建目录缺失"))?
            }
        }
    };
    pending.insert(path.to_owned(), node.clone());
    Ok(node)
}
/// 功能：用递归 CTE 收集一个节点及其后代的数据库记录，避免应用层逐层查子项。
/// 入参：conn: &Connection 为调用方连接/事务；identity: &str 为起点 ID。
/// 返回值：Ok(`Vec<Node>`)，递归 ID 集合去重后按 ID 排序读取，起点不存在为空。
/// 异常：SQL 或 row 的字段/JSON 转换失败传播 VeilError。
/// 注意事项：递归 UNION 对 ID 去重，不将树重复路径无限加入；只做字段解码，目录树合法性及访问锁由其他入口负责。
pub(crate) fn subtree(conn: &Connection, identity: &str) -> Result<Vec<Node>> {
    let mut stmt=conn.prepare(&format!("WITH RECURSIVE tree(x) AS (SELECT id FROM file_info WHERE id=?1 UNION SELECT f.id FROM file_info f JOIN tree t ON f.parent_id=t.x) SELECT {COLUMNS} FROM file_info WHERE id IN (SELECT x FROM tree) ORDER BY id"))?;
    Ok(stmt
        .query_map([identity], row)?
        .collect::<rusqlite::Result<_>>()?)
}
/// 功能：检查节点自身字段组合符合根/目录/文件规则，拒绝不可解释的树和加密引用数据。
/// 入参：node: &Node，为从数据库读出或待解释的单条节点记录。
/// 返回值：节点 ID 非空、类型正确、根形状或普通名称合法，文件专用字段/版本链/密文名/nonce 合规则 Ok(())。
/// 异常：任一形状或格式不合规返回 InvalidFormat，版本链检查错误向外传播。
/// 注意事项：只检查本节点，不查询父目录是否存在、目录树是否成环或 key_id 真在密钥集合中；节点 ID 只查非空，不强制 UUID 格式。
/// 目录必须不带文件专用字段；文件版本链借比较自身触发完整合法性检查，不据链长判断新旧。
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
/// 功能：限定容器文件引用只能指向符合新命名约定的单个密文文件。
/// 入参：name: &str 为数据库 cipher_name，要求“规范小写 UUID v4_规范小写 UUID v4.enc”。
/// 返回值：两段、下划线和后缀均满足规则时 Ok(())。
/// 异常：后缀、UUID、段数不符返回 InvalidFormat。
/// 注意事项：不验证第一段确为已有租约或第二段全库唯一，不检查磁盘存在及内容认证；唯一引用另受表约束保护。
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
/// 功能：把数据库毫秒时间戳转为供模型和界面展示的 UTC 时间文本。
/// 入参：millis: i64，为 Unix epoch 起算的毫秒数，可为表示 epoch 之前的负值。
/// 返回值：Ok(String)，Chrono 生成的 RFC 3339 UTC 表示。
/// 异常：超出 Chrono 支持的日期范围返回 InvalidFormat。
/// 注意事项：不读取当前时钟、不改变原记录，只做数值时间表示转换。
pub(crate) fn timestamp(millis: i64) -> Result<String> {
    chrono::DateTime::from_timestamp_millis(millis)
        .map(|t| t.to_rfc3339())
        .ok_or_else(|| invalid("时间戳超出范围"))
}
