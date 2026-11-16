//! service 独占业务表访问；连接与磁盘加密格式由 core 统一提供。
//!
//! 本模块是 service 的数据层入口，管理两份数据库的业务访问：
//! - app（本机应用库 app.db）：空间登记、用户/系统设置与展示缓存，明文
//!   SQLite，绝不能存放容器密码或文件密钥；
//! - container（加密容器库 veil.db）：目录树、内容版本链、文件密钥与租约
//!   表，由 SQLCipher 整库加密；
//! - locks（文件级租约）：operation_locks 表的申请/续租/释放/交接与冲突判定；
//! - container_upgrade：容器库结构版本检查与升级的唯一入口。
//! 连接初始化、SQLCipher 参数与磁盘格式全部在 veil-core 的 database 模块
//! 集中管理，本模块负责业务 SQL 与流程编排，并提供 now/id/json/parse、结构指纹校验及
//! canonical_path 等共享辅助函数；本文件不声明业务类，具体数据类型位于各子模块。
pub mod app;
pub mod container;
mod container_upgrade;
pub mod locks;
use crate::error::{Result, VeilError};
use rusqlite::{Connection, TransactionBehavior};
use serde::{Serialize, de::DeserializeOwned};

/// 当前时间（UTC 毫秒）；测试特性下可注入时钟偏移模拟时间跳变。
/// 入参：无。
/// 返回值：i64 类型的 UTC Unix 毫秒数；fault-injection 构建额外叠加测试偏移。
/// 异常：不返回错误；不合理测试偏移可能导致整数加法溢出。
/// 注意事项：这是可跳变的系统时钟，不是测量等待时长所用的单调时钟。
pub(crate) fn now() -> i64 {
    let time = chrono::Utc::now().timestamp_millis();
    // 故障注入：把测试偏移叠加到真实时间上，用于验证租约与提交校验的时钟防御。
    #[cfg(feature = "fault-injection")]
    return time + TEST_CLOCK_OFFSET.load(std::sync::atomic::Ordering::SeqCst);
    #[cfg(not(feature = "fault-injection"))]
    time
}
/// 生成新的随机身份 ID（UUID v4 字符串）。
/// 入参：无。
/// 返回值：String 类型的随机 UUID v4，用作容器业务记录身份。
/// 异常：此入口没有 Result 返回；随机 ID 的生成依赖底层实现。
/// 注意事项：只生成标识，不登记数据库记录；唯一性仍应由数据库约束兜底。
pub(crate) fn id() -> String {
    veil_core::metadata::generate_version_id()
}
/// 把业务模型序列化为数据库 JSON 字段的文本。
/// 入参：value: &T 为实现 Serialize 的业务对象，可包括列表、映射或结构体。
/// 返回值：Ok(String) 为紧凑 JSON 文本，各字段结构由传入类型的序列化定义决定。
/// 异常：无法序列化时返回 SerializationError，例如 JSON 不支持的映射键。
/// 注意事项：不执行脱敏，含密钥材料的文本只能写入允许存储该数据的加密位置。
pub(crate) fn json<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| VeilError::SerializationError(e.to_string()))
}
/// 把数据库 JSON 文本解析回业务模型；非法 JSON 明确报错，绝不当默认值。
/// 入参：value: &str 为数据库 JSON 文本；T 指定需要还原的业务类型。
/// 返回值：Ok(T) 为通过 JSON 语法和目标类型反序列化检查的对象。
/// 异常：语法、字段类型或目标类型校验失败返回 InvalidFormat。
/// 注意事项：反序列化成功不自动代表满足全部业务规则；仍需对应领域校验。
pub(crate) fn parse<T: DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value)
        .map_err(|e| VeilError::InvalidFormat(format!("数据库 JSON 无效: {e}")))
}
/// 构造「数据格式无效」错误，作为本模块统一的数据校验失败出口。
/// 入参：message: &str 为描述数据格式问题的说明。
/// 返回值：VeilError::InvalidFormat，复制并持有说明文本。
/// 异常：构造错误对象本身不返回 Result。
/// 注意事项：不会自动写日志；说明不要直接拼接密码或密钥等敏感内容。
pub(crate) fn invalid(message: &str) -> VeilError {
    VeilError::InvalidFormat(message.into())
}

