//! 文件版本链与离线继承关系。
//!
//! # 业务定位
//!
//! 容器里每个文件都维护一条"内容版本链"（按时间顺序排列的版本 UUID）：
//! 内容每次真实变化就追加一个版本 UUID，改名/移动/重新加密不追加；版本链只
//! 记录继承关系，不保留历史内容副本。本模块负责：校验版本链合法性，
//! 并比较两条链（本地链 vs 导入链）之间的继承关系，供"防止旧覆盖新/防止分叉
//! 冲突"的提交校验使用。
//!
//! # 核心规则
//!
//! - 版本链必须非空，成员是合法且不重复的 UUID v4。
//! - 只按"前缀关系"判断新旧：谁是谁的前缀，谁就落后；绝不按链长猜测新旧。
//! - 分叉（有共同起点但随后各走各路）与无关（没有共同起点）都要明确拒绝覆盖。
//!
//! 核心类型与入口：VersionSnapshot 保存一次读取的身份及版本链，VersionRelation 表达五种关系，
//! compare_version_history 先检查两条链再按相等、严格前缀、共同起点顺序分类。
//! 依赖：metadata::is_uuid_v4 解释版本身份，HashSet 检测链内重复，Serde 支持快照和关系的传输表示；不访问数据库。
use crate::error::VeilError;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// 一次文件读取对应的容器身份、文件身份和内容版本链。
///
/// 业务定位：读取文件时拿到的"版本快照"，保存时要原样带回，作为"我看到的
/// 还是不是现在数据库里的版本"的比对基准（基础版本校验）。三条信息共同锁定
/// 一个唯一的内容状态，防止把旧版本内容覆盖到已经更新的文件上。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionSnapshot {
    /// 文件所属容器的稳定身份。
    /// 业务含义：确认后续操作仍作用在同一个容器上，防串容器。
    pub veil_id: String,
    /// 文件的稳定身份。
    /// 业务含义：文件 ID 不随改名、移动或重新加密变化，用于定位目标节点。
    pub file_id: String,
    /// 从首个版本到本次读取版本的 UUID 链。
    /// 业务含义：内容版本链快照，按时间顺序排列；提交时与数据库当前链比对。
    pub version_history: Vec<String>,
}

/// 以本地版本链为基准的导入版本关系。
///
/// 业务定位：比较"本地链"与"导入/待提交链"的继承关系，决定导入内容能否
/// 覆盖本地内容。关系判定基于完整链的相等和前缀关系，而不是单凭长度推断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionRelation {
    /// 版本链完全一致。
    /// 业务含义：两边声明的版本链相同；是否允许覆盖还需由调用方核对身份和内容真实性。
    Same,
    /// 本地链是导入链的严格前缀。
    /// 业务含义：导入方在本地版本之后追加过版本，可供调用方判断是否接受更新。
    IncomingAhead,
    /// 导入链是本地链的严格前缀。
    /// 业务含义：本地在导入之后又有新版本，导入内容陈旧，不能覆盖本地。
    LocalAhead,
    /// 两条链有共同起点，但随后分叉。
    /// 业务含义：两边各自追加过版本，无法判断谁新，必须拒绝覆盖并报冲突。
    Diverged,
    /// 两条链没有共同起点。
    /// 业务含义：两个内容历史完全无关，视为不同文件/不同来源，拒绝合并。
    Unrelated,
}

