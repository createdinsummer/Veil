//! 容器数据库结构检查与升级的唯一入口。
//!
//! 当前首发版本只验证并创建版本 1；不包含低版本迁移。未来结构变化时，
//! 将经确认的逐版本迁移步骤集中登记在此模块，再由本入口按序执行。
use super::{ensure_schema, invalid};
use crate::error::Result;
use rusqlite::Connection;

const APPLICATION_ID: i64 = 0x56444956;
const CURRENT_VERSION: i64 = 1;

pub(crate) fn check_and_upgrade(conn: &mut Connection, create: bool) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != 0 && version != CURRENT_VERSION {
        return Err(invalid("容器数据库结构版本不受支持"));
    }
    ensure_schema(
        conn,
        APPLICATION_ID,
        include_str!("container.sql"),
        CURRENT_VERSION,
        create,
    )
}
