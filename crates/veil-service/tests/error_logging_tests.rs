//! 服务端错误日志与故障诊断流程的集成测试。
//! 所属模块：锁、后台租约、数据库和部分文件操作的诊断；验证日志保留可操作上下文且避免重复写入或泄露敏感值。
//! worker 函数通过独立进程模拟锁与存储故障，测试数据写入隔离临时目录。
use std::{
    process::Command,
    time::{Duration, Instant},
};
use veil_core::logging::{self, Config};
use veil_service::{
    storage::locks::{LockManager, LockMode, LockOptions},
    veil_ops::VeilManager,
};

#[test]
/// 验证数据库锁等待和后台租约续期失败可被诊断，同时相同故障不会无限重复写日志。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn database_lock_and_background_lease_failures_are_diagnosable_without_log_spam() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "error_logging_worker", "--nocapture"])
        .env("VEIL_ERROR_LOG_TEST_ROOT", root.path())
        .env("VEIL_APP_DATA_DIR", root.path().join("app"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
/// 作为子进程执行指定故障场景，并把结果写入临时测试日志供父测试核对。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn error_logging_worker() {
    let Some(root) = std::env::var_os("VEIL_ERROR_LOG_TEST_ROOT") else {
        return;
    };
    #[cfg(debug_assertions)]
    veil_core::kdf::enable_fast_test_kdf();
    let root = std::path::PathBuf::from(root);
    logging::configure(Config::new(root.join("logs"))).unwrap();
    let _request = logging::request_scope();
    let veil = VeilManager::new(root.join("private-container"));
    veil.init_veil("private-container", "sensitive-password")
        .unwrap();
    let service = veil_service::VeilService::default();
    assert!(
        service
            .execute(veil_service::ServiceRequest::RefreshContainer)
            .is_err()
    );
    assert!(
        service
            .execute(veil_service::ServiceRequest::ValidateNewPassword { password: "" })
            .is_err()
    );
    let source = root.join("sensitive-source-name");
    std::fs::write(&source, b"sensitive-plaintext-content").unwrap();
    veil.add_files(
        &[veil_service::veil_ops::AddFileSpec::new(
            &source,
            "sensitive-target-name",
        )],
        "sensitive-password",
    )
    .unwrap();
    veil.remove_file("sensitive-target-name", "sensitive-password")
        .unwrap();
    let locks = LockManager::new(&veil.veil_dir, "sensitive-password");
    locks
        .acquire_container(LockMode::Write)
        .unwrap()
        .release()
        .unwrap();
    let lease = locks.acquire_container(LockMode::Write).unwrap();
    assert!(
        logging::snapshot()
            .unwrap()
            .iter()
            .all(|record| record.event == "OWNER_PROBE_UNAVAILABLE"),
        "正常写入、删除、租约释放以及业务校验不能产生异常日志；运行环境拒绝监听时允许记录探测降级"
    );
    let db = veil_core::database::open(
        &veil.veil_dir.join("veil.db"),
        Some("sensitive-password"),
        false,
    )
    .unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    // 真实占用 SQLite 写锁，确认续租会在临时繁忙后等待并恢复；不注入伪造错误。
    std::thread::sleep(Duration::from_secs(8));
    db.execute_batch("ROLLBACK").unwrap();
    drop(db);
    let deadline = Instant::now() + Duration::from_secs(8);
    let recovered = loop {
        if let Some(record) = logging::snapshot()
            .unwrap()
            .into_iter()
            .find(|r| r.event == "LEASE_RENEW_SLOW")
        {
            break record;
        }
        assert!(
            Instant::now() < deadline,
            "续租线程应在 SQLite 临时繁忙后恢复"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(recovered.request_id, logging::request_id());
    assert_eq!(recovered.context["lock_count"], 1);
    assert!(recovered.context["elapsed_ms"].as_u64().unwrap() >= 1000);
    assert!(recovered.context["remaining_lease_ms"].as_u64().unwrap() > 0);
    assert!(recovered.backtrace.is_none(), "常规故障默认不采集堆栈");
    drop(lease);

    let lease = locks.acquire_container(LockMode::Write).unwrap();
    let waiting = locks.clone().with_options(LockOptions {
        timeout: Some(Duration::from_millis(80)),
        cancellation: None,
    });
    assert!(waiting.acquire_container(LockMode::Write).is_err());
    let db = veil_core::database::open(
        &veil.veil_dir.join("veil.db"),
        Some("sensitive-password"),
        false,
    )
    .unwrap();
    db.execute("DELETE FROM operation_locks", []).unwrap();
    assert!(locks.renew(&mut lease.credentials().unwrap()).is_err());
    drop(db);
    drop(lease);
    let records = logging::snapshot().unwrap();
    assert!(
        records.iter().any(
            |r| r.event == "LOCK_ACQUIRE_FAILED" && r.context["attempts"].as_u64().unwrap() > 1
        )
    );
    assert!(
        records
            .iter()
            .any(|r| r.event == "LEASE_RENEW_FAILED" && r.code == "INVALID_FORMAT")
    );
    assert!(
        records.len() <= 12,
        "锁轮询和续租不能频繁打印：{} 条",
        records.len()
    );
    // 批量提交的文件定位包装必须保留原始原因，不能再降格为 String。
    let wrapped = veil_core::error::VeilError::ImportBatchFailed {
        file: "sensitive-wrapped-file-name".into(),
        source: Box::new(veil_core::error::VeilError::Database(
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some("sensitive-database-message".into()),
            ),
        )),
    };
    logging::failure(
        logging::Level::Error,
        "service",
        "commit_import_batch",
        "IMPORT_BATCH_FAILED",
        &wrapped,
        &[("phase", logging::Field::Label("apply_metadata"))],
    );
    let nested = logging::snapshot()
        .unwrap()
        .into_iter()
        .find(|record| record.event == "IMPORT_BATCH_FAILED")
        .unwrap();
    assert_eq!(nested.code, "SQLITE_BUSY");
    assert_eq!(nested.native_code, Some(5));
    // 包装层无法归类时，原始原因被提升为 message。
    assert!(
        nested.message.contains("database is locked"),
        "{}",
        nested.message
    );
    let text = String::from_utf8(logging::export().unwrap()).unwrap();
    assert!(!text.contains("sensitive-wrapped-file-name"));
    assert!(!text.contains("sensitive-database-message"));
    assert!(!text.contains("sensitive-password"));
    assert!(!text.contains("private-container"));
    assert!(!text.contains("sensitive-source-name"));
    assert!(!text.contains("sensitive-target-name"));
    assert!(!text.contains("sensitive-plaintext-content"));
    assert!(!text.contains(root.to_str().unwrap()));
}

#[cfg(unix)]
#[test]
/// 验证存储变更部分失败时记录失败阶段和安全错误码，日志不包含用户路径或文件内容。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
/// 异常：测试准备或断言失败会 panic；场景中预期的错误由测试体捕获或核对，不作为生产错误返回。
fn failed_storage_mutation_records_stage_and_preserves_privacy() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "partial_storage_mutation_worker", "--nocapture"])
        .env("VEIL_PARTIAL_LOG_TEST_ROOT", root.path())
        .env("VEIL_APP_DATA_DIR", root.path().join("app"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
/// 在子进程中触发指定的部分存储变更故障，用于验证恢复和诊断记录。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn partial_storage_mutation_worker() {
    let Some(root) = std::env::var_os("VEIL_PARTIAL_LOG_TEST_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    logging::configure(Config::new(root.join("logs"))).unwrap();
    let service = veil_service::VeilService::default();
    let path = root.join("sensitive-space-directory");
    let veil_service::ServiceResponse::Spaces(spaces) = service
        .execute(veil_service::ServiceRequest::CreateSpace {
            name: "sensitive-space-name".into(),
            path: Some(path.clone()),
        })
        .unwrap()
    else {
        panic!("空间列表")
    };
    let identity = spaces
        .iter()
        .find(|space| space.name == "sensitive-space-name")
        .unwrap()
        .id
        .clone();
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path.join("sensitive-file-name"), b"sensitive-content").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).unwrap();
    let failure = service
        .execute(veil_service::ServiceRequest::DeleteSpace {
            id: identity.clone(),
        })
        .err();
    // 还原权限后才断言，保证测试目录能可靠清理。
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(failure.is_some(), "目录写权限不足时删除必须失败");
    let record = logging::snapshot()
        .unwrap()
        .into_iter()
        .find(|record| record.event == "STORAGE_MUTATION_INCOMPLETE")
        .expect("部分删除必须可诊断");
    assert_eq!(record.operation, "delete_space");
    assert_eq!(record.context["phase"], "remove_files");
    assert_eq!(record.context["outcome"], "verify_storage_and_registration");
    assert_ne!(record.context["resource"], identity);
    let export = String::from_utf8(logging::export().unwrap()).unwrap();
    for secret in [
        "sensitive-space-directory",
        "sensitive-space-name",
        "sensitive-file-name",
        "sensitive-content",
        root.to_str().unwrap(),
    ] {
        assert!(!export.contains(secret), "日志不得包含用户数据");
    }
}

