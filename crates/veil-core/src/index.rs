//! 容器目录树的数据模型与序列化。
//!
//! 容器的目录使用嵌套树表示：
//! - [`Node`]：一个节点，是文件（[`FileMeta`]）或目录（子节点 map）；
//! - [`Tree`]：目录树的根 = 顶层「名字 → 节点」的 `BTreeMap`（按名字有序）；
//! - **路径由节点在树中的位置隐含**，不再冗余存整条路径。
//!
//! 提供路径导航（[`insert_file`]/[`remove_file`]/[`get_file`]/[`list_files`]）
//! 与序列化（[`serialize_index`]/[`deserialize_index`]，postcard）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// 一个文件的元数据（路径由它在树中的位置隐含，故这里不含 path）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileMeta {
    /// 明文大小（字节）
    pub size: u64,
    /// 该文件的 age 密文块在容器内的起始偏移
    pub blob_offset: u64,
    /// 该 age 密文块的长度
    pub blob_len: u64,
    /// 明文的 blake3 哈希，用于往返校验
    pub content_hash: [u8; 32],
    /// MIME 类型（选查看器用）；可能没有
    pub mime: Option<String>,
    /// 修改时间（Unix 秒）；可能没有
    pub mtime: Option<i64>,
}

/// 目录树节点。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Node {
    /// 文件及其 blob 定位、完整性和展示元数据。
    File(FileMeta),
    /// 目录及其按名称排序的直接子节点。
    Dir(BTreeMap<String, Node>),
}

/// 目录树的根：顶层「名字 → 节点」。`BTreeMap` 保证按名字有序。
pub type Tree = BTreeMap<String, Node>;

/// 使用 postcard 序列化整棵目录树。
///
/// # 错误
/// 树中的字段无法完成 postcard 编码时返回 [`crate::error::VeilError::Serialize`]。
pub fn serialize_index(root: &Tree) -> Result<Vec<u8>> {
    Ok(postcard::to_stdvec(root)?)
}

/// 从 postcard 字节反序列化目录树。
///
/// # 错误
/// 字节不足、格式无效或字段类型不匹配时返回 [`crate::error::VeilError::Serialize`]。
pub fn deserialize_index(bytes: &[u8]) -> Result<Tree> {
    Ok(postcard::from_bytes::<Tree>(bytes)?)
}

