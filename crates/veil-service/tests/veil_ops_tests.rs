//! 工作区功能集成测试

use std::fs;
use tempfile::TempDir;
use veil_core::kdf;

use veil_service::veil_ops::{AddFileSpec, MovedPathKind, RemovedPathKind, VeilManager};

/// 验证工作区初始化、元数据读取和空文件列表。
#[test]
fn test_work_init_and_operations() {
    kdf::enable_fast_test_kdf();
    // 创建临时目录
    let temp_dir = TempDir::new().unwrap();
    let veil_dir = temp_dir.path().join("test-veil");

    // 创建工作区管理器
    let manager = VeilManager::new(veil_dir.clone());

    // 初始化容器
    let password = "test-password-123";
    manager.init_veil("test-veil", password).unwrap();

    // 验证 .veil-info 文件存在，并记录实际的 ChaCha20-Poly1305 算法。
    let meta_path = veil_dir.join("veil.db");
    assert!(meta_path.exists());
    assert_ne!(&fs::read(meta_path).unwrap()[..16], b"SQLite format 3\0");

    // 读取元数据
    let meta = manager.read_meta(password).unwrap();
    assert_eq!(meta.veil_name, "test-veil");

    assert_eq!(meta.files.len(), 0);
    assert_eq!(meta.data_keys.len(), 1);

    println!("✓ 容器初始化成功");
}

/// 验证添加文件后可列出并解密导出原内容。
#[test]
fn test_add_and_extract_file() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let veil_dir = temp_dir.path().join("test-veil");
    let manager = VeilManager::new(veil_dir.clone());

    let password = "test-password-123";
    manager.init_veil("test-veil", password).unwrap();

    // 创建测试文件
    let test_file = temp_dir.path().join("test.txt");
    fs::write(&test_file, b"Hello, Veil!").unwrap();

    // 添加文件
    let encrypted_name = manager.add_file(&test_file, password).unwrap();
    println!("✓ 文件已加密: {}", encrypted_name);

    // 验证加密文件存在
    assert!(veil_dir.join(&encrypted_name).exists());

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

/// 两个基于同一版本的编辑，先提交者成功，后提交者不能覆盖。
#[test]
fn stale_export_cannot_overwrite_a_newer_version() {
    kdf::enable_fast_test_kdf();
    let root = TempDir::new().unwrap();
    let manager = VeilManager::new(root.path().join("veil"));
    let password = "password";
    manager.init_veil("test", password).unwrap();
    let source = root.path().join("source");
    fs::write(&source, b"original").unwrap();
    manager
        .add_files(&[AddFileSpec::new(&source, "file.txt")], password)
        .unwrap();
    let a = root.path().join("a");
    let b = root.path().join("b");
    let base_a = manager
        .extract_file_with_version("file.txt", &a, password)
        .unwrap();
    let base_b = manager
        .extract_file_with_version("file.txt", &b, password)
        .unwrap();
    assert_eq!(base_a, base_b);
    fs::write(&a, b"edit A").unwrap();
    fs::write(&b, b"edit B").unwrap();
    manager
        .add_files(
            &[AddFileSpec::new(&a, "file.txt").with_base_version(base_a)],
            password,
        )
        .unwrap();
    let before = manager.list_files(password).unwrap();
    let error = manager
        .add_files(
            &[AddFileSpec::new(&b, "file.txt").with_base_version(base_b.clone())],
            password,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        veil_core::error::VeilError::VersionConflict(_)
    ));
    let after = manager.list_files(password).unwrap();
    assert_eq!(after[0].version_history, before[0].version_history);
    assert_eq!(after[0].encrypted_name, before[0].encrypted_name);
    let output = root.path().join("result");
    manager.extract_file("file.txt", &output, password).unwrap();
    assert_eq!(fs::read(output).unwrap(), b"edit A");
    let mut wrong_identity = base_b;
    wrong_identity.veil_id = "wrong".into();
    assert!(matches!(
        manager.add_files(
            &[AddFileSpec::new(&b, "file.txt").with_base_version(wrong_identity)],
            password
        ),
        Err(veil_core::error::VeilError::VersionConflict(_))
    ));
}

