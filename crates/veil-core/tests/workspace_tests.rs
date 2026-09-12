//! 工作区功能集成测试

use std::fs;
use tempfile::TempDir;
use veil_core::kdf;
use veil_core::metadata::{AlgorithmId, MetaHeader};
use veil_core::workspace_ops::WorkspaceManager;

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