/// 只创建当前结构或验证当前结构，不自动迁移旧版数据库。
///
/// 入参 conn 是目标数据库连接；application_id 是数据库类型标识
/// （app 库 0x56415050，容器库 0x56444956）；schema 是目标结构的建表
/// SQL；target_version 是当前支持的结构版本；create 表示库文件是否由本
/// 次调用新建（决定是否允许执行建表）。
///
/// 策略：版本与标识都匹配 → 校验结构指纹；全新库 → 建表并写版本；其他
/// 情况（版本不符、非空未知库）一律拒绝，不自动迁移、不按字段缺失补表。
/// 返回值：版本与标识匹配且结构验证通过，或空白库初始化事务提交成功时返回 Ok(())。
/// 异常：未知类型/版本/非空未知库/结构指纹不匹配返回 InvalidFormat；SQL 失败传播数据库错误。
/// 注意事项：create: bool 是允许初始化的授权条件，不仅仅是文件存在性；不会覆盖未知库。
/// 事务内二次读取发现其他进程已完成同版本初始化时直接提交，不在该分支重复执行指纹比对。
pub(crate) fn ensure_schema(
    conn: &mut Connection,
    application_id: i64,
    schema: &str,
    target_version: i64,
    create: bool,
) -> Result<()> {
    // 先读当前版本与类型标识，决定走「验证」还是「初始化」分支。
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let actual: i64 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    if version == target_version && actual == application_id {
        // 版本标识都对：仍需校验实际结构（防缺表/改表/未知触发器）。
        verify_schema(conn, application_id, schema)?;
        return Ok(());
    }
    // 需要变更时开立即事务：把「检查版本 + 建表 + 写版本」原子完成，
    // 防止两个首次启动进程同时初始化产生半套结构。
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // 事务内重读：上面读到的状态可能已过期。
    let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let actual: i64 = tx.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    if version == 0 && create {
        // 只允许在真正空白的库上初始化：已有表或已写标识都说明是未知库。
        let tables: i64 = tx.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table'",
            [],
            |r| r.get(0),
        )?;
        if tables != 0 || actual != 0 {
            return Err(invalid("拒绝初始化已有的未知数据库"));
        }
        // 执行建表并写入结构版本与类型标识。
        tx.execute_batch(schema)?;
        tx.pragma_update(None, "application_id", application_id)?;
        tx.pragma_update(None, "user_version", target_version)?;
    } else if version != target_version || actual != application_id {
        // 已有数据但版本/标识不符：明确拒绝，不尝试低版本迁移。
        return Err(invalid("数据库结构版本或应用标识不受支持"));
    }
    tx.commit()?;
    Ok(())
}

/// 结构版本相同也不能接受缺表、改表或未知触发器；只缓存固定结构，不缓存业务树。
///
/// 把数据库里所有表/索引/触发器的定义（sqlite_master）序列化后做 BLAKE3
/// 指纹，与「同版本建表 SQL 在内存库上执行出的指纹」比对。指纹模板按
/// 数据库类型各缓存一份，避免每次打开都重建内存库。
/// 入参：conn: &Connection 为目标库；application_id: i64 为 app 或容器类型标识；schema: &str 为声明建表 SQL。
/// 返回值：实际结构指纹与对应模板一致时为 Ok(())，不比较业务行内容。
/// 异常：未知类型或结构不符返回 InvalidFormat；内存库建表、查询、序列化错误向外传播。
/// 注意事项：模板按数据库类型在本进程缓存一次，同类型 schema 不能在运行期间任意替换。
fn verify_schema(conn: &Connection, application_id: i64, schema: &str) -> Result<()> {
    use std::sync::OnceLock;
    // 两份指纹模板缓存：app 库与容器库各一份（进程内只算一次）。
    static APP: OnceLock<String> = OnceLock::new();
    static CONTAINER: OnceLock<String> = OnceLock::new();
    // 计算一个数据库连接的结构指纹（嵌套工具函数，随外层函数使用）。
    /// 功能：把实际表结构转换成指纹，以识别与声明版本不一致的表、索引、视图或触发器。
    /// 入参：conn 为只读查询结构的 rusqlite 连接。
    /// 返回值：返回对象定义序列化后计算的 BLAKE3 十六进制摘要。
    /// 异常：查询或字段读取失败传播数据库错误；序列化失败返回 SerializationError。
    /// 注意事项：定义按 type/name 排序，只统一 CRLF 换行，不忽略 SQL 空白或同义语法差异。
    fn fingerprint(conn: &Connection) -> Result<String> {
        // 读取系统表里全部对象定义：表、索引、视图、触发器。
        let mut query = conn.prepare("SELECT type,name,tbl_name,sql FROM sqlite_master WHERE name NOT GLOB 'sqlite_*' OR type='trigger' ORDER BY type,name")?;
        let definitions = query
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?
                        .map(|sql| sql.replace("\r\n", "\n")),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // 每条定义是 (对象类型, 对象名称, 所属表名称, 可空建表 SQL)；集合按类型和名称排序。
        // 只归一化 CRLF 到 LF，再对整个序列化集合哈希，不忽略其他 SQL 格式差异。
        Ok(blake3::hash(json(&definitions)?.as_bytes())
            .to_hex()
            .to_string())
    }
    // 按 application_id 选择对应类型模板；未知标识直接拒绝。
    let cached = match application_id {
        0x56415050 => &APP,
        0x56444956 => &CONTAINER,
        _ => return Err(invalid("未知数据库类型")),
    };
    if cached.get().is_none() {
        // 首次：在内存库上执行声明的建表 SQL，得到标准指纹并缓存。
        let template = Connection::open_in_memory()?;
        template.execute_batch(schema)?;
        let _ = cached.set(fingerprint(&template)?);
    }
    // 目标库指纹与标准模板不一致：结构被改动过，拒绝使用。
    if cached.get() != Some(&fingerprint(conn)?) {
        return Err(invalid("数据库结构与声明的版本不一致"));
    }
    Ok(())
}

