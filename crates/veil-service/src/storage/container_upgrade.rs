//! 容器数据库结构校验与空白库初始化入口，属于服务层容器存储模块。
//! 核心函数 check_and_upgrade 选择接受的结构版本；依赖通用 ensure_schema、固定建表SQL与SQLite连接，
//! 目标是拒绝不受支持结构，避免各业务入口自行补表造成格式不一致。
//!
//! 当前首发版本只验证并创建版本 1；不包含低版本迁移。未来结构变化时，
//! 将经确认的逐版本迁移步骤集中登记在此模块，再由本入口按序执行。
//! 注意：升级入口在「设置密码并验证解锁之后」才被调用，业务表结构不能
//! 作为密码派生的入口，也不能被各业务模块各自补表绕过。
use super::{ensure_schema, invalid};
use crate::error::Result;
use rusqlite::Connection;

// 容器库的类型标识：写入 PRAGMA application_id，与 app 库（0x56415050）区分。
const APPLICATION_ID: i64 = 0x56444956;
// 当前支持的业务表结构版本：首发格式固定为 1，只接受精确匹配。
const CURRENT_VERSION: i64 = 1;

/// 验证当前容器业务结构，或在获准的空白数据库上初始化版本1。
/// 入参：conn（&mut Connection）为已设置密码并可访问的容器连接；create（bool）为调用方授权初始化空白库的标志，false 不允许从版本0建表。
/// 返回值：现有版本1、类型标识和结构校验通过，或新空白库初始化提交成功时返回 Ok(())。
/// 异常：不支持的 user_version、未知类型/非空未知库/结构指纹不符返回 InvalidFormat；查询、事务或建表错误传播为数据库错误。
/// 注意事项：本版本无迁移步骤；函数名不意味着会升级旧格式。新建分支执行固定 container.sql 并写标识/版本；已有匹配结构分支才比对实际指纹。
pub(crate) fn check_and_upgrade(conn: &mut Connection, create: bool) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    // 只认 0（未初始化）和当前版本，其他版本不猜测、不迁移。
    if version != 0 && version != CURRENT_VERSION {
        return Err(invalid("容器数据库结构版本不受支持"));
    }
    // 交给通用入口：空白库分支建表并写标识/版本；已匹配版本分支核对结构指纹，并非每次都执行三步。
    ensure_schema(
        conn,
        APPLICATION_ID,
        include_str!("container.sql"),
        CURRENT_VERSION,
        create,
    )
}
