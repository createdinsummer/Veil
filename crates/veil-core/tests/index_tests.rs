//! # index 集成测试
//!
//! 测试 `veil_core::index` 模块的公开 API，验证嵌套目录树的操作。

use veil_core::index::{FileMeta, Tree, deserialize_index, serialize_index};
use veil_core::index::{insert_file, remove_file, get_file, list_files, match_files};

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

#[test]
fn test_serialize_and_deserialize_empty_tree() {
    use std::collections::BTreeMap;
    let tree: Tree = BTreeMap::new();

    let bytes = serialize_index(&tree).unwrap();
    let deserialized = deserialize_index(&bytes).unwrap();

    assert_eq!(tree.len(), deserialized.len());
}

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

#[test]
fn test_insert_nested_files() {
    use std::collections::BTreeMap;
    let mut tree: Tree = BTreeMap::new();

    insert_file(&mut tree, "a/b/c/file.txt", create_test_meta(100));

    assert!(get_file(&tree, "a/b/c/file.txt").is_some());
    assert!(get_file(&tree, "a/b/file.txt").is_none());
}

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

#[test]
fn test_remove_nonexistent_file() {
    use std::collections::BTreeMap;
    let mut tree: Tree = BTreeMap::new();

    let removed = remove_file(&mut tree, "nonexistent.txt");
    assert!(removed.is_none());
}

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

#[test]
fn test_overwrite_existing_file() {
    use std::collections::BTreeMap;
    let mut tree: Tree = BTreeMap::new();

    insert_file(&mut tree, "test.txt", create_test_meta(100));
    insert_file(&mut tree, "test.txt", create_test_meta(200));

    let meta = get_file(&tree, "test.txt").unwrap();
    assert_eq!(meta.size, 200);
}

#[test]
fn test_get_file_from_empty_tree() {
    use std::collections::BTreeMap;
    let tree: Tree = BTreeMap::new();

    assert!(get_file(&tree, "any.txt").is_none());
}

#[test]
fn test_list_files_empty_tree() {
    use std::collections::BTreeMap;
    let tree: Tree = BTreeMap::new();

    let files = list_files(&tree);
    assert_eq!(files.len(), 0);
}