#[test]
/// 验证空间操作系统锁发生等待时留下诊断，未竞争的正常加锁不制造噪声日志。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn space_os_lock_wait_is_diagnosable_without_logging_uncontended_locks() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "space_os_lock_logging_worker", "--nocapture"])
        .env("VEIL_SPACE_LOCK_LOG_TEST_ROOT", root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
/// 在隔离子进程中执行空间锁竞争场景，供父测试检查锁等待日志。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn space_os_lock_logging_worker() {
    let Some(root) = std::env::var_os("VEIL_SPACE_LOCK_LOG_TEST_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    logging::configure(Config::new(root.join("logs"))).unwrap();
    let _request = logging::request_scope();
    let mut store = veil_service::storage::app::AppStore::open(&root.join("app")).unwrap();
    let space = store
        .create_space(
            "sensitive-space-name",
            Some(&root.join("sensitive-space-path")),
            false,
        )
        .unwrap();
    drop(store.acquire_space_exclusive_lock(&space.id).unwrap());
    let held = store.acquire_space_shared_lock(&space.id).unwrap();
    assert!(
        logging::snapshot().unwrap().is_empty(),
        "无竞争的空间锁不能产生日志"
    );
    let context = logging::capture_context();
    let worker_root = root.clone();
    let identity = space.id.clone();
    let worker = std::thread::spawn(move || {
        let _logging = context.attach();
        let store = veil_service::storage::app::AppStore::open(&worker_root.join("app")).unwrap();
        store.acquire_space_exclusive_lock(&identity).unwrap()
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let waiting = loop {
        if let Some(record) = logging::snapshot()
            .unwrap()
            .into_iter()
            .find(|record| record.event == "SPACE_LOCK_WAITING")
        {
            break record;
        }
        assert!(Instant::now() < deadline, "空间锁阻塞时必须留下等待证据");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        !worker.is_finished(),
        "原共享锁仍被持有，排他锁必须继续等待"
    );
    assert_eq!(waiting.request_id, logging::request_id());
    assert_eq!(waiting.context["mode"], "exclusive");
    assert_eq!(waiting.context["phase"], "acquire_os_lock");
    std::thread::sleep(Duration::from_millis(1050));
    drop(held);
    drop(worker.join().unwrap());
    let records = logging::snapshot().unwrap();
    assert_eq!(
        records
            .iter()
            .filter(|record| record.event == "SPACE_LOCK_WAITING")
            .count(),
        1
    );
    let recovered = records
        .iter()
        .find(|record| record.event == "SPACE_LOCK_RECOVERED")
        .expect("长时间锁等待结束必须留下恢复结果");
    assert_eq!(recovered.context["mode"], "exclusive");
    assert!(recovered.context["elapsed_ms"].as_u64().unwrap() >= 1000);
    assert_eq!(recovered.context["outcome"], "acquired");
    let export = String::from_utf8(logging::export().unwrap()).unwrap();
    for secret in [
        "sensitive-space-name",
        "sensitive-space-path",
        root.to_str().unwrap(),
    ] {
        assert!(!export.contains(secret));
    }
}
