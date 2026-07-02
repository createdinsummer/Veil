//! # index —— 目录树的数据模型与序列化
//!
//! 本模块定义容器里「有哪些文件、各在什么位置」的元数据（对应 spec §3 的 Index）：
//!
//! - [`FsNode`]：目录树里的一个节点（文件或目录），记录虚拟路径、大小、
//!   对应 blob 在容器内的偏移/长度、明文 blake3 哈希等；
//! - [`Kind`]：节点类型（文件 / 目录）；
//! - [`serialize_index`] / [`deserialize_index`]：整棵目录树 `Vec<FsNode>`
//!   与紧凑二进制（postcard）之间的相互转换。
//!
//! 这层**只管数据结构与序列化，不碰加密**：序列化出的字节之后会被
//! 用容器公钥整体加密成 Index，写进 `.veil`。P1 用扁平列表（每个节点带完整
//! 路径），GUI 浏览时（P4）再按路径折叠成树。

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// 条目类型：是文件还是目录。
///
/// derive 说明：
/// - `Serialize/Deserialize`：让 serde 能把它转成字节 / 从字节还原（postcard 靠这个）。
/// - `Debug`：可用 `{:?}` 打印（调试用；这里没有秘密，安全）。
/// - `Clone`：可复制。
/// - `PartialEq`：可用 `==` 比较（写测试要用）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Kind {
    File,
    Dir,
}


/// 文件系统节点：目录树里的一个条目，可能是文件也可能是目录（对应 spec §3 的 Entry）。
/// 整棵树就是 `Vec<FsNode>`，序列化加密后即容器里的 Index。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FsNode {
    /// 虚拟路径，如 "photos/2024/img1.jpg"（明文路径只存在于加密后的 Index 里）
    pub path: String,
    /// 文件还是目录
    pub kind: Kind,
    /// 明文大小（字节）
    pub size: u64,
    /// 该文件的 age 密文块在容器内的起始偏移
    pub blob_offset: u64,
    /// 该 age 密文块的长度
    pub blob_len: u64,
    /// 明文的 blake3 哈希，用于往返校验（P5 强制用）
    pub content_hash: [u8; 32],
    /// MIME 类型，用来选查看器（image/jpeg 等）；可能没有，用 Option
    pub mime: Option<String>,
    /// 修改时间（Unix 秒）；可能没有
    pub mtime: Option<i64>,
}
/// 把整棵目录树序列化成紧凑二进制（尚未加密）。
pub fn serialize_index(entries: &[FsNode]) -> Result<Vec<u8>> {
    Ok(postcard::to_stdvec(entries)?)
}

/// 反向：从字节还原出目录树。
pub fn deserialize_index(bytes: &[u8]) -> Result<Vec<FsNode>> {
    Ok(postcard::from_bytes::<Vec<FsNode>>(bytes)?)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_serialize_roundtrip() {
        let tree = vec![
            FsNode {
                path: "photos".to_owned(),
                kind: Kind::Dir,
                size: 0,
                blob_offset: 0,
                blob_len: 0,
                content_hash: [0u8; 32],
                mime: None,
                mtime: None,
            },
            FsNode {
                path: "photos/a.jpg".to_owned(),
                kind: Kind::File,
                size: 1234,
                blob_offset: 64,
                blob_len: 1300,
                content_hash: [7u8; 32],
                mime: Some("image/jpeg".to_owned()),
                mtime: Some(1_700_000_000),
            },
        ];

        let bytes = serialize_index(&tree).unwrap();
        println!("目录树序列化后 = {} 字节（紧凑二进制）", bytes.len());

        let back = deserialize_index(&bytes).unwrap();
        assert_eq!(back, tree);
    }
}