/// 同一基础版本的并发提交必须恰有一个成功。
#[test]
fn concurrent_checked_updates_allow_only_one_writer() {
    use std::sync::{Arc, Barrier};
    kdf::enable_fast_test_kdf();
    let root = TempDir::new().unwrap();
    let veil_dir = root.path().join("veil");
    let manager = VeilManager::new(veil_dir.clone());
    manager.init_veil("test", "password").unwrap();
    let original = root.path().join("original");
    fs::write(&original, b"original").unwrap();
    manager
        .add_files(&[AddFileSpec::new(&original, "file.txt")], "password")
        .unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    let first_base = manager
        .extract_file_with_version("file.txt", &first, "password")
        .unwrap();
    let second_base = manager
        .extract_file_with_version("file.txt", &second, "password")
        .unwrap();
    fs::write(&first, b"first change").unwrap();
    fs::write(&second, b"second change").unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let mut threads = Vec::new();
    for (source, base) in [(first, first_base), (second, second_base)] {
        let directory = veil_dir.clone();
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            let manager = VeilManager::new(directory);
            barrier.wait();
            manager.add_files(
                &[AddFileSpec::new(source, "file.txt").with_base_version(base)],
                "password",
            )
        }));
    }
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(
        results.iter().filter(|result| result.is_ok()).count(),
        1,
        "{results:?}"
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(veil_core::error::VeilError::VersionConflict(_))))
            .count(),
        1,
        "{results:?}"
    );
    let files = manager.list_files("password").unwrap();
    assert_eq!(files[0].version_history.len(), 2);
    let output = root.path().join("output");
    manager
        .extract_file("file.txt", &output, "password")
        .unwrap();
    let contents = fs::read(output).unwrap();
    assert!(contents == b"first change" || contents == b"second change");
}

/// 验证 Unix 下新密文仅当前用户可读写。
#[cfg(unix)]
#[test]
fn test_added_ciphertext_is_private() {
    use std::os::unix::fs::PermissionsExt;

    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let veil_dir = temp_dir.path().join("test-veil");
    let manager = VeilManager::new(veil_dir.clone());
    let password = "test-password";
    manager.init_veil("test-veil", password).unwrap();

    let source = temp_dir.path().join("private.txt");
    fs::write(&source, b"private").unwrap();
    let encrypted_name = manager.add_file(&source, password).unwrap();

    let mode = fs::metadata(veil_dir.join(encrypted_name))
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
    let veil_dir = temp_dir.path().join("test-veil");
    let manager = VeilManager::new(veil_dir);

    let password = "correct-password";
    manager.init_veil("test-veil", password).unwrap();

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
    let veil_dir = temp_dir.path().join("test-veil");
    let manager = VeilManager::new(veil_dir.clone());

    let password = "test-password";
    manager.init_veil("test-veil", password).unwrap();

    // 添加文件
    let test_file = temp_dir.path().join("delete-me.txt");
    fs::write(&test_file, b"will be deleted").unwrap();
    let encrypted_name = manager.add_file(&test_file, password).unwrap();

    // 验证文件存在
    assert!(veil_dir.join(&encrypted_name).exists());
    assert_eq!(manager.list_files(password).unwrap().len(), 1);

    // 删除文件
    manager.remove_file("delete-me.txt", password).unwrap();

    // 验证文件已删除
    assert!(!veil_dir.join(&encrypted_name).exists());
    assert_eq!(manager.list_files(password).unwrap().len(), 0);

    println!("✓ 文件删除成功");
}

/// 验证容器内目标路径生效，重复添加同名目标会替换旧条目和旧密文。
#[test]
fn test_add_file_destination_replaces_existing() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let veil_dir = temp_dir.path().join("test-veil");
    let manager = VeilManager::new(veil_dir.clone());
    let password = "test-password";
    manager.init_veil("test-veil", password).unwrap();

    let source = temp_dir.path().join("source.txt");
    fs::write(&source, b"first").unwrap();
    let first = manager
        .add_files(&[AddFileSpec::new(&source, "nested/renamed.txt")], password)
        .unwrap();

    let files = manager.list_files(password).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].original_name, "nested/renamed.txt");
    let original_file_id = files[0].file_id.clone();
    let initial_history = files[0].version_history.clone();
    assert_eq!(initial_history.len(), 1);
    assert!(veil_dir.join(&first[0]).exists());

    fs::write(&source, b"second").unwrap();
    let second = manager
        .add_files(&[AddFileSpec::new(&source, "nested/renamed.txt")], password)
        .unwrap();

    let files = manager.list_files(password).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].original_name, "nested/renamed.txt");
    assert_eq!(files[0].size, 6);
    assert_eq!(files[0].file_id, original_file_id);
    assert_eq!(files[0].version_history.len(), 2);
    assert_eq!(files[0].version_history[0], initial_history[0]);
    assert_ne!(files[0].version_history[0], files[0].version_history[1]);
    assert!(!veil_dir.join(&first[0]).exists());
    assert!(veil_dir.join(&second[0]).exists());

    let output = temp_dir.path().join("output.txt");
    manager
        .extract_file("nested/renamed.txt", &output, password)
        .unwrap();
    assert_eq!(fs::read_to_string(output).unwrap(), "second");
}

