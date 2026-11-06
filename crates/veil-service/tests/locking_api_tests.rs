//! 公开文件租约与容器操作共用数据库协议。
use std::time::Duration;
use tempfile::TempDir;
use veil_service::{
    ExecutionService,
    storage::locks::{LockManager, LockMode, LockOptions},
    veil_ops::{AddFileSpec, VeilManager},
};

#[test]
fn container_lock_covers_every_scope_and_ro_has_no_lease() {
    let root = TempDir::new().unwrap();
    let veil = VeilManager::new(root.path().join("veil"));
    veil.init_veil("test", "password").unwrap();
    veil.create_directory("dir", "password").unwrap();
    let db = veil_service::storage::container::ContainerDb::open(&veil.veil_dir, "password", None)
        .unwrap();
    let directory = db.resolve("dir").unwrap().unwrap().id;
    drop(db);
    let locks = LockManager::new(&veil.veil_dir, "password").with_options(LockOptions {
        timeout: Some(Duration::from_millis(200)),
        cancellation: None,
    });
    let read = locks
        .acquire_directory(&directory, LockMode::ReadOnly)
        .unwrap();
    let exclusive = locks.acquire_container(LockMode::Write).unwrap();
    let error = match locks.acquire_directory(&directory, LockMode::ReadOnly) {
        Ok(_) => panic!("排他容器锁应阻止新的读取"),
        Err(error) => error,
    };
    assert!(
        matches!(error, veil_core::error::VeilError::LockConflict { ref level, .. } if level == "EW")
    );
    drop(exclusive);
    drop(read);
}

#[test]
fn shared_write_allows_ro_but_blocks_protected_readers_and_writers() {
    let root = TempDir::new().unwrap();
    let veil = VeilManager::new(root.path().join("veil")).with_lock_options(LockOptions {
        timeout: Some(Duration::from_millis(250)),
        cancellation: None,
    });
    veil.init_veil("test", "password").unwrap();
    veil.create_directory("dir", "password").unwrap();
    let source = root.path().join("source");
    std::fs::write(&source, b"content").unwrap();
    veil.add_files(&[AddFileSpec::new(&source, "dir/file")], "password")
        .unwrap();
    let db = veil_service::storage::container::ContainerDb::open(&veil.veil_dir, "password", None)
        .unwrap();
    let directory = db.resolve("dir").unwrap().unwrap().id;
    let file = db.resolve("dir/file").unwrap().unwrap().id;
    drop(db);
    let locks = LockManager::new(&veil.veil_dir, "password").with_options(LockOptions {
        timeout: Some(Duration::from_millis(250)),
        cancellation: None,
    });
    let direct = locks.acquire(&[file.clone()], LockMode::ReadOnly).unwrap();
    assert!(direct.credentials().unwrap()[0].is_read_only());
    let read = locks.acquire(&[file.clone()], LockMode::Read).unwrap();
    assert!(!read.credentials().unwrap()[0].is_read_only());
    assert!(
        locks
            .acquire_directory(&directory, LockMode::SharedWrite)
            .is_err()
    );
    drop(read);
    let write = locks
        .acquire_directory(&directory, LockMode::SharedWrite)
        .unwrap();
    assert!(locks.acquire(&[file.clone()], LockMode::Read).is_err());
    veil.add_files_with_shared_directory_lock(
        &[AddFileSpec::new(&source, "dir/new")],
        "password",
        "dir",
        &write,
    )
    .unwrap();
    assert!(
        veil.add_files_with_shared_directory_lock(
            &[AddFileSpec::new(&source, "outside")],
            "password",
            "dir",
            &write
        )
        .is_err()
    );
    assert_eq!(
        veil.read_file_bytes("dir/file", "password", 100).unwrap(),
        b"content"
    );
    let (bytes, unstable) = veil
        .read_file_bytes_with_status("dir/file", "password", 100)
        .unwrap();
    assert_eq!(bytes, b"content");
    assert!(unstable);
    assert!(
        veil.extract_file_without_overwrite_with_status(
            "dir/file",
            &root.path().join("export"),
            "password"
        )
        .is_err()
    );
    assert!(
        veil.matches_existing_file("dir/file", &source, "password")
            .is_err()
    );
    assert!(
        veil.snapshot_to(&root.path().join("backup"), "password")
            .is_err()
    );
    assert!(
        locks
            .acquire(&[file.clone()], LockMode::SharedWrite)
            .is_err()
    );
    assert!(locks.acquire(&[file.clone()], LockMode::Write).is_err());
    drop(write);
    let file_write = locks
        .acquire(&[file.clone()], LockMode::SharedWrite)
        .unwrap();
    std::fs::write(&source, b"changed").unwrap();
    veil.add_file_with_shared_lock(
        &AddFileSpec::new(&source, "dir/file"),
        "password",
        &file_write,
    )
    .unwrap();
    assert_eq!(
        veil.read_file_bytes("dir/file", "password", 100).unwrap(),
        b"changed"
    );
    drop(file_write);
    let exclusive = locks.acquire(&[file.clone()], LockMode::Write).unwrap();
    assert!(locks.acquire(&[file.clone()], LockMode::ReadOnly).is_err());
    assert!(locks.acquire(&[file], LockMode::Read).is_err());
    drop(exclusive);
    drop(direct);
}

