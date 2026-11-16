//! 空间与容器登记的客户端视图。
//!
//! 所属业务模块：veil-service 的空间（space，本机上存放各容器工作目录的
//! 隔离区域）管理编排层，面向 CLI / 桌面端 / 移动端的展示与操作入口。
//!
//! 文件内核心结构/函数清单：
//! - SpaceView / ContainerSummary：空间及其容器清单的客户端展示视图
//! - list_spaces：读取 app.db 生成全部空间的展示快照
//! - create_space：校验名称与目录后登记新空间
//! - delete_space / delete_space_record：删除空间（物理删除或仅删登记）
//! - delete_container：删除容器登记与整个加密目录
//! - create_container / move_container：通过统一创建流程生成容器；
//!   move_container 是"移动容器"的独立复制语义（新 ID、新副本）
//!
//! 依赖能力：
//! - storage::app：本机应用库 app.db 的空间/登记/设置/本地记录读写
//! - workflow：容器创建的通用计划与提交流程
//! - storage::container / locks：容器库读取与容器级租约
//!
//! 整体业务目标：空间与容器登记都是"本机视图"，权威身份在容器库
//! veil.db 里。本文件协调登记、最近记录、链接缓存与磁盘目录的创建/删除；
//! 单容器删除先提交登记再删文件，空间删除则先删文件再提交登记，失败均可能部分完成。
use crate::{
    config::AppConfig,
    error::VeilError,
    storage::app::AppStore,
    workflow::{self, CreateOptions, CreatedContainer, WorkflowError},
};
use rusqlite::{OptionalExtension, TransactionBehavior};
use serde::Serialize;
/// 空间在客户端界面上的展示视图。
///
/// 业务定位：把 app.db 里 space_info 的记录 + 该空间下的容器登记
/// 组装成界面可直接渲染的快照；只用于展示，不修改任何权威数据。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceView {
    /// 空间稳定 ID（改名/移动目录都不变）。
    pub id: String,
    /// 空间展示名称。
    pub name: String,
    /// 本机绑定的空间目录路径（未绑定为空字符串）。
    pub path: String,
    /// 是否为系统设置里默认空间的引用对象。
    pub is_default: bool,
    /// 绑定路径当前是否为目录；未绑定或缺失为 false，不代表读写权限已测试。
    pub is_available: bool,
    /// 该空间下已登记容器的摘要列表；空间不可用时为空。
    pub containers: Vec<ContainerSummary>,
}
/// 空间内单个已登记容器的展示摘要。
///
/// 业务定位：未解锁时也要能让界面列出容器，所以这里只放
/// 本机缓存的身份与名称，不代表已验证容器内容。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerSummary {
    /// 容器稳定 ID（对应容器库 veil.db 里的 veil_id）。
    pub id: String,
    /// 本机登记缓存的容器展示名称。
    pub name: String,
    /// 按空间目录 + 容器 ID 拼出的容器目录路径。
    pub path: String,
}
/// 组装本机空间及容器登记的展示列表，供客户端在未解锁时展示。
/// 入参：无。
/// 返回值：`Vec<SpaceView>`，每项一个空间；仅空间目录当前存在时填入其容器摘要。
/// 异常：应用库打开、默认空间初始化或查询失败返回 VeilError。
/// 注意事项：default_space 可能创建默认空间和目录；多个查询不是同一个事务快照。is_available 仅为 is_dir 检查，不证明读写权限或每个容器存在。
pub(crate) fn list_spaces() -> Result<Vec<SpaceView>, VeilError> {
    let mut store = AppStore::global()?;
    // default：系统设置里的默认空间 ID，用于给视图打默认标记。
    let default = store.default_space()?.id;
    // registrations：全部容器登记，之后按 space_id 分组挂到各空间下。
    let registrations = store.registrations()?;
    Ok(store
        .spaces()?
        .into_iter()
        .map(|s| {
            // 逐空间组装视图：目录可用才挂容器清单。
            let path = s.local_path.unwrap_or_default();
            // is_available：目录已绑定且当前磁盘上确实存在。
            let is_available = !path.as_os_str().is_empty() && path.is_dir();
            // 该空间下的容器：筛选登记表，拼出容器目录路径。
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

/// 确认创建或复制的目标空间能被唯一选中且当前有目录。
/// 入参：identity（&str）为精确匹配的空间 ID 或名称。
/// 返回值：恰好匹配一个且绑定路径 is_dir 为 true 时返回 Ok(())。
/// 异常：不存在、ID/名称合计多命中、未绑定或目录缺失返回 WorkError；查询错误传播。
/// 注意事项：ID 与名称并列匹配，没有 ID 优先规则；检查不加锁、不保证后续仍可用，也不测试写权限。
pub(crate) fn ensure_space_available(identity: &str) -> Result<(), VeilError> {
    let store = AppStore::global()?;
    // matches：按 ID 或名称过滤出的候选空间。
    let matches = store
        .spaces()?
        .into_iter()
        .filter(|space| space.id == identity || space.name == identity)
        .collect::<Vec<_>>();
    // 必须恰好命中一个；零个是空间不存在，多个是名称不唯一。
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

/// 登记新的本机容器存储区域。
/// 入参：name（&str）须去空白后非空、原长度不超过80字符、无控制字符；path（`Option<&Path>`）为可选绝对目录，None 由存储层创建默认位置。
/// 返回值：空间登记成功返回 Ok(())。
/// 异常：名称无效或重名、路径非绝对/非目录/被占用、文件系统或数据库失败返回 VeilError。
/// 注意事项：名称按原文保存和查重，不自动 trim；已存在路径先 canonicalize 查重，最终路径约束由存储层执行。目录创建和登记并非跨资源原子事务。
pub(crate) fn create_space(name: &str, path: Option<&std::path::Path>) -> Result<(), VeilError> {
    if name.trim().is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(VeilError::WorkError("空间名称无效".into()));
    }
    let mut store = AppStore::global()?;
    let spaces = store.spaces()?;
    // 名称查重：同一空间名称只允许登记一次。
    if spaces.iter().any(|s| s.name == name) {
        return Err(VeilError::WorkError("空间名称已存在".into()));
    }
    if let Some(path) = path {
        // 目录必须绝对路径，否则后续按路径解析容器位置会不可靠。
        if !path.is_absolute() {
            return Err(VeilError::WorkError("空间目录必须是绝对路径".into()));
        }
        if path.try_exists()? {
            // 已存在的路径必须是文件夹。
            if !path.is_dir() {
                return Err(VeilError::WorkError("请选择文件夹作为空间目录".into()));
            }
            // 归一化路径后与其他空间的绑定目录比较，防同一目录重复登记。
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

/// 删除指定空间的整个磁盘目录及范围内空间的本机登记。
/// 入参：identity（&str）为精确空间 ID，不接受名称解析。
/// 返回值：目录删除且登记事务提交成功返回 Ok(())。
/// 异常：空间不可用、危险路径、关联移动记录、锁/数据库/文件系统失败返回 VeilError。
/// 注意事项：目录必须可访问；先递归删除目录，后提交登记。删除失败可能已移除部分文件，提交失败也不能恢复已删除内容。
pub fn delete_space(identity: &str) -> Result<(), VeilError> {
    delete_space_inner(identity, true)
}

/// 移除已绑定但当前不可访问空间的本机登记，保留磁盘数据。
/// 入参：identity（&str）为精确空间 ID。
/// 返回值：目标空间及相关本机缓存清理事务提交后返回 Ok(())。
/// 异常：空间不存在、未绑定、路径非绝对、目录仍可访问、涉及移动或数据库/锁错误时返回错误。
/// 注意事项：不是任意空间的解绑入口；仍要求有已绑定路径，默认空间引用可能被清除。
pub fn delete_space_record(identity: &str) -> Result<(), VeilError> {
    delete_space_inner(identity, false)
}

/// 执行空间删除，并在失败时记录进入的变更阶段。
/// 入参：identity 为空间 ID；delete_files（bool）为 true 删除磁盘，为 false 仅删不可用空间记录。
/// 返回值：原样返回分阶段实现的 `Result<(), VeilError>`。
/// 异常：锁、参数、数据库或文件系统错误原样传播。
/// 注意事项：诊断在内部事务和锁释放后写入；阶段名表示尝试执行到该步骤，不证明该步骤成功。
fn delete_space_inner(identity: &str, delete_files: bool) -> Result<(), VeilError> {
    // phase：已完成的阶段标记，出错时供 diagnose_partial_mutation 记录。
    let mut phase = None;
    let result = delete_space_staged(identity, delete_files, &mut phase);
    diagnose_partial_mutation("delete_space", identity, phase, &result);
    result
}

/// 在空间排他锁和登记写事务下，检查影响范围并执行空间删除。
/// 入参：identity（&str）为 ID；delete_files（bool）选择物理删除或仅删记录；phase（&mut `Option<&'static str>`）记录已进入的变更阶段。
/// 返回值：目标及必要的嵌套空间登记删除提交后返回 Ok(())。
/// 异常：路径无效、空间模式不匹配、危险路径、活动移动引用、磁盘/数据库/锁失败返回错误。
/// 注意事项：物理删除发生在写事务持有期间且先于登记提交；事务回滚不能撤销磁盘变更。嵌套空间按已登记路径的前缀比较，不在此逐个 canonicalize。
fn delete_space_staged(
    identity: &str,
    delete_files: bool,
    phase: &mut Option<&'static str>,
) -> Result<(), VeilError> {
    let mut store = AppStore::global()?;
    // 空间独占锁：整个删除过程排斥并发的容器移动/复制与空间删除。
    let space_lock = store.acquire_space_exclusive_lock(identity)?;
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    // 读取登记的目录路径：不存在则说明空间已被删。
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
    // 物理删除要求目录当前可访问；仅删登记要求目录当前不可访问。
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
    // 安全护栏：拒绝删除 app.db 所在目录或包含它的目录，防止误删全盘。
    let app_directory = std::fs::canonicalize(&store.directory)?;
    if stored_path == app_directory || app_directory.starts_with(&stored_path) {
        return Err(VeilError::WorkError(
            "拒绝删除文件系统根目录或包含 Veil 数据库的目录".into(),
        ));
    }
    // space_ids：受影响的全部空间 ID——物理删除时把路径落在目标目录
    // 内的嵌套空间一并收集；仅删登记时只处理目标空间自己。
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
    // 目标空间必须落在受影响的集合里（登记路径互相包含时兜底）。
    if !space_ids.iter().any(|id| id == identity) {
        return Err(VeilError::WorkError("空间目录登记无效".into()));
    }
    // 正在执行"移动容器"（独立复制）的空间不能删除，避免复制中途丢失登记。
    if space_has_active_movement(&tx, &space_ids, &stored_path)? {
        return Err(VeilError::WorkError(
            "空间中有容器正在移动，不能删除空间".into(),
        ));
    }
    if delete_files {
        // 先删磁盘目录；失败时登记事务未提交，但磁盘可能已删除一部分，无法随 SQL 回滚恢复。
        *phase = Some("remove_files");
        std::fs::remove_dir_all(&stored_path)?;
        // 阶段二：磁盘已删，接下来提交登记事务。
        *phase = Some("commit_registration");
    }
    // 若默认空间引用在被删集合里，先清除默认空间设置（避免悬空引用）。
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
    // 清最近访问与容器状态缓存（按空间下登记的容器）。
    tx.execute(
        "DELETE FROM local_record WHERE record_type IN ('recent_div','div_state') AND record_key IN (SELECT div_id FROM space_div WHERE space_id IN (SELECT value FROM json_each(?1)))",
        [&space_ids_json],
    )?;
    // 清链接缓存（按链接正文里的容器 ID 匹配）。
    tx.execute(
        "DELETE FROM local_record WHERE record_type='link_cache' AND json_extract(value_json,'$.link.veil.veil_id') IN (SELECT div_id FROM space_div WHERE space_id IN (SELECT value FROM json_each(?1)))",
        [&space_ids_json],
    )?;
    // 清目录访问授权记录。
    tx.execute(
        "DELETE FROM local_record WHERE record_type='space_access' AND record_key IN (SELECT value FROM json_each(?1))",
        [&space_ids_json],
    )?;
    // 清容器登记（space_div 只是本机视图，删除不影响容器库本身）。
    tx.execute(
        "DELETE FROM space_div WHERE space_id IN (SELECT value FROM json_each(?1))",
        [&space_ids_json],
    )?;
    // 清空间登记；影响行数为 0 说明空间不存在（并发删除兜底）。
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

/// 检查删除范围是否被本机保存的容器移动意图引用。
/// 入参：tx 为当前 rusqlite::Transaction；space_ids（&[String]）每项为待删空间 ID；root（&Path）为待删目录。
/// 返回值：任一 movement 的源/目标空间 ID 命中集合或路径位于 root 下返回 true，否则 false。
/// 异常：SQL 或记录 JSON 解析失败返回 VeilError。
/// 注意事项：不向执行者探活，也不校验意图是否过期；字段存在但为 null 或缺少匹配字段的记录不会单独使结果为 true。路径比较不做 canonicalize。
fn space_has_active_movement(
    tx: &rusqlite::Transaction<'_>,
    space_ids: &[String],
    root: &std::path::Path,
) -> Result<bool, VeilError> {
    let mut statement = tx.prepare(
        "SELECT value_json FROM local_record WHERE record_type='div_state' AND json_type(value_json,'$.movement') IS NOT NULL",
    )?;
    // states：所有带 movement 字段的容器状态 JSON 原文。
    let states = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for raw in states {
        // 逐条解析移动意图：涉及被删空间 ID 或路径落入被删目录即视为活跃移动。
        let state: serde_json::Value = crate::storage::parse(&raw)?;
        let Some(movement) = state.get("movement") else {
            continue;
        };
        // references_space：移动的源/目标空间 ID 是否在被删集合中。
        let references_space = ["source_space_id", "target_space_id"]
            .into_iter()
            .filter_map(|key| movement.get(key).and_then(serde_json::Value::as_str))
            .any(|id| space_ids.iter().any(|space| space == id));
        // includes_path：移动的源/目标路径是否落在被删目录内。
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

/// 删除已登记容器的整个加密目录及本机登记，不要求密码。
/// 入参：identity（&str）为合法容器 UUID，按精确 ID 定位。
/// 返回值：登记事务、目录删除及父目录同步请求均成功返回 Ok(())。
/// 异常：容器被使用、登记/路径变化、移动字段存在、锁/数据库/文件系统失败返回错误。
/// 注意事项：先提交登记删除，再删目录；后半段失败不恢复登记。阶段日志只说明失败位置，不能替代人工检查残留。
pub fn delete_container(identity: &str) -> Result<(), VeilError> {
    // phase：阶段标记，出错时用于诊断部分完成的删除。
    let mut phase = None;
    let result = delete_container_staged(identity, &mut phase);
    diagnose_partial_mutation("delete_container", identity, phase, &result);
    result
}

/// 取得应用库后执行单个容器的分阶段删除。
/// 入参：identity 为容器 ID；phase 为可变阶段标签输出，供外层诊断。
/// 返回值：原样返回删除实现结果。
/// 异常：应用库初始化或删除流程中的 VeilError 传播。
/// 注意事项：本包装不另建恢复登记，也不重命名容器目录。
fn delete_container_staged(
    identity: &str,
    phase: &mut Option<&'static str>,
) -> Result<(), VeilError> {
    let mut store = AppStore::global()?;
    delete_container_in_store(&mut store, identity, phase)
}

/// 在生命周期锁保护下先清理登记，再移除容器目录。
/// 入参：store（&mut AppStore）为应用库；identity（&str）为容器 ID；phase（&mut `Option<&'static str>`）为正在尝试的变更阶段。
/// 返回值：登记提交、删除目录和父目录同步均成功为 Ok(())。
/// 异常：租约使用冲突、路径/归属无效、移动标记、SQL、目录删除或同步失败返回 VeilError。
/// 注意事项：持有容器删除排他锁及空间共享锁；路径检查不阻止不遵循协议的外部文件替换。连接缓存失效只阻止后续复用，不中止已借出的连接。
fn delete_container_in_store(
    store: &mut AppStore,
    identity: &str,
    phase: &mut Option<&'static str>,
) -> Result<(), VeilError> {
    crate::work::validate_veil_id(identity)?;
    // 容器删除排他锁：正在使用（解锁、导入导出等持有共享生命周期锁）时直接拒绝。
    let _container_lock = store.acquire_container_delete_lock(identity)?;
    // 找登记：容器必须已登记到某个空间。
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
    // container_path：按空间目录 + 容器 ID 解析出的容器目录。
    let container_path = root.join(identity);
    // 必须是真实目录、不能是符号链接（防删除目标被偷换）。
    let metadata = std::fs::symlink_metadata(&container_path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(VeilError::WorkError("容器目录无效".into()));
    }
    // 归一化后再次确认目录仍直接位于空间目录下且名字就是容器 ID。
    let container_path = std::fs::canonicalize(container_path)?;
    if container_path.parent() != Some(root.as_path())
        || container_path.file_name().and_then(|name| name.to_str()) != Some(identity)
    {
        return Err(VeilError::WorkError("容器目录超出空间范围".into()));
    }

    // 排他锁保护整个删除过程；事务只清除 app.db 记录。
    // 阶段一：提交登记删除事务（失败则磁盘完全不动）。
    *phase = Some("commit_registration");
    let tx = store
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    // 事务内复核：容器不在移动中、归属未变化。
    check_container_delete_registration(&tx, identity, &registration.space_id)?;
    // 清容器登记。
    tx.execute("DELETE FROM space_div WHERE div_id=?1", [identity])?;
    // 清最近访问与容器状态缓存。
    tx.execute("DELETE FROM local_record WHERE record_key=?1 AND record_type IN ('recent_div','div_state')", [identity])?;
    // 清链接缓存（按链接正文里的容器 ID 匹配）。
    tx.execute("DELETE FROM local_record WHERE record_type='link_cache' AND json_extract(value_json,'$.link.veil.veil_id')=?1", [identity])?;
    tx.commit()?;

    // 直接删除整个目录，容器 DB 和密文文件一起移除，不改名、不恢复目录。
    // 阶段二：登记已提交；此后目录删除失败也不恢复登记。
    debug_assert!(store.conn.is_autocommit());
    // 使后续连接不再复用旧缓存；已借出的连接不会因此中止，依赖生命周期协议防止并发使用。
    veil_core::database::invalidate(&container_path.join(veil_core::database::CONTAINER_DB));
    *phase = Some("remove_files");
    std::fs::remove_dir_all(&container_path).map_err(|error| {
        // 登记已删但目录没删成：错误里带回原目录路径，供用户手动清理。
        VeilError::WorkError(format!(
            "容器登记已删除，但容器目录删除失败（{}）：{error}",
            container_path.display()
        ))
    })?;
    // 阶段三：请求同步父目录元数据；实际持久性取决于平台和设备，部分平台此操作为空。
    *phase = Some("sync_removed_directory");
    veil_core::fsutil::sync_directory(&root)?;
    Ok(())
}
/// 在删除登记的同一写事务内复核空间归属与移动状态。
/// 入参：conn（&Connection）为调用方写事务连接；identity 为容器 ID；expected_space 为此前读到的空间 ID。
/// 返回值：未带 movement 字段且登记空间仍相同返回 Ok(())。
/// 异常：movement 字段存在（含 JSON null）、登记缺失/变化或 SQL 失败返回错误。
/// 注意事项：仅校验本机登记，不核对目录中的加密身份；调用方负责锁、事务提交及路径校验。
fn check_container_delete_registration(
    conn: &rusqlite::Connection,
    identity: &str,
    expected_space: &str,
) -> Result<(), VeilError> {
    use crate::storage::invalid;
    // moving：该容器是否带移动意图（正在"移动容器"的独立复制流程中）。
    let moving: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_record WHERE record_type='div_state' AND record_key=?1 AND json_type(value_json,'$.movement') IS NOT NULL)",
        [identity], |row| row.get(0),
    )?;
    if moving {
        return Err(invalid("容器正在移动，不能删除"));
    }
    // current_space：事务内重读登记归属，防止等待锁期间归属被改动。
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

/// 在选定空间创建新身份容器并生成本机链接入口。
/// 入参：space（&str）为唯一空间 ID/名称；name（&str）须非空白、不超过100字符、无路径分隔符和控制字符；password（&str）为非空容器密码。
/// 返回值：成功返回 CreatedContainer，含名称、链接路径、容器目录。
/// 异常：名称无效为 InvalidName；空间、密码、链接、初始化或登记失败返回 WorkflowError。
/// 注意事项：链接放在配置目录的 links 下并随机命名；创建跨文件与应用库，初始化成功后的后续失败可能保留容器。
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
    // link：随机命名的 .veil-link 输出路径（links 目录下）。
    let link = AppConfig::config_path()?
        .parent()
        .unwrap()
        .join("links")
        .join(format!("{}.veil-link", crate::storage::id()));
    // 创建计划：目标名称、链接输出、所属空间；由计划生成新容器 ID 并确认目标不存在。
    let plan = workflow::prepare_create(CreateOptions {
        target: name,
        link_output: link.to_str(),
        work_name: Some(space),
        work_root: None,
        dedicated: false,
        portable: false,
    })?;
    // 提交创建：初始化容器库、写密文与链接、登记到空间。
    workflow::commit_create(plan, password)
}

/// 按产品“移动容器”入口语义，在目标空间生成独立身份副本，保留源容器。
/// 入参：identity 为源容器 UUID；target_space 为目标空间唯一 ID/名称；password 为源数据库密码，也用于副本。
/// 返回值：副本创建、验证、登记及两端租约显式释放均成功返回 Ok(())。
/// 异常：源/空间无效、密码/锁/复制/认证/登记/清理失败返回 VeilError；清理失败可能附带残留路径。
/// 注意事项：复制前后核验源读租约；新 ID 不代表新文件密钥，副本保留原文件密钥。成功登记后的释放失败也会使本方法返回错误，此时副本可能已经完整存在。
pub fn move_container(identity: &str, target_space: &str, password: &str) -> Result<(), VeilError> {
    use crate::storage::{
        container::ContainerDb,
        invalid,
        locks::{self, LockManager, LockMode},
    };
    crate::work::validate_veil_id(identity)?;
    let store = AppStore::global()?;
    // registration：源的登记记录（决定源空间与目录位置）。
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
    // 源、目标空间都持共享生命周期锁：复制期间两边空间都不能被删除。
    let _source_space_lock = store.acquire_space_shared_lock(&source_space.id)?;
    let _target_space_lock = store.acquire_space_shared_lock(target_space)?;
    ensure_space_available(target_space)?;
    // source：源容器目录路径（空间目录 + 容器 ID）。
    let source = source_space
        .local_path
        .as_ref()
        .ok_or_else(|| invalid("原空间未绑定"))?
        .join(identity);
    // 源数据库必须是普通文件（非符号链接），复制才能逐字节可靠。
    ensure_regular_container_database(&source)?;
    let source_db = ContainerDb::open(&source, password, Some(identity))?;
    // name：源容器名称，复制后沿用。
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
    // 从此处到复制结束持有源容器共享生命周期锁，与遵守本机协议的删除请求互斥。
    let _source_lifecycle_lock = store.acquire_container_shared_lock(identity)?;
    // 源读租约阻止遵守资源租约协议的业务写入/改密；租约自身仍可能续期，不能当成 OS 文件快照。
    let source_guard = LockManager::new(&source, password).acquire_container(LockMode::Read)?;
    // target_guard：目标容器排他锁；owned_target：本次是否已创建目标。
    let mut target_guard = None;
    let mut owned_target = false;
    let result = workflow::commit_create_with_initializer(
        plan,
        password,
        |directory, id, name, password| {
            // 初始化回调：在创建流程占位的目录内完成"复制 + 校验"。
            owned_target = true;
            // 复制前复核源读租约凭证仍有效。
            for credential in source_guard.credentials()? {
                locks::validate(&source_db.conn, &credential, Some(LockMode::Read))?;
            }
            // 先复制 DB 并修正新身份、清除源锁，再对该目标 DB 加锁。
            copy_container_database(&source_db, directory, id, name, password)?;
            // 目标排他锁：加锁后复制密文树，期间目标不被并发改动。
            let guard = LockManager::new(directory, password).acquire_container(LockMode::Write)?;
            copy_container_tree(&source, directory)?;
            // 认证校验：身份、目录树、引用文件与密文完整可解密。
            verify_container_copy(directory, password, id)?;
            let target = ContainerDb::open(directory, password, Some(id))?;
            // 复核两端租约仍有效后，把目标元数据交回创建流程登记。
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
            // 目标已登记走正式删除；未登记则使缓存失效后直接删目录。
            let cleanup = if store.registrations()?.iter().any(|r| r.div_id == target_id) {
                delete_container(&target_id)
            } else {
                veil_core::database::invalidate(
                    &destination.join(veil_core::database::CONTAINER_DB),
                );
                std::fs::remove_dir_all(&destination).map_err(VeilError::from)
            };
            if let Err(cleanup) = cleanup {
                // 清理也失败：错误里带回残留路径，用户可手动处理。
                return Err(invalid(&format!(
                    "复制失败：{error}；目标残留 {} 清理失败：{cleanup}",
                    destination.display()
                )));
            }
        }
        return Err(invalid(&error.to_string()));
    }
    // 成功：释放两端容器租约（源会话与登记保持不变）。
    if let Some(guard) = target_guard {
        guard.release()?;
    }
    source_guard.release()?;
    Ok(())
}

/// 复制源加密数据库并把副本身份改为新容器，清除源租约记录。
/// 入参：source（&ContainerDb）提供源目录；destination（&Path）为已占位目标目录；identity 为新 UUID；name 为副本名称；password 为打开复制数据库的密码。
/// 返回值：完整性检查、身份更新事务及同步请求成功返回 Ok(())。
/// 异常：目标文件已存在、复制/解锁/完整性检查/SQL/同步失败返回 VeilError。
/// 注意事项：调用方须维持源读租约及文件一致性；目标先 create_new 再复制，失败不自动删目标。副本沿用原内容与密钥，不执行重新加密。
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
    // create_new：目标文件必须不存在，防止误覆盖已登记容器。
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    drop(file);
    // 直接复制数据库主文件；本方法不开启源事务，源稳定性依赖调用方协调，复制后再校验目标完整性。
    copy_database_file(source, &path)?;
    let mut target = veil_core::database::open(&path, Some(password), false)?;
    // 完整性检查：复制结果必须是结构完整的 SQLite 库。
    let integrity: String = target.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(invalid("容器数据库文件副本完整性校验失败"));
    }
    let tx = target.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // 事务内改身份：新 veil_id、沿用名称、重置创建时间。
    tx.execute(
        "UPDATE div_info SET veil_id=?1,name=?2,created_at=?3 WHERE id=1",
        rusqlite::params![identity, name, now()],
    )?;
    // 清空源设备的运行锁：导出的副本不保留原设备锁记录。
    tx.execute("DELETE FROM operation_locks", [])?;
    tx.commit()?;
    drop(target);
    // 使后续打开不再复用旧缓存并请求同步；已借出的连接不因 invalidate 被强制关闭。
    veil_core::database::invalidate(&path);
    veil_core::fsutil::sync_file(&std::fs::File::open(&path)?)?;
    veil_core::fsutil::sync_directory(destination)?;
    Ok(())
}

/// 逐字节复制源数据库主文件，并核对复制字节数。
/// 入参：source（&ContainerDb）提供源数据库目录；destination（&Path）为目标数据库文件路径。
/// 返回值：复制数与事先读取的源长度一致且同步目标成功返回 Ok(())。
/// 异常：元数据、复制、同步失败或长度不一致返回错误。
/// 注意事项：直接写目标，已有目标可被覆盖；无暂存发布及失败清理。调用方负责源稳定性，方法不复制 WAL/日志，也不启动 SQLite 备份事务。
fn copy_database_file(
    source: &crate::storage::container::ContainerDb,
    destination: &std::path::Path,
) -> Result<(), VeilError> {
    let source_path = source.directory.join(veil_core::database::CONTAINER_DB);
    // expected：源文件字节数，复制后必须完全一致。
    let expected = std::fs::metadata(&source_path)?.len();
    let copied = std::fs::copy(&source_path, destination)?;
    if copied != expected {
        return Err(crate::storage::invalid("容器数据库文件复制长度不一致"));
    }
    veil_core::fsutil::sync_file(&std::fs::File::open(destination)?)?;
    Ok(())
}

/// 检查容器主数据库入口当前是否为普通文件。
/// 入参：directory（&Path）为容器根目录。
/// 返回值：veil.db 是非符号链接普通文件时返回 Ok(())。
/// 异常：缺失/无法读取元数据或类型无效时返回错误。
/// 注意事项：仅检查这一刻的文件类型，不打开数据库、不验证密码，也无法防止随后外部替换。
fn ensure_regular_container_database(directory: &std::path::Path) -> Result<(), VeilError> {
    let metadata = std::fs::symlink_metadata(directory.join(veil_core::database::CONTAINER_DB))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(crate::storage::invalid("容器数据库文件无效"));
    }
    Ok(())
}

/// 把源容器的非数据库目录树复制到已占位副本。
/// 入参：source（&Path）为源根目录；destination（&Path）为已存在目标目录。
/// 返回值：子树复制、权限设置及同步请求完成返回 Ok(())。
/// 异常：符号链接、特殊文件、目标已存在或遍历/复制/权限/同步失败返回错误。
/// 注意事项：根层跳过主库及 journal/wal/shm；其他普通文件和目录包括暂存项都会复制。无内置取消或清理，失败留下部分副本由上层处理。
fn copy_container_tree(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> Result<(), VeilError> {
    use crate::storage::invalid;
    use std::fs;

    /// 复制当前层的每个文件或子目录，保持副本相对位置。
    /// 入参：source/destination（&Path）为对应层源/目标目录；root（bool）为 true 时跳过根层数据库及日志。
    /// 返回值：本层所有子项及其子树复制并请求同步完成返回 Ok(())。
    /// 异常：目录项读取、类型检查、目标占用、文件长度/权限/同步问题返回错误。
    /// 注意事项：条目按文件名排序后处理；目录递归不比较长度，普通文件才校验复制长度。不提供整体原子性或取消。
    fn copy_directory(
        source: &std::path::Path,
        destination: &std::path::Path,
        root: bool,
    ) -> Result<(), VeilError> {
        use crate::storage::invalid;
        use std::{fs, io};
        // entries：本层目录项，按文件名排序复制（便于一致性校验）。
        let mut entries = fs::read_dir(source)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            // 每项为当前层的一个磁盘条目：目录递归，普通文件流式复制并校验长度，均请求同步。
            let source_path = entry.path();
            let name = entry.file_name();
            // 根层跳过数据库文件：它已由 copy_container_database 单独处理。
            if root && name == veil_core::database::CONTAINER_DB {
                continue;
            }
            // 根层跳过数据库日志（journal/wal/shm）：副本是关闭状态，无需日志。
            if root
                && ["veil.db-journal", "veil.db-wal", "veil.db-shm"]
                    .contains(&name.to_string_lossy().as_ref())
            {
                continue;
            }
            let destination_path = destination.join(name);
            let metadata = fs::symlink_metadata(&source_path)?;
            // 符号链接一律拒绝：容器目录里不允许出现可被替换的间接引用。
            if metadata.file_type().is_symlink() {
                return Err(invalid("容器目录中不能包含符号链接"));
            }
            if metadata.is_dir() {
                // 目录：先创建并同步父目录，再递归复制子内容，最后恢复权限。
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
                // 文件：create_new 保证不覆盖已有目标，流式复制并校验长度。
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
                // 其他类型（设备文件、套接字等）一律拒绝。
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
    // 从根开始递归复制，最后恢复根目录权限并同步。
    copy_directory(source, destination, true)?;
    fs::set_permissions(destination, source_metadata.permissions())?;
    veil_core::fsutil::sync_directory(destination)?;
    Ok(())
}

/// 验证副本自身的目录类型、数据库身份和全部引用密文认证。
/// 入参：directory 为副本根目录；password 为数据库密码；identity 为期望新容器 UUID。
/// 返回值：全部引用文件可按存储密钥解密到丢弃输出时返回 Ok(())。
/// 异常：非法目录类型、数据库/身份错误、缺密文/节点/密钥、认证或 I/O 失败返回错误。
/// 注意事项：不与源逐字节或摘要比较；目录类型检查覆盖额外文件，但仅元数据引用的密文做解密认证。明文不落盘，返回元数据含解密密钥须按敏感数据处理。
fn verify_container_copy(
    directory: &std::path::Path,
    password: &str,
    identity: &str,
) -> Result<(), VeilError> {
    use crate::storage::{container::ContainerDb, invalid};

    /// 递归拒绝副本目录中的符号链接与特殊文件。
    /// 入参：directory（&Path）为当前待查子目录。
    /// 返回值：所有后代条目类型均为目录或普通文件返回 Ok(())。
    /// 异常：读取目录/元数据失败，或发现符号链接及特殊文件返回错误。
    /// 注意事项：本函数不读文件内容或认证密文；由外层另行认证引用的加密文件。
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
        // 逐文件认证：密文必须真实存在且是普通文件。
        let path = directory.join(&file.encrypted_name);
        let entry = std::fs::symlink_metadata(&path)?;
        if entry.file_type().is_symlink() || !entry.is_file() {
            return Err(invalid("目标容器缺少有效的密文文件"));
        }
        // 按节点找到文件密钥。
        let node = db
            .node(&file.file_id)?
            .ok_or_else(|| invalid("文件身份缺失"))?;
        let key = db
            .keys()?
            .into_iter()
            .find(|key| Some(&key.key_id) == node.key_id.as_ref())
            .ok_or_else(|| invalid("文件密钥缺失"))?;
        // 全量解密写入 sink：认证每个分块，不把明文落盘。
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
/// 为已进入变更阶段但失败的删除操作记录排查线索。
/// 入参：operation（&'static str）为固定操作标签；identity（&str）为业务 ID；phase（`Option<&'static str>`）为进入的阶段；result（&`Result<T, VeilError>`）为原业务结果。
/// 返回值：无；仅当有阶段且结果为 Err 时记录日志。
/// 异常：无错误返回，不改写原业务结果。
/// 注意事项：不恢复文件或登记，也不判断确切完成程度；日志在内部操作资源释放后写出，阶段代表尝试点。
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

/// 数据库文件复制与容器删除流程的测试模块。
#[cfg(test)]
mod database_copy_tests {
    use super::{copy_container_database, copy_database_file};
    use crate::storage::{
        container::ContainerDb,
        locks::{LockManager, LockMode, LockOptions},
    };
    use std::time::Duration;

    /// 验证数据库文件复制是逐字节一致的，且不占用源数据库事务。
    #[test]
    /// 验证复制后的 SQLite 数据库文件逐字节一致，复制过程不要求在源数据库上开启长事务。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn database_file_copy_is_byte_for_byte_without_source_transaction() {
        let root = tempfile::tempdir().unwrap();
        let id = veil_core::metadata::generate_veil_id();
        let directory = root.path().join(&id);
        let source = ContainerDb::create(&directory, &id, "source", "pw").unwrap();
        // original：复制前的源库字节快照，用于事后比对未被破坏。
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

    /// 验证"先复制并改身份、清源锁，再加目标锁"的顺序：目标库加锁后
    /// 不能被再次复制覆盖，已有目标锁也不被搬运。
    #[test]
    /// 验证复制数据库先改为新身份并清除源操作租约，再取得目标容器写租约。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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

/// 容器删除流程的测试模块（生命周期锁、失败回滚行为）。
#[cfg(test)]
mod container_delete_tests {
    use super::delete_container_in_store;
    use crate::storage::app::AppStore;

    /// 为容器删除测试准备独立应用库和已登记的假容器目录。
    /// 入参：无。
    /// 返回值：(TempDir, AppStore, String, PathBuf)，依次为自动清理根目录的守卫、应用库、容器 UUID、容器目录；目录内包含故意无效的数据库和假密文。
    /// 异常：临时目录、数据库、文件写入或登记失败通过 unwrap 触发 panic，使测试准备失败。
    /// 注意事项：测试删除不需解密，故意不用真实容器数据库；必须保留 TempDir 至测试结束，避免资源提前清理。
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

    /// 验证删除使用生命周期锁：共享锁被占用时拒绝删除且不动磁盘/登记；
    /// 释放后删除成功，且不影响其他容器的共享锁。
    #[test]
    /// 验证删除目标容器时持有生命周期锁，并且同空间其他容器仍可正常打开。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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

    /// 验证登记事务失败时磁盘目录完全不动（先登记、后删盘）。
    #[test]
    /// 验证删除登记事务失败时，磁盘上的容器目录不会被提前移除。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn registration_failure_keeps_directory_untouched() {
        let (_root, mut store, id, directory) = fixture();
        // 注入故障：删除 space_div 时触发 SQL 错误，模拟登记事务失败。
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
    /// 验证登记已提交后目录删除失败不恢复登记，且错误带回原目录路径。
    #[cfg(unix)]
    #[test]
    /// 验证单容器登记已提交但磁盘删除失败时，登记不恢复且错误给出残留路径。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：准备或断言失败触发 panic；具有绕过目录权限能力的运行用户可能使故障注入失效。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn directory_removal_failure_does_not_restore_deleted_registration() {
        use std::os::unix::fs::PermissionsExt;
        let (_root, mut store, id, directory) = fixture();
        // 把目录权限改为只读，让 remove_dir_all 失败模拟"登记已删、目录没删成"。
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();
        let mut phase = None;
        let result = delete_container_in_store(&mut store, &id, &mut phase);
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let error = result.unwrap_err();
        // phase 停在 remove_files：登记事务已提交，删除卡在磁盘阶段。
        assert_eq!(phase, Some("remove_files"));
        // 错误信息包含原目录路径，供用户手动清理。
        assert!(error.to_string().contains(directory.to_str().unwrap()));
        assert!(store.conn.is_autocommit());
        // 登记不回滚：registrations 已空，目录仍存在。
        assert!(store.registrations().unwrap().is_empty());
        assert!(directory.is_dir());
        assert_eq!(
            std::fs::read_dir(directory.parent().unwrap())
                .unwrap()
                .count(),
            1
        );
        // 删除锁已释放：可以再次申请删除锁重试。
        assert!(store.acquire_container_delete_lock(&id).is_ok());
    }
}
