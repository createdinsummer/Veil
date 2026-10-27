//! 文件版本链与离线继承关系。

use crate::error::VeilError;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// 一次文件读取对应的容器身份、文件身份和内容版本链。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionSnapshot {
    /// 文件所属容器的稳定身份。
    pub veil_id: String,
    /// 文件的稳定身份。
    pub file_id: String,
    /// 从首个版本到本次读取版本的 UUID 链。
    pub version_history: Vec<String>,
}

/// 以本地版本链为基准的导入版本关系。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionRelation {
    /// 版本链完全一致。
    Same,
    /// 本地链是导入链的严格前缀。
    IncomingAhead,
    /// 导入链是本地链的严格前缀。
    LocalAhead,
    /// 两条链有共同起点，但随后分叉。
    Diverged,
    /// 两条链没有共同起点。
    Unrelated,
}

/// 校验版本链并比较继承关系；只按前缀判断新旧，不按长度猜测。
///
/// 本函数只比较版本链；调用方需要另行确认容器身份、文件身份和数据真实性。
pub fn compare_version_history(
    local: &[String],
    incoming: &[String],
) -> Result<VersionRelation, VeilError> {
    for chain in [local, incoming] {
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
    if local == incoming {
        return Ok(VersionRelation::Same);
    }
    if incoming.starts_with(local) {
        return Ok(VersionRelation::IncomingAhead);
    }
    if local.starts_with(incoming) {
        return Ok(VersionRelation::LocalAhead);
    }
    if local[0] == incoming[0] {
        Ok(VersionRelation::Diverged)
    } else {
        Ok(VersionRelation::Unrelated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(values: &[u8]) -> Vec<String> {
        values
            .iter()
            .map(|value| format!("00000000-0000-4000-8000-{value:012x}"))
            .collect()
    }

    #[test]
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
