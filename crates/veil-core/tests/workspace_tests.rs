//! 工作区功能集成测试

use std::fs;
use tempfile::TempDir;
use veil_core::kdf;
use veil_core::metadata::{AlgorithmId, MetaHeader};
use veil_core::workspace_ops::{AddFileSpec, MovedPathKind, RemovedPathKind, WorkspaceManager};

/// 验证工作区初始化、元数据读取和空文件列表。
#[test]
fn test_workspace_init_and_operations() {
    kdf::enable_fast_test_kdf();
    // 创建临时目录
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path().join("test-container");

    // 创建工作区管理器
    let manager = WorkspaceManager::new(workspace_path.clone());

    // 初始化容器
    let password = "test-password-123";
    manager
        .init_container("test-container", "default", password)
        .unwrap();

    // 验证 .veil-meta 文件存在，并记录实际的 ChaCha20-Poly1305 算法。
    let meta_path = workspace_path.join(".veil-meta");
    assert!(meta_path.exists());
    let header = MetaHeader::from_bytes(&fs::read(meta_path).unwrap()).unwrap();
    assert_eq!(header.algorithm, AlgorithmId::ChaCha20Poly1305);

    // 读取元数据
    let meta = manager.read_meta(password).unwrap();
    assert_eq!(meta.container_name, "test-container");
    assert_eq!(meta.workspace_type, "default");
    assert_eq!(meta.files.len(), 0);

    println!("✓ 容器初始化成功");
}

/// 验证添加文件后可列出并解密导出原内容。
#[test]
fn test_add_and_extract_file() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path().join("test-container");
    let manager = WorkspaceManager::new(workspace_path.clone());

    let password = "test-password-123";
    manager
        .init_container("test-container", "default", password)
        .unwrap();

    // 创建测试文件
    let test_file = temp_dir.path().join("test.txt");
    fs::write(&test_file, b"Hello, Veil!").unwrap();

    // 添加文件
    let encrypted_name = manager.add_file(&test_file, password).unwrap();
    println!("✓ 文件已加密: {}", encrypted_name);

    // 验证加密文件存在
    assert!(workspace_path.join(&encrypted_name).exists());

    // 列出文件
    let files = manager.list_files(password).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].original_name, "test.txt");
    assert_eq!(files[0].size, 12);

    // 提取文件
    let output_file = temp_dir.path().join("extracted.txt");
    manager
        .extract_file("test.txt", &output_file, password)
        .unwrap();

    // 验证内容
    let content = fs::read_to_string(&output_file).unwrap();
    assert_eq!(content, "Hello, Veil!");

    println!("✓ 文件添加和提取成功");
}

/// 验证 Unix 下新密文仅当前用户可读写。
#[cfg(unix)]
#[test]
fn test_added_ciphertext_is_private() {
    use std::os::unix::fs::PermissionsExt;

    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path().join("test-container");
    let manager = WorkspaceManager::new(workspace_path.clone());
    let password = "test-password";
    manager
        .init_container("test-container", "default", password)
        .unwrap();

    let source = temp_dir.path().join("private.txt");
    fs::write(&source, b"private").unwrap();
    let encrypted_name = manager.add_file(&source, password).unwrap();

    let mode = fs::metadata(workspace_path.join(encrypted_name))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

/// 验证错误密码无法读取工作区元数据。
#[test]
fn test_wrong_password() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path().join("test-container");
    let manager = WorkspaceManager::new(workspace_path);

    let password = "correct-password";
    manager
        .init_container("test-container", "default", password)
        .unwrap();

    // 使用错误密码应该失败
    let result = manager.read_meta("wrong-password");
    assert!(result.is_err());

    println!("✓ 错误密码被正确拒绝");
}

