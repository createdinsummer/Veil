//! # index 集成测试
//!
//! 测试 `veil_core::index` 模块的公开 API，验证嵌套目录树的操作。

use veil_core::index::{FileMeta, Tree, deserialize_index, serialize_index};
use veil_core::index::{insert_file, remove_file, get_file, list_files, match_files};

/// 构造指定大小的测试文件元数据。
fn create_test_meta(size: u64) -> FileMeta {
    FileMeta {
        size,
        blob_offset: 0,
        blob_len: size,
        content_hash: [0u8; 32],
        mime: Some("text/plain".to_string()),
        mtime: None,
    }
}

/// 验证空目录树能够完成序列化往返。
#[test]
fn test_serialize_and_deserialize_empty_tree() {
    use std::collections::BTreeMap;
    let tree: Tree = BTreeMap::new();

    let bytes = serialize_index(&tree).unwrap();
    let deserialized = deserialize_index(&bytes).unwrap();

    assert_eq!(tree.len(), deserialized.len());
}

/// 验证包含多级文件的目录树能够完成序列化往返。
#[test]
fn test_serialize_and_deserialize_with_files() {
    use std::collections::BTreeMap;
    let mut tree: Tree = BTreeMap::new();
    insert_file(&mut tree, "file1.txt", create_test_meta(100));
    insert_file(&mut tree, "dir/file2.txt", create_test_meta(200));
    insert_file(&mut tree, "dir/subdir/file3.txt", create_test_meta(300));

    let bytes = serialize_index(&tree).unwrap();
    let deserialized = deserialize_index(&bytes).unwrap();

    assert!(get_file(&deserialized, "file1.txt").is_some());
    assert!(get_file(&deserialized, "dir/file2.txt").is_some());
    assert!(get_file(&deserialized, "dir/subdir/file3.txt").is_some());
}

/// 验证插入文件后可按完整路径查询。
#[test]
fn test_insert_and_get_file() {
    use std::collections::BTreeMap;
    let mut tree: Tree = BTreeMap::new();
    let meta = create_test_meta(1024);

    insert_file(&mut tree, "test.txt", meta.clone());

    let retrieved = get_file(&tree, "test.txt").unwrap();
    assert_eq!(retrieved.size, 1024);
    assert_eq!(retrieved.mime, Some("text/plain".to_string()));
}

/// 验证嵌套路径会创建对应的中间目录节点。
#[test]
fn test_insert_nested_files() {
    use std::collections::BTreeMap;
    let mut tree: Tree = BTreeMap::new();

    insert_file(&mut tree, "a/b/c/file.txt", create_test_meta(100));

    assert!(get_file(&tree, "a/b/c/file.txt").is_some());
    assert!(get_file(&tree, "a/b/file.txt").is_none());
}

/// 验证删除文件会返回原元数据并移除节点。
#[test]
fn test_remove_file() {
    use std::collections::BTreeMap;
    let mut tree: Tree = BTreeMap::new();

    insert_file(&mut tree, "test.txt", create_test_meta(100));
    assert!(get_file(&tree, "test.txt").is_some());

    let removed = remove_file(&mut tree, "test.txt");
    assert!(removed.is_some());
    assert_eq!(removed.unwrap().size, 100);

    assert!(get_file(&tree, "test.txt").is_none());
}

/// 验证删除不存在的文件返回 `None`。
#[test]
fn test_remove_nonexistent_file() {
    use std::collections::BTreeMap;
    let mut tree: Tree = BTreeMap::new();

    let removed = remove_file(&mut tree, "nonexistent.txt");
    assert!(removed.is_none());
}

/// 验证遍历结果包含所有文件的完整路径。
#[test]
fn test_list_files() {
    use std::collections::BTreeMap;
    let mut tree: Tree = BTreeMap::new();

    insert_file(&mut tree, "file1.txt", create_test_meta(100));
    insert_file(&mut tree, "file2.txt", create_test_meta(200));
    insert_file(&mut tree, "dir/file3.txt", create_test_meta(300));

    let mut files = list_files(&tree);
    files.sort_by(|a, b| a.0.cmp(&b.0));

    assert_eq!(files.len(), 3);
    assert_eq!(files[0].0, "dir/file3.txt");
    assert_eq!(files[1].0, "file1.txt");
    assert_eq!(files[2].0, "file2.txt");
}

/// 验证单层通配符不匹配下级目录。
#[test]
fn test_match_files_with_wildcard() {
    use std::collections::BTreeMap;
    let mut tree: Tree = BTreeMap::new();

    insert_file(&mut tree, "image1.png", create_test_meta(100));
    insert_file(&mut tree, "image2.png", create_test_meta(200));
    insert_file(&mut tree, "doc.txt", create_test_meta(300));
    insert_file(&mut tree, "photos/pic.png", create_test_meta(400));

    let mut matches = match_files(&tree, "*.png").unwrap();
    matches.sort_by(|a, b| a.0.cmp(&b.0));

    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].0, "image1.png");
    assert_eq!(matches[1].0, "image2.png");
}

/// 验证目录限定模式只匹配指定目录下的文件。
#[test]
fn test_match_files_with_directory_pattern() {
    use std::collections::BTreeMap;
    let mut tree: Tree = BTreeMap::new();

    insert_file(&mut tree, "photos/pic1.png", create_test_meta(100));
    insert_file(&mut tree, "photos/pic2.jpg", create_test_meta(200));
    insert_file(&mut tree, "docs/file.txt", create_test_meta(300));

    let mut matches = match_files(&tree, "photos/*").unwrap();
    matches.sort_by(|a, b| a.0.cmp(&b.0));

    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].0, "photos/pic1.png");
    assert_eq!(matches[1].0, "photos/pic2.jpg");
}

/// 验证递归通配符跨目录匹配全部目标文件。
#[test]
fn test_match_files_recursive_wildcard() {
    use std::collections::BTreeMap;
    let mut tree: Tree = BTreeMap::new();

    insert_file(&mut tree, "a/b/file1.txt", create_test_meta(100));
    insert_file(&mut tree, "a/c/file2.txt", create_test_meta(200));
    insert_file(&mut tree, "d/file3.txt", create_test_meta(300));

    let matches = match_files(&tree, "**/*.txt").unwrap();
    assert_eq!(matches.len(), 3);
}

/// 验证同一路径再次插入会覆盖旧元数据。
#[test]
fn test_overwrite_existing_file() {
    use std::collections::BTreeMap;
    let mut tree: Tree = BTreeMap::new();

    insert_file(&mut tree, "test.txt", create_test_meta(100));
    insert_file(&mut tree, "test.txt", create_test_meta(200));

    let meta = get_file(&tree, "test.txt").unwrap();
    assert_eq!(meta.size, 200);
}

/// 验证空目录树查询任意路径均返回 `None`。
#[test]
fn test_get_file_from_empty_tree() {
    use std::collections::BTreeMap;
    let tree: Tree = BTreeMap::new();

    assert!(get_file(&tree, "any.txt").is_none());
}

/// 验证空目录树遍历结果为空。
#[test]
fn test_list_files_empty_tree() {
    use std::collections::BTreeMap;
    let tree: Tree = BTreeMap::new();

    let files = list_files(&tree);
    assert_eq!(files.len(), 0);
}
