//! # container 集成测试
//!
//! 测试 `veil_core::container::Container` 的公开 API，模拟真实使用场景。

use std::fs;
use std::path::PathBuf;
use veil_core::container::Container;
use veil_core::error::VeilError;

/// 临时测试目录，析构时自动删除
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("veil_test_{}", name));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn test_create_and_open_container() {
    let temp_dir = TempDir::new("create_open");
    let container_path = temp_dir.path().join("test.veil");
    let password = "test_password_123";

    // 创建容器
    let container = Container::create(&container_path, password, "test/1.0.0").unwrap();
    assert_eq!(container.cli_version(), "test/1.0.0");
    drop(container);

    // 重新打开
    let container = Container::open(&container_path, password).unwrap();
    assert_eq!(container.cli_version(), "test/1.0.0");
}

#[test]
fn test_open_with_wrong_password() {
    let temp_dir = TempDir::new("wrong_password");
    let container_path = temp_dir.path().join("test.veil");

    Container::create(&container_path, "correct_password", "test/1.0.0").unwrap();

    // 用错误密码打开
    let result = Container::open(&container_path, "wrong_password");
    assert!(result.is_err());
    match result {
        Err(VeilError::Decrypt(_)) => (),
        _ => panic!("应该返回 Decrypt 错误"),
    }
}

#[test]
fn test_add_and_read_file() {
    let temp_dir = TempDir::new("add_read");
    let container_path = temp_dir.path().join("test.veil");

    let mut container = Container::create(&container_path, "password", "test/1.0.0").unwrap();

    // 添加文件
    let content = b"Hello, Veil!";
    container.add_file("test.txt", content).unwrap();

    // 读取文件
    let read_content = container.read_file("test.txt").unwrap();
    assert_eq!(read_content, content);
}

#[test]
fn test_add_multiple_files() {
    let temp_dir = TempDir::new("multiple_files");
    let container_path = temp_dir.path().join("test.veil");

    let mut container = Container::create(&container_path, "password", "test/1.0.0").unwrap();

    // 添加多个文件
    container.add_file("file1.txt", b"content 1").unwrap();
    container.add_file("file2.txt", b"content 2").unwrap();
    container.add_file("dir/file3.txt", b"content 3").unwrap();

    // 验证所有文件
    assert_eq!(container.read_file("file1.txt").unwrap(), b"content 1");
    assert_eq!(container.read_file("file2.txt").unwrap(), b"content 2");
    assert_eq!(container.read_file("dir/file3.txt").unwrap(), b"content 3");

    // 检查文件元信息
    let meta = container.get_file("file1.txt").unwrap();
    assert_eq!(meta.size, 9);
}

#[test]
fn test_remove_file() {
    let temp_dir = TempDir::new("remove_file");
    let container_path = temp_dir.path().join("test.veil");

    let mut container = Container::create(&container_path, "password", "test/1.0.0").unwrap();

    container.add_file("test.txt", b"content").unwrap();
    assert!(container.get_file("test.txt").is_some());

    // 删除文件
    container.remove_file("test.txt").unwrap();
    assert!(container.get_file("test.txt").is_none());

    // 再次删除应该报错
    let result = container.remove_file("test.txt");
    assert!(result.is_err());
}

#[test]
fn test_rename_file() {
    let temp_dir = TempDir::new("rename_file");
    let container_path = temp_dir.path().join("test.veil");

    let mut container = Container::create(&container_path, "password", "test/1.0.0").unwrap();

    container.add_file("old.txt", b"content").unwrap();

    // 重命名
    container.rename_file("old.txt", "new.txt").unwrap();

    assert!(container.get_file("old.txt").is_none());
    assert!(container.get_file("new.txt").is_some());
    assert_eq!(container.read_file("new.txt").unwrap(), b"content");
}

#[test]
fn test_change_password() {
    let temp_dir = TempDir::new("change_password");
    let container_path = temp_dir.path().join("test.veil");

    let old_password = "old_password";
    let new_password = "new_password";

    let mut container = Container::create(&container_path, old_password, "test/1.0.0").unwrap();
    container.add_file("test.txt", b"content").unwrap();

    // 修改密码
    container.change_password(new_password).unwrap();
    drop(container);

    // 旧密码无法打开
    assert!(Container::open(&container_path, old_password).is_err());

    // 新密码可以打开
    let container = Container::open(&container_path, new_password).unwrap();
    assert_eq!(container.read_file("test.txt").unwrap(), b"content");
}

#[test]
fn test_find_files_with_wildcard() {
    let temp_dir = TempDir::new("find_files");
    let container_path = temp_dir.path().join("test.veil");

    let mut container = Container::create(&container_path, "password", "test/1.0.0").unwrap();

    container.add_file("image1.png", b"png1").unwrap();
    container.add_file("image2.png", b"png2").unwrap();
    container.add_file("doc.txt", b"txt").unwrap();
    container.add_file("dir/image3.png", b"png3").unwrap();

    // 查找所有 png 文件（*.png 只匹配顶层）
    let mut files = container.find_files("*.png").unwrap();
    files.sort();
    assert_eq!(files, vec!["image1.png", "image2.png"]);

    // 查找特定目录下的文件
    let mut files = container.find_files("dir/*").unwrap();
    files.sort();
    assert_eq!(files, vec!["dir/image3.png"]);

    // 查找所有层级的 png 文件
    let mut files = container.find_files("**/*.png").unwrap();
    files.sort();
    assert_eq!(files, vec!["dir/image3.png", "image1.png", "image2.png"]);
}

#[test]
fn test_extract_file() {
    let temp_dir = TempDir::new("extract_file");
    let container_path = temp_dir.path().join("test.veil");
    let extract_path = temp_dir.path().join("extracted.txt");

    let mut container = Container::create(&container_path, "password", "test/1.0.0").unwrap();
    container.add_file("test.txt", b"extracted content").unwrap();

    // 提取文件
    container.extract_file("test.txt", &extract_path).unwrap();

    // 验证提取的文件
    let content = fs::read(&extract_path).unwrap();
    assert_eq!(content, b"extracted content");
}

#[test]
fn test_persistence_after_reopen() {
    let temp_dir = TempDir::new("persistence");
    let container_path = temp_dir.path().join("test.veil");
    let password = "password";

    // 第一次：创建并添加文件
    {
        let mut container = Container::create(&container_path, password, "test/1.0.0").unwrap();
        container.add_file("file1.txt", b"content 1").unwrap();
        container.add_file("file2.txt", b"content 2").unwrap();
    }

    // 第二次：重新打开，验证数据持久化
    {
        let container = Container::open(&container_path, password).unwrap();
        assert_eq!(container.read_file("file1.txt").unwrap(), b"content 1");
        assert_eq!(container.read_file("file2.txt").unwrap(), b"content 2");
    }
}

#[test]
fn test_read_nonexistent_file() {
    let temp_dir = TempDir::new("nonexistent");
    let container_path = temp_dir.path().join("test.veil");

    let container = Container::create(&container_path, "password", "test/1.0.0").unwrap();

    // 读取不存在的文件
    let result = container.read_file("nonexistent.txt");
    assert!(result.is_err());
    match result {
        Err(VeilError::Format(_)) => (),
        _ => panic!("应该返回 Format 错误"),
    }
}