/// 重命名、失败的覆盖和完整改密均不产生内容版本。
#[test]
fn test_file_version_survives_non_content_changes() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let manager = VeilManager::new(temp_dir.path().join("veil"));
    let password = "test-password";
    manager.init_veil("test", password).unwrap();
    let source = temp_dir.path().join("source.txt");
    fs::write(&source, b"content").unwrap();
    manager.add_file(&source, password).unwrap();
    let history = manager.list_files(password).unwrap()[0]
        .version_history
        .clone();

    manager
        .rename_file("source.txt", "renamed.txt", password)
        .unwrap();
    assert_eq!(
        manager.list_files(password).unwrap()[0].version_history,
        history
    );
    let missing = temp_dir.path().join("missing.txt");
    assert!(
        manager
            .add_files(&[AddFileSpec::new(&missing, "renamed.txt")], password)
            .is_err()
    );
    assert_eq!(
        manager.list_files(password).unwrap()[0].version_history,
        history
    );
    manager
        .change_password_full(password, "new-password")
        .unwrap();
    assert_eq!(
        manager.list_files("new-password").unwrap()[0].version_history,
        history
    );
}

/// 验证错误密码在任何密文写入前失败。
#[test]
fn test_add_files_wrong_password_does_not_write_ciphertext() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let veil_dir = temp_dir.path().join("test-veil");
    let manager = VeilManager::new(veil_dir.clone());
    let password = "correct-password";
    manager.init_veil("test-veil", password).unwrap();

    let source = temp_dir.path().join("source.txt");
    fs::write(&source, b"secret").unwrap();

    let before = encrypted_file_count(&veil_dir);
    let result = manager.add_files(&[AddFileSpec::new(&source, "source.txt")], "wrong-password");

    assert!(result.is_err());
    assert_eq!(encrypted_file_count(&veil_dir), before);
    assert!(manager.list_files(password).unwrap().is_empty());
}

/// 验证目录递归删除和 `./` 路径规范化。
#[test]
fn test_remove_directory_recursively() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let veil_dir = temp_dir.path().join("test-veil");
    let manager = VeilManager::new(veil_dir.clone());
    let password = "test-password";
    manager.init_veil("test-veil", password).unwrap();

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
    assert_eq!(encrypted_file_count(&veil_dir), 1);
}

/// 验证文件重命名和移动到已有目录。
#[test]
fn test_move_file_into_directory_and_rename() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let veil_dir = temp_dir.path().join("test-veil");
    let manager = VeilManager::new(veil_dir);
    let password = "test-password";
    manager.init_veil("test-veil", password).unwrap();

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
    let original_file_id = manager
        .list_files(password)
        .unwrap()
        .into_iter()
        .find(|file| file.original_name == "source.txt")
        .unwrap()
        .file_id;

    let (kind, target) = manager.move_path("source.txt", "target", password).unwrap();
    assert_eq!(kind, MovedPathKind::File);
    assert_eq!(target, "target/source.txt");
    let moved = manager.list_files(password).unwrap();
    let moved_file = moved
        .iter()
        .find(|file| file.original_name == "target/source.txt")
        .unwrap();
    assert_eq!(moved_file.file_id, original_file_id);

    let (_, renamed) = manager
        .move_path("target/source.txt", "./renamed.txt", password)
        .unwrap();
    assert_eq!(renamed, "renamed.txt");
    let renamed_file = manager
        .list_files(password)
        .unwrap()
        .into_iter()
        .find(|file| file.original_name == "renamed.txt")
        .unwrap();
    assert_eq!(renamed_file.file_id, original_file_id);
}

/// 验证目录整体移动、子路径更新和循环移动保护。
#[test]
fn test_move_directory_recursively() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let veil_dir = temp_dir.path().join("test-veil");
    let manager = VeilManager::new(veil_dir);
    let password = "test-password";
    manager.init_veil("test-veil", password).unwrap();

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