/// 对尚不存在的目标也解析最近的已存在祖先，统一 /var 与 /private/var 等别名。
///
/// 用于比较两个路径是否指向同一位置（如防止同一目录登记成多个空间）：
/// 目标路径可能还没创建，所以从目标向上找最近一个真实存在的祖先做
/// canonicalize（解析符号链接），再把缺失的尾巴原样拼回去。
/// 入参：path: &Path 为待比较位置，可为相对路径或尚不存在的目标。
/// 返回值：Ok(PathBuf) 为已存在祖先解析别名后拼回未存在后缀的绝对路径。
/// 异常：绝对化、祖先解析等失败返回 I/O 错误；无法取得有效路径段返回 InvalidFormat。
/// 注意事项：缺失后缀原样保留，不保证消除其中所有 ..；不是对未来文件系统状态的预留。
pub(crate) fn canonical_path(path: &std::path::Path) -> Result<std::path::PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut ancestor = absolute.as_path();
    // 缺失的路径段倒序暂存，等祖先解析成功后再拼回。
    let mut suffix = Vec::new();
    loop {
        match std::fs::canonicalize(ancestor) {
            Ok(mut resolved) => {
                // 祖先解析成功：把缺失的尾巴按原顺序接上。
                for name in suffix.iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 这一段还不存在：记下名字，继续向上找祖先。
                suffix.push(
                    ancestor
                        .file_name()
                        .ok_or_else(|| invalid("路径无效"))?
                        .to_os_string(),
                );
                ancestor = ancestor.parent().ok_or_else(|| invalid("路径无效"))?;
            }
            // 其他错误（权限等）直接上报。
            Err(e) => return Err(e.into()),
        }
    }
}

// 测试时钟偏移：故障注入特性下由测试设置，模拟系统时间跳变/挂起。
#[cfg(feature = "fault-injection")]
static TEST_CLOCK_OFFSET: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
#[cfg(feature = "fault-injection")]
#[doc(hidden)]
/// 为故障注入测试设置毫秒级虚拟时钟偏移，用于验证租约到期和系统时钟跳变处理。
/// 入参：millis（i64）为相对真实系统时钟的偏移毫秒数；正数使读到的时间更晚，负数使其更早。
/// - Returns: 无返回值；仅在 fault-injection 测试构建中可用，不用于生产时间设置。
/// 异常：不返回错误；调用后其他测试读取 now 会立即受到影响。
/// 注意事项：这是进程共享开关，应避免并行测试互相覆盖，场景结束后需恢复偏移。
pub fn set_test_clock_offset(millis: i64) {
    TEST_CLOCK_OFFSET.store(millis, std::sync::atomic::Ordering::SeqCst);
}