#[test]
fn shared_write_cross_process_worker() {
    let Some(path) = std::env::var_os("VEIL_SW_TEST_PATH") else {
        return;
    };
    let manager = VeilManager::new(path.into());
    let (bytes, unstable) = manager
        .read_file_bytes_with_status("dir/file", "password", 100)
        .unwrap();
    assert_eq!(bytes, b"content");
    assert!(unstable);
}

#[test]
fn shared_write_downgrade_works_across_processes() {
    let root = TempDir::new().unwrap();
    let manager = VeilManager::new(root.path().join("veil"));
    manager.init_veil("test", "password").unwrap();
    manager.create_directory("dir", "password").unwrap();
    let source = root.path().join("source");
    std::fs::write(&source, b"content").unwrap();
    manager
        .add_files(&[AddFileSpec::new(&source, "dir/file")], "password")
        .unwrap();
    let db =
        veil_service::storage::container::ContainerDb::open(&manager.veil_dir, "password", None)
            .unwrap();
    let directory = db.resolve("dir").unwrap().unwrap().id;
    drop(db);
    let guard = LockManager::new(&manager.veil_dir, "password")
        .acquire_directory(&directory, LockMode::SharedWrite)
        .unwrap();
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "shared_write_cross_process_worker",
            "--nocapture",
        ])
        .env("VEIL_SW_TEST_PATH", &manager.veil_dir)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    drop(guard);
}

#[test]
fn directory_ranges_share_reads_and_exclude_overlapping_writes() {
    let root = TempDir::new().unwrap();
    let m = VeilManager::new(root.path().join("veil"));
    m.init_veil("test", "password").unwrap();
    m.create_directory("photos/2026", "password").unwrap();
    m.create_directory("documents", "password").unwrap();
    let source = root.path().join("source");
    std::fs::write(&source, b"content").unwrap();
    m.add_files(&[AddFileSpec::new(&source, "photos/2026/a")], "password")
        .unwrap();
    let db =
        veil_service::storage::container::ContainerDb::open(&m.veil_dir, "password", None).unwrap();
    let photos = db.resolve("photos").unwrap().unwrap().id;
    let child = db.resolve("photos/2026").unwrap().unwrap().id;
    let documents = db.resolve("documents").unwrap().unwrap().id;
    let file = db.resolve("photos/2026/a").unwrap().unwrap().id;
    drop(db);
    let manager = LockManager::new(&m.veil_dir, "password").with_options(LockOptions {
        timeout: Some(Duration::from_millis(350)),
        cancellation: None,
    });
    let directory_read = manager.acquire_directory(&photos, LockMode::Read).unwrap();
    let file_read = manager.acquire(&[file.clone()], LockMode::Read).unwrap();
    assert!(manager.acquire(&[file.clone()], LockMode::Write).is_err());
    assert!(manager.acquire_directory(&child, LockMode::Write).is_err());
    let independent = manager
        .acquire_directory(&documents, LockMode::Write)
        .unwrap();
    drop(independent);
    drop(file_read);
    drop(directory_read);
    let directory_write = manager.acquire_directory(&photos, LockMode::Write).unwrap();
    assert!(manager.acquire(&[file], LockMode::Read).is_err());
    assert!(manager.acquire_directory(&child, LockMode::Read).is_err());
    drop(directory_write);
}