/// 验证完整改密中途失败时保留旧密码和双密钥状态。
#[test]
fn test_change_password_full_failure_keeps_old_password() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let veil_dir = temp_dir.path().join("test-veil");
    let manager = VeilManager::new(veil_dir.clone());
    let old_password = "old-password";
    manager.init_veil("test-veil", old_password).unwrap();

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
    fs::remove_file(veil_dir.join(missing.encrypted_name)).unwrap();

    let result = manager.change_password_full(old_password, "new-password");
    assert!(result.is_err());
    assert!(manager.read_meta(old_password).is_ok());
    assert!(manager.read_meta("new-password").is_err());

    let files = manager.list_files(old_password).unwrap();
    assert_eq!(files.len(), 2);
    assert!(
        !fs::read_dir(&veil_dir)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().starts_with(".veil-pw-"))
    );
}

/// 验证完整改密中断后可以继续使用旧密码恢复迁移。
#[test]
fn test_change_password_full_can_resume_after_partial_migration() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let veil_dir = temp_dir.path().join("test-veil");
    let manager = VeilManager::new(veil_dir.clone());
    let old_password = "old-password";
    let new_password = "new-password";

    manager.init_veil("test-veil", old_password).unwrap();
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

    let second_entry = manager
        .list_files(old_password)
        .unwrap()
        .into_iter()
        .find(|entry| entry.original_name == "second.txt")
        .unwrap();
    let second_path = veil_dir.join(&second_entry.encrypted_name);
    let second_ciphertext = fs::read(&second_path).unwrap();
    fs::remove_file(&second_path).unwrap();

    assert!(
        manager
            .change_password_full(old_password, new_password)
            .is_err()
    );
    assert!(manager.read_meta(old_password).is_ok());
    assert!(manager.read_meta(new_password).is_err());

    fs::write(&second_path, second_ciphertext).unwrap();
    manager
        .change_password_full(old_password, new_password)
        .unwrap();

    let first_output = temp_dir.path().join("first-out.txt");
    let second_output = temp_dir.path().join("second-out.txt");
    manager
        .extract_file("first.txt", &first_output, new_password)
        .unwrap();
    manager
        .extract_file("second.txt", &second_output, new_password)
        .unwrap();
    assert_eq!(fs::read(first_output).unwrap(), b"first");
    assert_eq!(fs::read(second_output).unwrap(), b"second");
}

#[test]
fn database_rekey_uses_new_password_without_rewriting_files() {
    let temp = TempDir::new().unwrap();
    let m = VeilManager::new(temp.path().join("veil"));
    m.init_veil("test", "old").unwrap();
    let before = fs::read(m.veil_dir.join("veil.db")).unwrap();
    m.change_password("old", "new").unwrap();
    assert!(m.read_meta("old").is_err());
    assert!(m.read_meta("new").is_ok());
    assert_ne!(before, fs::read(m.veil_dir.join("veil.db")).unwrap());
}

/// 验证崩溃遗留的临时文件和孤立密文会在下次成功读取时清理。
#[test]
fn test_crash_orphans_are_cleaned_after_successful_read() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let veil_dir = temp_dir.path().join("test-veil");
    let manager = VeilManager::new(veil_dir.clone());
    let password = "test-password";

    manager.init_veil("test-veil", password).unwrap();
    let source = temp_dir.path().join("keep.txt");
    fs::write(&source, b"keep").unwrap();
    let encrypted_name = manager.add_file(&source, password).unwrap();

    let orphan = veil_dir.join(format!("{}.enc", "a".repeat(32)));
    let temp = veil_dir.join(".veil-tmp-crashed");
    fs::write(&orphan, b"orphan").unwrap();
    fs::write(&temp, b"partial").unwrap();

    manager.read_meta(password).unwrap();

    assert!(veil_dir.join(encrypted_name).exists());
    assert!(orphan.exists());
    assert!(temp.exists());
}