/// 验证删除文件会移除密文和元数据条目。
#[test]
fn test_remove_file() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path().join("test-container");
    let manager = WorkspaceManager::new(workspace_path.clone());

    let password = "test-password";
    manager
        .init_container("test-container", "default", password)
        .unwrap();

    // 添加文件
    let test_file = temp_dir.path().join("delete-me.txt");
    fs::write(&test_file, b"will be deleted").unwrap();
    let encrypted_name = manager.add_file(&test_file, password).unwrap();

    // 验证文件存在
    assert!(workspace_path.join(&encrypted_name).exists());
    assert_eq!(manager.list_files(password).unwrap().len(), 1);

    // 删除文件
    manager.remove_file("delete-me.txt", password).unwrap();

    // 验证文件已删除
    assert!(!workspace_path.join(&encrypted_name).exists());
    assert_eq!(manager.list_files(password).unwrap().len(), 0);

    println!("✓ 文件删除成功");
}

/// 验证容器内目标路径生效，重复添加同名目标会替换旧条目和旧密文。
#[test]
fn test_add_file_destination_replaces_existing() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path().join("test-container");
    let manager = WorkspaceManager::new(workspace_path.clone());
    let password = "test-password";
    manager
        .init_container("test-container", "default", password)
        .unwrap();

    let source = temp_dir.path().join("source.txt");
    fs::write(&source, b"first").unwrap();
    let first = manager
        .add_files(&[AddFileSpec::new(&source, "nested/renamed.txt")], password)
        .unwrap();

    let files = manager.list_files(password).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].original_name, "nested/renamed.txt");
    assert!(workspace_path.join(&first[0]).exists());

    fs::write(&source, b"second").unwrap();
    let second = manager
        .add_files(&[AddFileSpec::new(&source, "nested/renamed.txt")], password)
        .unwrap();

    let files = manager.list_files(password).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].original_name, "nested/renamed.txt");
    assert_eq!(files[0].size, 6);
    assert!(!workspace_path.join(&first[0]).exists());
    assert!(workspace_path.join(&second[0]).exists());

    let output = temp_dir.path().join("output.txt");
    manager
        .extract_file("nested/renamed.txt", &output, password)
        .unwrap();
    assert_eq!(fs::read_to_string(output).unwrap(), "second");
}

/// 验证错误密码在任何密文写入前失败。
#[test]
fn test_add_files_wrong_password_does_not_write_ciphertext() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path().join("test-container");
    let manager = WorkspaceManager::new(workspace_path.clone());
    let password = "correct-password";
    manager
        .init_container("test-container", "default", password)
        .unwrap();

    let source = temp_dir.path().join("source.txt");
    fs::write(&source, b"secret").unwrap();

    let before = encrypted_file_count(&workspace_path);
    let result = manager.add_files(&[AddFileSpec::new(&source, "source.txt")], "wrong-password");

    assert!(result.is_err());
    assert_eq!(encrypted_file_count(&workspace_path), before);
    assert!(manager.list_files(password).unwrap().is_empty());
}

/// 验证目录递归删除和 `./` 路径规范化。
#[test]
fn test_remove_directory_recursively() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path().join("test-container");
    let manager = WorkspaceManager::new(workspace_path.clone());
    let password = "test-password";
    manager
        .init_container("test-container", "default", password)
        .unwrap();

    let top = temp_dir.path().join("top.txt");
    let deep = temp_dir.path().join("deep.txt");
    let keep = temp_dir.path().join("keep.txt");
    fs::write(&top, b"top").unwrap();
    fs::write(&deep, b"deep").unwrap();
    fs::write(&keep, b"keep").unwrap();

    manager
        .add_files(
            &[
                AddFileSpec::new(&top, "tree/top.txt"),
                AddFileSpec::new(&deep, "tree/sub/deep.txt"),
                AddFileSpec::new(&keep, "keep.txt"),
            ],
            password,
        )
        .unwrap();

    let removed = manager.remove_path("./tree", password).unwrap();
    assert_eq!(removed, RemovedPathKind::Directory);

    let files = manager.list_files(password).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].original_name, "keep.txt");
    assert_eq!(encrypted_file_count(&workspace_path), 1);
}