#[test]
fn directory_lock_blocks_mutations_and_allows_unrelated_directory() {
    let root = TempDir::new().unwrap();
    let base = root.path().join("veil");
    let options = LockOptions {
        timeout: Some(Duration::from_millis(400)),
        cancellation: None,
    };
    let manager = VeilManager::new(base.clone()).with_lock_options(options.clone());
    manager.init_veil("test", "password").unwrap();
    manager.create_directory("locked/sub", "password").unwrap();
    manager.create_directory("other", "password").unwrap();
    let source = root.path().join("source");
    std::fs::write(&source, b"content").unwrap();
    manager
        .add_files(
            &[AddFileSpec::new(&source, "locked/sub/existing")],
            "password",
        )
        .unwrap();
    let db = veil_service::storage::container::ContainerDb::open(&base, "password", None).unwrap();
    let locked = db.resolve("locked").unwrap().unwrap().id;
    drop(db);
    let lock = LockManager::new(&base, "password")
        .with_options(options)
        .acquire_directory(&locked, LockMode::Write)
        .unwrap();
    assert!(
        manager
            .create_directory("locked/sub/new", "password")
            .is_err()
    );
    assert!(
        manager
            .add_files(
                &[AddFileSpec::new(&source, "locked/sub/new-file")],
                "password"
            )
            .is_err()
    );
    assert!(
        manager
            .read_file_bytes("locked/sub/existing", "password", 100)
            .is_err()
    );
    assert!(
        manager
            .remove_path("locked/sub/existing", "password")
            .is_err()
    );
    assert!(
        manager
            .move_path("locked", "other/locked", "password")
            .is_err()
    );
    manager.create_directory("other/child", "password").unwrap();
    manager
        .add_files(&[AddFileSpec::new(&source, "other/child/new")], "password")
        .unwrap();
    assert!(
        manager
            .move_path("other/child/new", "locked/sub/new", "password")
            .is_err()
    );
    assert!(manager.change_password("password", "new-password").is_err());
    assert!(
        manager
            .change_password_full("password", "new-password")
            .is_err()
    );
    drop(lock);
    assert_eq!(
        manager
            .read_file_bytes("locked/sub/existing", "password", 100)
            .unwrap(),
        b"content"
    );
}

#[test]
fn directory_lock_process_worker() {
    let Some(root) = std::env::var_os("VEIL_DIRECTORY_LOCK_TEST_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let manager = VeilManager::new(root.join("veil")).with_lock_options(LockOptions {
        timeout: Some(Duration::from_millis(500)),
        cancellation: None,
    });
    assert!(manager.create_directory("locked/new", "password").is_err());
    assert!(
        manager
            .read_file_bytes("locked/existing", "password", 100)
            .is_err()
    );
    assert!(
        manager
            .add_files(
                &[AddFileSpec::new(root.join("source"), "locked/new-file")],
                "password"
            )
            .is_err()
    );
    assert!(
        manager
            .move_path("locked/existing", "other/moved", "password")
            .is_err()
    );
    manager
        .create_directory("other/allowed", "password")
        .unwrap();
}

#[test]
fn directory_lock_blocks_another_process() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("veil");
    let manager = VeilManager::new(path.clone());
    manager.init_veil("test", "password").unwrap();
    manager.create_directory("locked", "password").unwrap();
    manager.create_directory("other", "password").unwrap();
    std::fs::write(root.path().join("source"), b"content").unwrap();
    manager
        .add_files(
            &[AddFileSpec::new(
                root.path().join("source"),
                "locked/existing",
            )],
            "password",
        )
        .unwrap();
    let db = veil_service::storage::container::ContainerDb::open(&path, "password", None).unwrap();
    let directory = db.resolve("locked").unwrap().unwrap().id;
    drop(db);
    let guard = LockManager::new(&path, "password")
        .acquire_directory(&directory, LockMode::Write)
        .unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "directory_lock_process_worker", "--nocapture"])
        .env("VEIL_DIRECTORY_LOCK_TEST_ROOT", root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    drop(guard);
    assert_eq!(
        manager
            .read_file_bytes("locked/existing", "password", 100)
            .unwrap(),
        b"content"
    );
}