/// 验证默认改密只重新保护元数据，不修改现有密文文件。
#[test]
fn test_change_password_metadata_only_keeps_ciphertext() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let veil_dir = temp_dir.path().join("test-veil");
    let manager = VeilManager::new(veil_dir.clone());
    let old_password = "old-password";
    let new_password = "new-password";

    manager.init_veil("test-veil", old_password).unwrap();
    let source = temp_dir.path().join("data.txt");
    fs::write(&source, b"secret").unwrap();
    manager
        .add_files(&[AddFileSpec::new(&source, "data.txt")], old_password)
        .unwrap();

    let old_entries = manager.list_files(old_password).unwrap();
    let old_names: std::collections::HashSet<_> = old_entries
        .iter()
        .map(|entry| entry.encrypted_name.clone())
        .collect();
    let old_file_ids: std::collections::HashSet<_> = old_entries
        .iter()
        .map(|entry| entry.file_id.clone())
        .collect();
    let old_ciphertexts: Vec<_> = old_names
        .iter()
        .map(|name| fs::read(veil_dir.join(name)).unwrap())
        .collect();

    manager.change_password(old_password, new_password).unwrap();

    let new_entries = manager.list_files(new_password).unwrap();
    let new_names: std::collections::HashSet<_> = new_entries
        .iter()
        .map(|entry| entry.encrypted_name.clone())
        .collect();
    let new_file_ids: std::collections::HashSet<_> = new_entries
        .iter()
        .map(|entry| entry.file_id.clone())
        .collect();
    assert_eq!(old_names, new_names);
    assert_eq!(old_file_ids, new_file_ids);
    assert_eq!(
        old_ciphertexts,
        new_names
            .iter()
            .map(|name| fs::read(veil_dir.join(name)).unwrap())
            .collect::<Vec<_>>()
    );
    assert!(new_names.iter().all(|name| veil_dir.join(name).exists()));
    assert!(manager.read_meta(old_password).is_err());

    let output = temp_dir.path().join("output.txt");
    manager
        .extract_file("data.txt", &output, new_password)
        .unwrap();
    assert_eq!(fs::read(output).unwrap(), b"secret");
}

/// 验证完整改密会轮换数据主密钥并替换全部文件密文。
#[test]
fn test_change_password_full_reencrypts_ciphertext() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let veil_dir = temp_dir.path().join("test-veil");
    let manager = VeilManager::new(veil_dir.clone());
    let old_password = "old-password";
    let new_password = "new-password";

    manager.init_veil("test-veil", old_password).unwrap();
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

    let old_entries = manager.list_files(old_password).unwrap();
    let old_names: std::collections::HashSet<_> = old_entries
        .iter()
        .map(|entry| entry.encrypted_name.clone())
        .collect();
    let old_file_ids: std::collections::HashSet<_> = old_entries
        .iter()
        .map(|entry| entry.file_id.clone())
        .collect();

    manager
        .change_password_full(old_password, new_password)
        .unwrap();

    let new_entries = manager.list_files(new_password).unwrap();
    let new_names: std::collections::HashSet<_> = new_entries
        .iter()
        .map(|entry| entry.encrypted_name.clone())
        .collect();
    let new_file_ids: std::collections::HashSet<_> = new_entries
        .iter()
        .map(|entry| entry.file_id.clone())
        .collect();
    assert!(old_names.is_disjoint(&new_names));
    assert_eq!(old_file_ids, new_file_ids);
    assert!(old_names.iter().all(|name| !veil_dir.join(name).exists()));
    assert!(manager.read_meta(old_password).is_err());

    let output = temp_dir.path().join("first-out.txt");
    manager
        .extract_file("first.txt", &output, new_password)
        .unwrap();
    assert_eq!(fs::read(output).unwrap(), b"first");
}

/// 验证工作区对跨多个分块的大文件执行添加、完整改密和导出。
#[test]
fn test_work_streaming_multichunk_full_rekey() {
    kdf::enable_fast_test_kdf();
    let temp_dir = TempDir::new().unwrap();
    let veil_dir = temp_dir.path().join("test-veil");
    let manager = VeilManager::new(veil_dir);
    let old_password = "old-password";
    let new_password = "new-password";
    let plaintext: Vec<u8> = (0..veil_core::file_ops::CHUNK_SIZE * 2 + 123)
        .map(|index| (index % 251) as u8)
        .collect();

    manager.init_veil("test-veil", old_password).unwrap();
    let source = temp_dir.path().join("large.bin");
    fs::write(&source, &plaintext).unwrap();
    manager
        .add_files(&[AddFileSpec::new(&source, "large.bin")], old_password)
        .unwrap();

    manager
        .change_password_full(old_password, new_password)
        .unwrap();

    let output = temp_dir.path().join("large-out.bin");
    manager
        .extract_file("large.bin", &output, new_password)
        .unwrap();
    assert_eq!(fs::read(output).unwrap(), plaintext);
}

/// 统计工作区根目录下的密文文件数量。
fn encrypted_file_count(veil_dir: &std::path::Path) -> usize {
    fs::read_dir(veil_dir)
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