/// 把虚拟路径拆成非空段（"a//b/" → ["a","b"]）。
fn split_path(path: &str) -> Vec<&str> {
    // 连续分隔符和首尾斜杠不会产生空节点，统一按非空路径段处理。
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// 插入 / 覆盖一个文件（沿途缺失的目录自动创建）。
///
/// 已存在同名文件则覆盖；中间段若撞上同名文件，会被强制变成目录（罕见冲突）。
pub fn insert_file(root: &mut Tree, path: &str, meta: FileMeta) {
    let parts = split_path(path);
    let Some((last, dirs)) = parts.split_last() else {
        return; // 空路径，忽略
    };

    let mut cur = root;
    // 中间路径段必须对应目录；若当前节点是文件则替换为目录。
    for part in dirs {
        let entry = cur
            .entry((*part).to_owned())
            .or_insert_with(|| Node::Dir(BTreeMap::new()));
        if !matches!(entry, Node::Dir(_)) {
            *entry = Node::Dir(BTreeMap::new()); // 冲突：文件让位给目录
        }
        cur = match entry {
            Node::Dir(children) => children,
            Node::File(_) => unreachable!(),
        };
    }
    // 最后一个路径段写为文件，BTreeMap::insert 负责覆盖旧节点。
    cur.insert((*last).to_owned(), Node::File(meta));
}

/// 删除一个文件，返回它的元数据（顺带清理变空的父目录）。找不到 → `None`。
pub fn remove_file(root: &mut Tree, path: &str) -> Option<FileMeta> {
    remove_recursive(root, &split_path(path))
}

/// 递归删除叶子文件；删除后如父目录为空，则由调用层一并清理。
fn remove_recursive(dir: &mut Tree, parts: &[&str]) -> Option<FileMeta> {
    let (first, rest) = parts.split_first()?;
    if rest.is_empty() {
        // 只有叶子节点是文件时才删除；若同名节点为目录则放回原处。
        match dir.remove(*first) {
            Some(Node::File(meta)) => Some(meta),
            Some(other) => {
                dir.insert((*first).to_owned(), other); // 是目录，放回去
                None
            }
            None => None,
        }
    } else {
        // 非叶子路径继续向下递归，失败时保留整个子树不动。
        let removed = match dir.get_mut(*first) {
            Some(Node::Dir(children)) => remove_recursive(children, rest),
            _ => None,
        };
        // 删除后若父目录已经没有任何节点，则顺手清理空目录。
        if let Some(Node::Dir(children)) = dir.get(*first)
            && children.is_empty()
        {
            dir.remove(*first);
        }
        removed
    }
}

/// 按路径查一个文件的元数据（路径指向目录或不存在 → `None`）。
pub fn get_file<'a>(root: &'a Tree, path: &str) -> Option<&'a FileMeta> {
    let parts = split_path(path);
    if parts.is_empty() {
        return None;
    }
    let mut cur = root;
    // 每层都要求“中间是目录、最后是文件”，任何类型不匹配都视为不存在。
    for (i, part) in parts.iter().enumerate() {
        let last = i == parts.len() - 1;
        match cur.get(*part)? {
            Node::File(meta) if last => return Some(meta),
            Node::Dir(children) if !last => cur = children,
            _ => return None,
        }
    }
    None
}

/// 收集所有文件的 `(完整路径, 元数据)`，用于导出/校验遍历。
pub fn list_files(root: &Tree) -> Vec<(String, FileMeta)> {
    let mut out = Vec::new();
    collect(root, "", &mut out);
    out
}

/// 查找匹配通配符模式的所有文件。
///
/// 支持 `*`（匹配任意字符，不含 `/`）和 `**`（匹配任意字符，含 `/`）。
///
/// # 示例
/// - `*.jpg` - 根目录下所有 jpg 文件
/// - `photos/*.jpg` - photos 目录下所有 jpg 文件
/// - `**/*.jpg` - 所有子目录下的 jpg 文件
///
/// # 错误
/// 如果模式无效则返回 `Err`。
pub fn match_files(root: &Tree, pattern: &str) -> Result<Vec<(String, FileMeta)>> {
    use glob::{Pattern, MatchOptions};

    // 先验证模式，避免无效 glob 进入逐文件匹配阶段。
    let glob_pattern = Pattern::new(pattern)
        .map_err(|e| crate::error::VeilError::Format(format!("无效的通配符模式: {}", e)))?;

    // 要求 * 不跨过 /，只有 ** 能递归匹配目录。
    let options = MatchOptions {
        require_literal_separator: true, // * 不匹配 /，只有 ** 才能跨目录
        ..Default::default()
    };

    // 目录树没有反向索引，因此先展开完整路径，再统一做模式过滤。
    let all_files = list_files(root);
    let matched: Vec<_> = all_files
        .into_iter()
        .filter(|(path, _)| glob_pattern.matches_with(path, options))
        .collect();

    Ok(matched)
}

/// 深度优先收集文件，并将当前层级前缀拼成完整虚拟路径。
fn collect(dir: &Tree, prefix: &str, out: &mut Vec<(String, FileMeta)>) {
    // 深度优先遍历，prefix 保存从根到当前目录的已拼接路径。
    for (name, node) in dir {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        match node {
            Node::File(meta) => out.push((path, meta.clone())),
            Node::Dir(children) => collect(children, &path, out),
        }
    }
}