#[test]
fn directory_lock_crash_worker() {
    let Some(root) = std::env::var_os("VEIL_DIRECTORY_CRASH_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let path = root.join("veil");
    let db = veil_service::storage::container::ContainerDb::open(&path, "password", None).unwrap();
    let id = db.resolve("locked").unwrap().unwrap().id;
    drop(db);
    let _guard = LockManager::new(&path, "password")
        .acquire_directory(&id, LockMode::Write)
        .unwrap();
    std::fs::write(root.join("ready"), b"ready").unwrap();
    std::thread::sleep(Duration::from_secs(60));
}

#[test]
fn crashed_directory_lock_owner_is_reclaimed() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("veil");
    let manager = VeilManager::new(path.clone());
    manager.init_veil("test", "password").unwrap();
    manager.create_directory("locked", "password").unwrap();
    let db = veil_service::storage::container::ContainerDb::open(&path, "password", None).unwrap();
    let id = db.resolve("locked").unwrap().unwrap().id;
    drop(db);
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "directory_lock_crash_worker", "--nocapture"])
        .env("VEIL_DIRECTORY_CRASH_ROOT", root.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !root.path().join("ready").exists() {
        assert!(child.try_wait().unwrap().is_none(), "子进程提前退出");
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("子进程未获得目录锁");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    let manager = LockManager::new(&path, "password").with_options(LockOptions {
        timeout: Some(Duration::from_secs(5)),
        cancellation: None,
    });
    let guard = manager.acquire_directory(&id, LockMode::Write).unwrap();
    drop(guard);
}
#[test]
fn reads_share_writes_exclude_and_batch_failure_leaves_no_partial_locks() {
    let root = TempDir::new().unwrap();
    let m = VeilManager::new(root.path().join("veil"));
    m.init_veil("test", "password").unwrap();
    let source = root.path().join("source");
    std::fs::write(&source, b"contents").unwrap();
    m.add_files(&[AddFileSpec::new(&source, "a")], "password")
        .unwrap();
    let identity = m.list_files("password").unwrap()[0].file_id.clone();
    let options = LockOptions {
        timeout: Some(Duration::from_millis(250)),
        cancellation: None,
    };
    let manager = LockManager::new(&m.veil_dir, "password").with_options(options.clone());
    let read = manager
        .acquire(&[identity.clone()], LockMode::Read)
        .unwrap();
    let second = manager
        .acquire(&[identity.clone()], LockMode::Read)
        .unwrap();
    assert!(
        manager
            .acquire(&["other".into(), identity.clone()], LockMode::Write)
            .is_err()
    );
    let independent = manager.acquire(&["other".into()], LockMode::Write).unwrap();
    drop(independent);
    assert!(
        VeilManager::new(m.veil_dir.clone())
            .with_lock_options(options.clone())
            .add_files(&[AddFileSpec::new(&source, "a")], "password")
            .is_err()
    );
    drop(read);
    drop(second);
    let exclusive = manager
        .acquire(&[identity.clone()], LockMode::Write)
        .unwrap();
    assert!(manager.acquire(&[identity], LockMode::Read).is_err());
    exclusive.release().unwrap();
    assert!(!m.veil_dir.join(".locks").exists());
}
#[test]
fn handoff_rejects_old_credentials_and_cancellation_is_observed() {
    let root = TempDir::new().unwrap();
    let m = VeilManager::new(root.path().join("veil"));
    m.init_veil("test", "pw").unwrap();
    let manager = LockManager::new(&m.veil_dir, "pw");
    let guard = manager
        .acquire(&["a".into(), "b".into()], LockMode::Write)
        .unwrap();
    let old = guard.credentials().unwrap();
    let mut renewed = old.clone();
    manager.renew(&mut renewed).unwrap();
    let execution = ExecutionService::global().unwrap();
    let scope = execution.enter_task("接收锁");
    assert!(
        manager
            .handoff(&old[..1], scope.owner().clone(), None)
            .is_err()
    );
    let replacement = guard.handoff(scope.owner().clone(), None).unwrap();
    let mut stale = old.clone();
    assert!(manager.renew(&mut stale).is_err());
    assert!(manager.release(&old).is_err());
    replacement.release().unwrap();
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    assert!(
        manager
            .with_options(LockOptions {
                timeout: None,
                cancellation: Some(cancelled)
            })
            .acquire(&["a".into()], LockMode::Read)
            .is_err()
    );
}

#[test]
fn completed_tasks_are_reclaimed_early_and_unknown_tasks_keep_their_lease() {
    use veil_service::execution::ExecutionOwner;
    let root = TempDir::new().unwrap();
    let m = VeilManager::new(root.path().join("veil"));
    m.init_veil("test", "pw").unwrap();
    let manager = LockManager::new(&m.veil_dir, "pw").with_options(LockOptions {
        timeout: Some(Duration::from_millis(300)),
        cancellation: None,
    });
    let execution = ExecutionService::global().unwrap();
    let scope = execution.enter_task("原工作");
    let guard = manager
        .acquire_for(&["a".into()], LockMode::Write, scope.owner().clone(), None)
        .unwrap();
    drop(scope);
    let replacement = manager.acquire(&["a".into()], LockMode::Write).unwrap();
    assert!(manager.release(&guard.credentials().unwrap()).is_err());
    drop(replacement);
    drop(guard);
    let unknown = ExecutionOwner::Task {
        process: execution.runtime().process().clone(),
        task_id: "not-registered".into(),
    };
    let guard = manager
        .acquire_for(&["b".into()], LockMode::Write, unknown, None)
        .unwrap();
    assert!(manager.acquire(&["b".into()], LockMode::Read).is_err());
    drop(guard);
}
