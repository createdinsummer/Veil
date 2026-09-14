//! # veil 集成测试
//!
//! 测试 `veil_core::single_file_veil::SingleFileVeil` 的公开 API，模拟真实使用场景。

use std::fs;
use std::path::PathBuf;
use veil_core::error::VeilError;
use veil_core::single_file_veil::SingleFileVeil;

/// 临时测试目录，析构时自动删除
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// 创建并清空指定名称的临时目录，同时启用低开销测试 KDF。
    fn new(name: &str) -> Self {
        veil_core::kdf::enable_fast_test_kdf();
        let path = std::env::temp_dir().join(format!("veil_test_{}", name));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    /// 返回临时目录路径。
    fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for TempDir {
    /// 删除临时目录，清理失败时忽略错误。
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// 验证创建容器后重新打开仍保留 CLI 版本。
#[test]
fn test_create_and_open_veil() {
    let temp_dir = TempDir::new("create_open");
    let veil_dir = temp_dir.path().join("test.veil");
    let password = "test_password_123";

    // 创建容器
    let veil = SingleFileVeil::create(&veil_dir, password, "test/1.0.0").unwrap();
    assert_eq!(veil.cli_version(), "test/1.0.0");
    drop(veil);

    // 重新打开
    let veil = SingleFileVeil::open(&veil_dir, password).unwrap();
    assert_eq!(veil.cli_version(), "test/1.0.0");
}

/// 验证错误密码会返回解密错误。
#[test]
fn test_open_with_wrong_password() {
    let temp_dir = TempDir::new("wrong_password");
    let veil_dir = temp_dir.path().join("test.veil");

    SingleFileVeil::create(&veil_dir, "correct_password", "test/1.0.0").unwrap();

    // 用错误密码打开
    let result = SingleFileVeil::open(&veil_dir, "wrong_password");
    assert!(result.is_err());
    match result {
        Err(VeilError::Decrypt(_)) => (),
        _ => panic!("应该返回 Decrypt 错误"),
    }
}

/// 验证添加文件后能读回相同内容。
#[test]
fn test_add_and_read_file() {
    let temp_dir = TempDir::new("add_read");
    let veil_dir = temp_dir.path().join("test.veil");

    let mut veil = SingleFileVeil::create(&veil_dir, "password", "test/1.0.0").unwrap();

    // 添加文件
    let content = b"Hello, Veil!";
    veil.add_file("test.txt", content).unwrap();

    // 读取文件
    let read_content = veil.read_file("test.txt").unwrap();
    assert_eq!(read_content, content);
}

/// 验证多个层级文件均可写入并读取。
#[test]
fn test_add_multiple_files() {
    let temp_dir = TempDir::new("multiple_files");
    let veil_dir = temp_dir.path().join("test.veil");

    let mut veil = SingleFileVeil::create(&veil_dir, "password", "test/1.0.0").unwrap();

    // 添加多个文件
    veil.add_file("file1.txt", b"content 1").unwrap();
    veil.add_file("file2.txt", b"content 2").unwrap();
    veil.add_file("dir/file3.txt", b"content 3").unwrap();

    // 验证所有文件
    assert_eq!(veil.read_file("file1.txt").unwrap(), b"content 1");
    assert_eq!(veil.read_file("file2.txt").unwrap(), b"content 2");
    assert_eq!(veil.read_file("dir/file3.txt").unwrap(), b"content 3");

    // 检查文件元信息
    let meta = veil.get_file("file1.txt").unwrap();
    assert_eq!(meta.size, 9);
}

/// 验证删除文件会移除目录树条目，重复删除会失败。
#[test]
fn test_remove_file() {
    let temp_dir = TempDir::new("remove_file");
    let veil_dir = temp_dir.path().join("test.veil");

    let mut veil = SingleFileVeil::create(&veil_dir, "password", "test/1.0.0").unwrap();

    veil.add_file("test.txt", b"content").unwrap();
    assert!(veil.get_file("test.txt").is_some());

    // 删除文件
    veil.remove_file("test.txt").unwrap();
    assert!(veil.get_file("test.txt").is_none());

    // 再次删除应该报错
    let result = veil.remove_file("test.txt");
    assert!(result.is_err());
}

/// 验证重命名后旧路径不存在且新路径内容不变。
#[test]
fn test_rename_file() {
    let temp_dir = TempDir::new("rename_file");
    let veil_dir = temp_dir.path().join("test.veil");

    let mut veil = SingleFileVeil::create(&veil_dir, "password", "test/1.0.0").unwrap();

    veil.add_file("old.txt", b"content").unwrap();

    // 重命名
    veil.rename_file("old.txt", "new.txt").unwrap();

    assert!(veil.get_file("old.txt").is_none());
    assert!(veil.get_file("new.txt").is_some());
    assert_eq!(veil.read_file("new.txt").unwrap(), b"content");
}

/// 验证修改密码后旧密码失效、新密码可读取原文件。
#[test]
fn test_change_password() {
    let temp_dir = TempDir::new("change_password");
    let veil_dir = temp_dir.path().join("test.veil");

    let old_password = "old_password";
    let new_password = "new_password";

    let mut veil = SingleFileVeil::create(&veil_dir, old_password, "test/1.0.0").unwrap();
    veil.add_file("test.txt", b"content").unwrap();

    // 修改密码
    veil.change_password(new_password).unwrap();
    drop(veil);

    // 旧密码无法打开
    assert!(SingleFileVeil::open(&veil_dir, old_password).is_err());

    // 新密码可以打开
    let veil = SingleFileVeil::open(&veil_dir, new_password).unwrap();
    assert_eq!(veil.read_file("test.txt").unwrap(), b"content");
}

/// 验证通配符在顶层、指定目录和递归匹配场景下的结果。
#[test]
fn test_find_files_with_wildcard() {
    let temp_dir = TempDir::new("find_files");
    let veil_dir = temp_dir.path().join("test.veil");

    let mut veil = SingleFileVeil::create(&veil_dir, "password", "test/1.0.0").unwrap();

    veil.add_file("image1.png", b"png1").unwrap();
    veil.add_file("image2.png", b"png2").unwrap();
    veil.add_file("doc.txt", b"txt").unwrap();
    veil.add_file("dir/image3.png", b"png3").unwrap();

    // 查找所有 png 文件（*.png 只匹配顶层）
    let mut files = veil.find_files("*.png").unwrap();
    files.sort();
    assert_eq!(files, vec!["image1.png", "image2.png"]);

    // 查找特定目录下的文件
    let mut files = veil.find_files("dir/*").unwrap();
    files.sort();
    assert_eq!(files, vec!["dir/image3.png"]);

    // 查找所有层级的 png 文件
    let mut files = veil.find_files("**/*.png").unwrap();
    files.sort();
    assert_eq!(files, vec!["dir/image3.png", "image1.png", "image2.png"]);
}

/// 验证按路径导出文件并保持原始字节。
#[test]
fn test_extract_file() {
    let temp_dir = TempDir::new("extract_file");
    let veil_dir = temp_dir.path().join("test.veil");
    let extract_path = temp_dir.path().join("extracted.txt");

    let mut veil = SingleFileVeil::create(&veil_dir, "password", "test/1.0.0").unwrap();
    veil.add_file("test.txt", b"extracted content").unwrap();

    // 提取文件
    veil.extract_file("test.txt", &extract_path).unwrap();

    // 验证提取的文件
    let content = fs::read(&extract_path).unwrap();
    assert_eq!(content, b"extracted content");
}

/// 验证跨块导出时进度回调单调递增且最终完成。
#[test]
fn test_extract_file_with_progress() {
    let temp_dir = TempDir::new("extract_progress");
    let veil_dir = temp_dir.path().join("test.veil");
    let extract_path = temp_dir.path().join("extracted.bin");

    // 内容超过一个 64KB 块，确保回调被调用多次
    let content = vec![0xABu8; 128 * 1024 + 17];

    let mut veil = SingleFileVeil::create(&veil_dir, "password", "test/1.0.0").unwrap();
    veil.add_file("big.bin", &content).unwrap();

    let mut last = 0u64;
    let mut calls = 0usize;
    veil.extract_file_with_progress("big.bin", &extract_path, |done, total| {
        assert!(done > last, "进度只增不减");
        last = done;
        assert_eq!(total, content.len() as u64);
        calls += 1;
    })
    .unwrap();

    assert_eq!(last, content.len() as u64);
    assert!(calls >= 2, "跨块导出时应多次回调");
    assert_eq!(fs::read(&extract_path).unwrap(), content);
}

/// 验证重新打开容器后已提交文件仍然存在。
#[test]
fn test_persistence_after_reopen() {
    let temp_dir = TempDir::new("persistence");
    let veil_dir = temp_dir.path().join("test.veil");
    let password = "password";

    // 第一次：创建并添加文件
    {
        let mut veil = SingleFileVeil::create(&veil_dir, password, "test/1.0.0").unwrap();
        veil.add_file("file1.txt", b"content 1").unwrap();
        veil.add_file("file2.txt", b"content 2").unwrap();
    }

    // 第二次：重新打开，验证数据持久化
    {
        let veil = SingleFileVeil::open(&veil_dir, password).unwrap();
        assert_eq!(veil.read_file("file1.txt").unwrap(), b"content 1");
        assert_eq!(veil.read_file("file2.txt").unwrap(), b"content 2");
    }
}

/// 验证读取不存在的文件返回格式错误。
#[test]
fn test_read_nonexistent_file() {
    let temp_dir = TempDir::new("nonexistent");
    let veil_dir = temp_dir.path().join("test.veil");

    let veil = SingleFileVeil::create(&veil_dir, "password", "test/1.0.0").unwrap();

    // 读取不存在的文件
    let result = veil.read_file("nonexistent.txt");
    assert!(result.is_err());
    match result {
        Err(VeilError::Format(_)) => (),
        _ => panic!("应该返回 Format 错误"),
    }
}