/// 校验版本链并比较继承关系；只按前缀判断新旧，不按长度猜测。
///
/// 本函数只比较版本链；调用方需要另行确认容器身份、文件身份和数据真实性。
///
/// 功能：先校验两条版本链本身合法（非空、UUID v4、无重复），再比较两者的
/// 继承关系，输出版本关系结论，供写入提交时的基础版本校验使用。
///
/// 入参：`local` 是本地当前版本链，`incoming` 是导入/待提交的版本链；
/// 每条都是按时间顺序排列的版本 UUID 列表。
///
/// 返回值：`VersionRelation` 五选一的关系判定；链条非法时返回
/// `VeilError::InvalidFormat`。
///
/// 异常/错误：链条为空、包含非 UUID v4 字符串或出现重复 UUID 时返回
/// `InvalidFormat`；本函数不 panic。
///
/// 注意事项：只判断版本链，不校验容器/文件身份与内容真实性；比较基于
/// 前缀关系而非链长，调用方不得自行用长度猜新旧。
pub fn compare_version_history(
    local: &[String],
    incoming: &[String],
) -> Result<VersionRelation, VeilError> {
    // 两条链逐条做同一套合法性校验：非空、每个成员是 UUID v4、且不重复。
    for chain in [local, incoming] {
        // seen 集合用于检测链内重复：HashSet 记录已经见过的版本 UUID。
        let mut seen = HashSet::new();
        if chain.is_empty()
            || chain
                .iter()
                .any(|id| !crate::metadata::is_uuid_v4(id) || !seen.insert(id))
        {
            return Err(VeilError::InvalidFormat(
                "版本链为空，或包含无效、重复的版本 UUID".into(),
            ));
        }
    }
    // 两条声明的版本链完全相同；这一步本身不验证文件内容是否一致。
    if local == incoming {
        return Ok(VersionRelation::Same);
    }
    // 导入链以本地链开头：本地历史是导入历史的前缀，说明导入方追加过版本。
    if incoming.starts_with(local) {
        return Ok(VersionRelation::IncomingAhead);
    }
    // 本地链以导入链开头：导入内容只是本地历史的旧前缀，导入内容陈旧，拒绝覆盖。
    if local.starts_with(incoming) {
        return Ok(VersionRelation::LocalAhead);
    }
    // 前缀都不成立时看共同起点：首个版本相同说明同源后分叉（例如两台设备各自修改过）。
    if local[0] == incoming[0] {
        Ok(VersionRelation::Diverged)
    } else {
        // 首个版本不同则按本项目规则归为无共同起点；这里没有检查两条链是否在其他位置交叉。
        Ok(VersionRelation::Unrelated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 功能：把简短数字样本转换成固定形状的合法 UUID v4 列表，便于测试版本链关系。
    /// 入参：values: &[u8] 为按历史顺序排列的节点编号，每个数字对应一枚测试版本 UUID。
    /// 返回值：`Vec<String>`，按输入顺序生成 UUID，空输入返回空列表，重复数字生成相同 UUID。
    /// 异常：只做格式化，不返回业务错误。
    /// 注意事项：不读取父子节点，也不生成随机值；重复和空样本用于验证非法链，不能用于生产身份分配。
    fn chain(values: &[u8]) -> Vec<String> {
        values
            .iter()
            .map(|value| format!("00000000-0000-4000-8000-{value:012x}"))
            .collect()
    }

    #[test]
    /// 验证版本比较依据链结构和分支关系，而不是简单按版本字符串长度排序。
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn compares_prefixes_and_branches_without_using_length_as_order() {
        assert_eq!(
            compare_version_history(&chain(&[1, 2]), &chain(&[1, 2])).unwrap(),
            VersionRelation::Same
        );
        assert_eq!(
            compare_version_history(&chain(&[1]), &chain(&[1, 2])).unwrap(),
            VersionRelation::IncomingAhead
        );
        assert_eq!(
            compare_version_history(&chain(&[1, 2]), &chain(&[1])).unwrap(),
            VersionRelation::LocalAhead
        );
        assert_eq!(
            compare_version_history(&chain(&[1, 2]), &chain(&[1, 3, 4])).unwrap(),
            VersionRelation::Diverged
        );
        assert_eq!(
            compare_version_history(&chain(&[1, 2]), &chain(&[3])).unwrap(),
            VersionRelation::Unrelated
        );
        assert!(compare_version_history(&[], &chain(&[1])).is_err());
        assert!(compare_version_history(&chain(&[1, 1]), &chain(&[1])).is_err());
        assert!(compare_version_history(&["invalid".into()], &chain(&[1])).is_err());
    }
}
