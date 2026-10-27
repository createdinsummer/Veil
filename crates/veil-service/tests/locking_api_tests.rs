//! 公开的文件锁接口与容器操作使用同一套锁。

use std::fs;
use std::time::Duration;
use tempfile::TempDir;
use veil_core::kdf;
use veil_core::locking::{FileLockManager, LockOptions};
use veil_service::veil_ops::{AddFileSpec, VeilManager};

#[test]
fn public_file_locks_coordinate_with_container_operations() {
    kdf::enable_fast_test_kdf();
    let root = TempDir::new().unwrap();
    let veil_dir = root.path().join("veil");
    let manager = VeilManager::new(veil_dir.clone());
    manager.init_veil("test", "password").unwrap();
    let source = root.path().join("source");
    fs::write(&source, b"contents").unwrap();
    manager
        .add_files(&[AddFileSpec::new(&source, "file.txt")], "password")
        .unwrap();
    let file_id = manager.list_files("password").unwrap()[0].file_id.clone();
    let options = LockOptions {
        timeout: Some(Duration::from_millis(100)),
        cancellation: None,
    };
    let locks = FileLockManager::new(&veil_dir).with_lock_options(options.clone());
    let limited = VeilManager::new(veil_dir).with_lock_options(options);
    assert!(locks.lock_file_shared("").is_err());

    let first = locks.lock_file_shared(&file_id).unwrap();
    let second = locks.lock_file_shared(&file_id).unwrap();
    assert!(locks.lock_file_exclusive(&file_id).is_err());
    assert!(
        limited
            .add_files(&[AddFileSpec::new(&source, "file.txt")], "password")
            .is_err()
    );
    first.release().unwrap();
    drop(second);

    let exclusive = locks.lock_file_exclusive(&file_id).unwrap();
    assert!(
        limited
            .extract_file("file.txt", &root.path().join("output"), "password")
            .is_err()
    );
    exclusive.release().unwrap();
    limited
        .add_files(&[AddFileSpec::new(&source, "file.txt")], "password")
        .unwrap();
    assert_eq!(
        manager.list_files("password").unwrap()[0]
            .version_history
            .len(),
        2
    );
}