/// 目录树插入、删除、遍历和通配符匹配的单元测试。
#[cfg(test)]
mod tests {
    use super::*;

    /// 构造只关心大小字段的测试文件元数据。
    fn meta(size: u64) -> FileMeta {
        FileMeta {
            size,
            blob_offset: 0,
            blob_len: 0,
            content_hash: [0u8; 32],
            mime: None,
            mtime: None,
        }
    }

    /// 验证文件插入、覆盖、查找、删除和空目录清理。
    #[test]
    fn insert_get_remove() {
        let mut root = Tree::new();
        insert_file(&mut root, "a.txt", meta(1));
        insert_file(&mut root, "d/b.txt", meta(2));
        insert_file(&mut root, "d/e/c.txt", meta(3));

        assert_eq!(get_file(&root, "a.txt").unwrap().size, 1);
        assert_eq!(get_file(&root, "d/b.txt").unwrap().size, 2);
        assert_eq!(get_file(&root, "d/e/c.txt").unwrap().size, 3);
        assert!(get_file(&root, "nope").is_none());
        assert!(get_file(&root, "d").is_none()); // d 是目录不是文件

        // 覆盖
        insert_file(&mut root, "a.txt", meta(99));
        assert_eq!(get_file(&root, "a.txt").unwrap().size, 99);

        // 删除 + 空目录清理（删掉 d/e/c.txt 后 d/e 应被清理）
        assert!(remove_file(&mut root, "d/e/c.txt").is_some());
        assert!(get_file(&root, "d/e/c.txt").is_none());
        let files: Vec<String> = list_files(&root).into_iter().map(|(p, _)| p).collect();
        assert!(files.contains(&"a.txt".to_owned()));
        assert!(files.contains(&"d/b.txt".to_owned()));
        assert!(!files.iter().any(|p| p.contains("c.txt")));
    }

    /// 验证目录树序列化后反序列化保持相等。
    #[test]
    fn serialize_roundtrip() {
        let mut root = Tree::new();
        insert_file(&mut root, "photos/2024/a.jpg", meta(10));
        insert_file(&mut root, "readme.md", meta(20));

        let bytes = serialize_index(&root).unwrap();
        let back = deserialize_index(&bytes).unwrap();
        assert_eq!(back, root);
    }

    /// 验证单层与递归通配符模式的文件匹配结果。
    #[test]
    fn match_files_wildcard() {
        let mut root = Tree::new();
        insert_file(&mut root, "a.jpg", meta(1));
        insert_file(&mut root, "b.png", meta(2));
        insert_file(&mut root, "photos/2024/c.jpg", meta(3));
        insert_file(&mut root, "photos/2024/d.png", meta(4));
        insert_file(&mut root, "photos/2025/e.jpg", meta(5));
        insert_file(&mut root, "docs/readme.txt", meta(6));

        // 匹配根目录 jpg（* 不匹配 /）
        let matched = match_files(&root, "*.jpg").unwrap();
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].0, "a.jpg");

        // 匹配特定目录下的 jpg
        let matched = match_files(&root, "photos/2024/*.jpg").unwrap();
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].0, "photos/2024/c.jpg");

        // 匹配所有 jpg（递归，** 匹配任意层级目录）
        let matched = match_files(&root, "**/*.jpg").unwrap();
        assert_eq!(matched.len(), 3); // 所有 jpg 文件
        let paths: Vec<_> = matched.iter().map(|(p, _)| p.as_str()).collect();
        assert!(paths.contains(&"a.jpg"));
        assert!(paths.contains(&"photos/2024/c.jpg"));
        assert!(paths.contains(&"photos/2025/e.jpg"));

        // 匹配任意年份目录
        let matched = match_files(&root, "photos/*/*.jpg").unwrap();
        assert_eq!(matched.len(), 2);

        // 无匹配
        let matched = match_files(&root, "*.mp4").unwrap();
        assert_eq!(matched.len(), 0);
    }
}