/// 验证文件重命名和移动到已有目录。
#[test]
fn test_move_file_into_directory_and_rename() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path().join("test-container");
    let manager = WorkspaceManager::new(workspace_path);
    let password = "test-password";
    manager
        .init_container("test-container", "default", password)
        .unwrap();

    let source = temp_dir.path().join("source.txt");
    let existing = temp_dir.path().join("existing.txt");
    fs::write(&source, b"source").unwrap();
    fs::write(&existing, b"existing").unwrap();
    manager
        .add_files(
            &[
                AddFileSpec::new(&source, "source.txt"),
                AddFileSpec::new(&existing, "target/existing.txt"),
            ],
            password,
        )
        .unwrap();

    let (kind, target) = manager.move_path("source.txt", "target", password).unwrap();
    assert_eq!(kind, MovedPathKind::File);
    assert_eq!(target, "target/source.txt");
    assert!(
        manager
            .list_files(password)
            .unwrap()
            .iter()
            .any(|file| { file.original_name == "target/source.txt" })
    );

    let (_, renamed) = manager
        .move_path("target/source.txt", "./renamed.txt", password)
        .unwrap();
    assert_eq!(renamed, "renamed.txt");
    assert!(
        manager
            .list_files(password)
            .unwrap()
            .iter()
            .any(|file| { file.original_name == "renamed.txt" })
    );
}

/// 验证目录整体移动、子路径更新和循环移动保护。
#[test]
fn test_move_directory_recursively() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path().join("test-container");
    let manager = WorkspaceManager::new(workspace_path);
    let password = "test-password";
    manager
        .init_container("test-container", "default", password)
        .unwrap();

    let top = temp_dir.path().join("top.txt");
    let deep = temp_dir.path().join("deep.txt");
    let keep = temp_dir.path().join("keep.txt");
    fs::write(&top, b"top").unwrap();
    fs::write(&deep, b"deep").unwrap();
    fs::write(&keep, b"keep").unwrap();
    manager
        .add_files(
            &[
                AddFileSpec::new(&top, "source/top.txt"),
                AddFileSpec::new(&deep, "source/sub/deep.txt"),
                AddFileSpec::new(&keep, "target/keep.txt"),
            ],
            password,
        )
        .unwrap();

    let cycle = manager.move_path("source", "source/sub/moved", password);
    assert!(cycle.is_err());

    let (kind, target) = manager.move_path("source", "target", password).unwrap();
    assert_eq!(kind, MovedPathKind::Directory);
    assert_eq!(target, "target/source");

    let files = manager.list_files(password).unwrap();
    assert!(
        files
            .iter()
            .any(|file| { file.original_name == "target/source/top.txt" })
    );
    assert!(
        files
            .iter()
            .any(|file| { file.original_name == "target/source/sub/deep.txt" })
    );
    assert!(
        !files
            .iter()
            .any(|file| { file.original_name.starts_with("source/") })
    );
}

/// 验证密码修改在中途失败时保留旧密码可读状态。
#[test]
fn test_change_password_failure_rolls_back() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let workspace_path = temp_dir.path().join("test-container");
    let manager = WorkspaceManager::new(workspace_path.clone());
    let old_password = "old-password";
    manager
        .init_container("test-container", "default", old_password)
        .unwrap();

    let first = temp_dir.path().join("first.txt");
    let second = temp_dir.path().join("second.txt");
    fs::write(&first, b"first").unwrap();
    fs::write(&second, b"second").unwrap();
    manager
        .add_files(
            &[
                AddFileSpec::new(&first, "first.txt"),
                AddFileSpec::new(&second, "second.txt"),
            ],
            old_password,
        )
        .unwrap();

    let missing = manager
        .list_files(old_password)
        .unwrap()
        .into_iter()
        .find(|file| file.original_name == "second.txt")
        .unwrap();
    fs::remove_file(workspace_path.join(missing.encrypted_name)).unwrap();

    let result = manager.change_password(old_password, "new-password");
    assert!(result.is_err());
    assert!(manager.read_meta(old_password).is_ok());
    assert!(manager.read_meta("new-password").is_err());

    let files = manager.list_files(old_password).unwrap();
    assert_eq!(files.len(), 2);
    assert!(
        !fs::read_dir(&workspace_path)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().starts_with(".veil-pw-"))
    );
}

/// 统计工作区根目录下的密文文件数量。
fn encrypted_file_count(workspace_path: &std::path::Path) -> usize {
    fs::read_dir(workspace_path)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                == Some("enc")
        })
        .count()
}
