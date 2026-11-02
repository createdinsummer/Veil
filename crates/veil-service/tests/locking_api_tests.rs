//! 公开文件租约与容器操作共用数据库协议。
use std::time::Duration;
use tempfile::TempDir;
use veil_service::{
    ExecutionService,
    storage::locks::{LockManager, LockMode, LockOptions},
    veil_ops::{AddFileSpec, VeilManager},
};
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
