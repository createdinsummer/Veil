use std::{fs, process::Command};
use veil_core::{
    error::VeilError,
    logging::{self, Config, Level, Record},
};

fn worker(mode: &str) {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "logging_worker", "--nocapture"])
        .env("VEIL_LOG_TEST_MODE", mode)
        .env("VEIL_LOG_TEST_ROOT", root.path())
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
fn concurrent_threads_and_processes_preserve_json_records() {
    worker("concurrent");
}
#[test]
fn rotation_and_export_remain_bounded() {
    worker("rotation");
}
#[test]
fn errors_and_panics_do_not_leak_sensitive_text() {
    worker("privacy");
}
#[test]
fn disabled_levels_and_storage_failure_are_controllable() {
    worker("controls");
}
#[test]
fn incomplete_record_does_not_swallow_the_next_error() {
    worker("partial");
}
#[test]
fn repeated_errors_write_once_and_export_the_repeat_count() {
    worker("repeat");
}
#[test]
fn database_failure_keeps_the_cause_and_request_context() {
    worker("database");
}
#[test]
fn worker_threads_inherit_request_and_sensitive_values() {
    worker("context");
}
#[test]
fn logger_lock_timeout_and_storage_recovery_do_not_block_the_client() {
    worker("sink-lock");
}
#[test]
fn disabled_logging_does_not_format_errors() {
    worker("lazy");
}
#[test]
fn arbitrary_unlabelled_error_text_and_client_messages_are_not_exported() {
    worker("unstructured");
}
#[test]
fn completed_records_survive_a_killed_process() {
    worker("killed");
}
#[test]
fn invalid_utf8_record_does_not_prevent_export() {
    worker("invalid-utf8");
}
#[test]
fn wrapped_business_conflicts_do_not_become_transaction_errors() {
    worker("expected-batch");
}
#[test]
fn pending_repeat_summary_survives_failed_eviction_and_directory_change() {
    worker("pending-summary");
}
#[test]
fn legacy_unstructured_messages_are_removed_from_exports() {
    worker("legacy");
}
#[cfg(unix)]
#[test]
fn log_symlinks_cannot_overwrite_or_export_other_files() {
    worker("symlink");
}

#[test]
fn production_code_only_uses_the_core_logging_facade() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    fn inspect(path: &std::path::Path) {
        if path.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                inspect(&entry.unwrap().path());
            }
        } else if path.extension().is_some_and(|e| e == "rs" || e == "swift") {
            if path.ends_with("veil-core/src/logging.rs") {
                return;
            }
            let text = fs::read_to_string(path).unwrap();
            for backend in [
                "tracing::",
                "log::",
                "env_logger::",
                "NSLog(",
                "os_log(",
                "Logger(",
                "import OSLog",
                "import os.log",
            ] {
                assert!(
                    !text.contains(backend),
                    "{} 直接使用了日志后端 {}，请改用 core 封装",
                    path.display(),
                    backend
                );
            }
        }
    }
    for entry in fs::read_dir(root.join("crates")).unwrap() {
        inspect(&entry.unwrap().path().join("src"));
    }
    for app in ["veil-macos", "veil-ios"] {
        inspect(&root.join("apps").join(app).join("Sources"));
    }
}

#[test]
fn logging_worker() {
    let Ok(mode) = std::env::var("VEIL_LOG_TEST_MODE") else {
        return;
    };
    let root = std::path::PathBuf::from(std::env::var_os("VEIL_LOG_TEST_ROOT").unwrap());
    let directory = root.join("logs");
    let mut config = Config::new(directory.clone());
    config.repeat_interval = std::time::Duration::ZERO;
    config.max_file_bytes = 4096;
    config.max_files = 3;
    if mode == "concurrent" || mode == "child" {
        config.max_file_bytes = 1024 * 1024;
    }
    logging::configure(config.clone()).unwrap();
    match mode.as_str() {
        "legacy" => {
            logging::error("test", "legacy", "OLD_FAILURE");
            let path = directory.join("errors-0.jsonl");
            let mut record: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            record.as_object_mut().unwrap().remove("privacy_version");
            record["message"] = "任意用户明文".into();
            record["causes"] = serde_json::json!(["任意用户明文"]);
            record["backtrace"] = "任意用户明文".into();
            record["context"] = serde_json::json!({"unsafe_label": "任意用户明文", "attempts": 3});
            fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
            let exported = String::from_utf8(logging::export().unwrap()).unwrap();
            assert!(!exported.contains("任意用户明文"));
            let records = logging::snapshot().unwrap();
            assert_eq!(records[0].event, "OLD_FAILURE");
            assert_eq!(records[0].context["attempts"], 3);
        }
        "pending-summary" => {
            config.max_file_bytes = 1024 * 1024;
            config.repeat_interval = std::time::Duration::from_secs(30);
            logging::configure(config).unwrap();
            for count in 0..1000 {
                logging::event(
                    Level::Error,
                    "test",
                    "cache",
                    "FAILED",
                    "操作失败",
                    &[("attempts", logging::Field::Count(count))],
                );
            }
            for resource in 0..127 {
                logging::event(
                    Level::Error,
                    "test",
                    "cache",
                    "FAILED",
                    "操作失败",
                    &[("resource", logging::Field::Resource(&resource.to_string()))],
                );
            }
            fs::remove_file(directory.join(".lock")).unwrap();
            fs::create_dir(directory.join(".lock")).unwrap();
            // 缓存已满，淘汰摘要落盘失败时必须保留末次状态及重复计数。
            logging::error("test", "overflow", "FAILED");
            let moved = root.join("recovered-logs");
            logging::set_directory(moved).unwrap();
            let records = logging::snapshot().unwrap();
            let summary = records.iter().find(|r| r.repeat_summary).unwrap();
            assert_eq!(summary.suppressed_count, 999);
            assert_eq!(summary.context["attempts"], 999);
        }
        "expected-batch" => {
            let mut db = rusqlite::Connection::open_in_memory().unwrap();
            let result: Result<(), VeilError> = veil_core::database::commit_batch(
                &mut db,
                |_| {
                    Err(VeilError::ImportBatchFailed {
                        file: "private-name".into(),
                        source: Box::new(VeilError::VersionConflict("private-name".into())),
                    })
                },
                || {},
            );
            assert!(result.is_err());
            assert!(!directory.exists(), "预期版本冲突不应生成故障日志");
        }
        #[cfg(unix)]
        "symlink" => {
            let private = root.join("private-file");
            fs::write(&private, b"private content").unwrap();
            fs::create_dir_all(&directory).unwrap();
            std::os::unix::fs::symlink(&private, directory.join("errors-0.jsonl")).unwrap();
            logging::error("test", "symlink_sink", "WRITE_REJECTED");
            assert_eq!(logging::health().0, 1);
            assert!(logging::export().is_err());
            assert_eq!(fs::read_to_string(private).unwrap(), "private content");
        }
        "sink-lock" => {
            fs::create_dir_all(&directory).unwrap();
            let lock = fs::File::create(directory.join(".lock")).unwrap();
            lock.lock().unwrap();
            let started = std::time::Instant::now();
            logging::error("test", "blocked_sink", "WRITE_FAILED");
            assert!(started.elapsed() < std::time::Duration::from_secs(1));
            assert_eq!(logging::health().0, 1);
            let started = std::time::Instant::now();
            logging::error("test", "backoff", "WRITE_FAILED");
            assert!(started.elapsed() < std::time::Duration::from_millis(80));
            assert_eq!(logging::health().0, 2);
            // 日志失败不抛出异常，也不影响调用方的正常文件操作。
            fs::write(root.join("business-result"), b"ok").unwrap();
            drop(lock);
            logging::configure(config).unwrap();
            logging::error("test", "recovered_sink", "WRITE_OK");
            let records = logging::snapshot().unwrap();
            assert!(records.iter().any(|r| r.event == "WRITE_OK"));
            let recovered = records
                .iter()
                .find(|r| r.event == "LOGGING_RECOVERED")
                .unwrap();
            assert_eq!(recovered.context["failed_records"], 2);
        }
        "lazy" => {
            #[derive(Debug)]
            struct ExpensiveError;
            impl std::fmt::Display for ExpensiveError {
                fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    panic!("关闭的日志不能格式化错误")
                }
            }
            impl std::error::Error for ExpensiveError {
                fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                    panic!("关闭的日志不能遍历错误链")
                }
            }
            config.enabled = false;
            logging::configure(config).unwrap();
            logging::failure(
                Level::Error,
                "test",
                "disabled",
                "DISABLED",
                &ExpensiveError,
                &[],
            );
            logging::initialize();
            assert!(std::panic::catch_unwind(|| panic!("关闭的日志不能采集堆栈")).is_err());
            assert_eq!(logging::health().0, 0);
            assert!(!directory.exists());
        }
        "unstructured" => {
            let raw = "客户甲的银行明细 任意未标记明文 private-unlabelled-value";
            for error in [
                VeilError::DecryptionError(raw.into()),
                VeilError::SerializationError(raw.into()),
                VeilError::VersionConflict(raw.into()),
                VeilError::Io(std::io::Error::other(raw)),
                VeilError::Database(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                    Some(raw.into()),
                )),
            ] {
                logging::error_chain("test", "untrusted", &error);
            }
            logging::client_error(3, "ServiceClient.swift", 12, 260, raw);
            logging::client_error(
                3,
                "ServiceClient.swift",
                13,
                260,
                "客户端异常：Cocoa code=260\nCaused by: 客户端异常：POSIX code=13",
            );
            logging::initialize();
            assert!(std::panic::catch_unwind(|| panic!("{raw}")).is_err());
            logging::error_chain(
                "test",
                "integrity",
                &VeilError::InvalidFormat("文件密钥轮换状态无效".into()),
            );
            let text = String::from_utf8(logging::export().unwrap()).unwrap();
            assert!(!text.contains("客户甲"));
            assert!(!text.contains("任意未标记明文"));
            assert!(!text.contains("private-unlabelled-value"));
            let records = logging::snapshot().unwrap();
            assert_eq!(records.len(), 9);
            assert_eq!(records[6].message, "客户端异常：Cocoa code=260");
            assert_eq!(records[6].causes, ["客户端异常：POSIX code=13"]);
            assert_eq!(records[8].message, "文件密钥轮换状态无效");
        }
        "killed-child" => {
            use std::io::Write;
            logging::error("test", "before_kill", "BEFORE_KILL");
            println!("ready");
            std::io::stdout().flush().unwrap();
            loop {
                std::thread::park();
            }
        }
        "killed" => {
            use std::io::BufRead;
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "logging_worker", "--nocapture"])
                .env("VEIL_LOG_TEST_MODE", "killed-child")
                .env("VEIL_LOG_TEST_ROOT", &root)
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let output = std::io::BufReader::new(child.stdout.take().unwrap());
            for line in output.lines() {
                if line.unwrap().contains("ready") {
                    break;
                }
            }
            child.kill().unwrap();
            assert!(!child.wait().unwrap().success());
            logging::error("test", "after_kill", "AFTER_KILL");
            let records = logging::snapshot().unwrap();
            assert_eq!(records.len(), 2);
            assert_eq!(records[0].event, "BEFORE_KILL");
            assert_eq!(records[1].event, "AFTER_KILL");
        }
        "invalid-utf8" => {
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("errors-0.jsonl"), b"\xff\xfe\n").unwrap();
            logging::error("test", "after_corruption", "VALID_RECORD");
            let records = logging::snapshot().unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].event, "VALID_RECORD");
        }
        "repeat" => {
            config.repeat_interval = std::time::Duration::from_secs(30);
            config.max_file_bytes = 1024 * 1024;
            logging::configure(config).unwrap();
            for attempt in 0..1000 {
                logging::failure(
                    Level::Error,
                    "locks",
                    "renew",
                    "LEASE_RENEW_FAILED",
                    &std::io::Error::new(std::io::ErrorKind::TimedOut, "租约续租超时"),
                    &[("attempts", logging::Field::Count(attempt))],
                );
            }
            let file = fs::read_to_string(directory.join("errors-0.jsonl")).unwrap();
            assert_eq!(file.lines().count(), 1, "重复故障不能逐次落盘");
            let records = logging::snapshot().unwrap();
            assert_eq!(records.len(), 2);
            assert!(records[1].repeat_summary);
            assert_eq!(records[1].suppressed_count, 999);
            assert_eq!(records[1].context["attempts"], 999);
            assert!(records.iter().all(|r| r.backtrace.is_none()));
        }
        "database" => {
            let _request = logging::request_scope();
            let path = root.join("private-name.db");
            let mut first = rusqlite::Connection::open(&path).unwrap();
            let second = rusqlite::Connection::open(&path).unwrap();
            first
                .busy_timeout(std::time::Duration::from_millis(10))
                .unwrap();
            second
                .execute_batch("CREATE TABLE data(value); BEGIN IMMEDIATE")
                .unwrap();
            let result = veil_core::database::commit_batch(&mut first, |_| Ok(()), || {});
            assert!(result.is_err());
            second.execute_batch("ROLLBACK").unwrap();
            let records = logging::snapshot().unwrap();
            let record = records
                .iter()
                .find(|r| r.event == "TRANSACTION_BEGIN_FAILED")
                .unwrap();
            assert_eq!(record.code, "SQLITE_BUSY");
            assert_eq!(record.native_code, Some(5));
            assert!(
                record
                    .causes
                    .iter()
                    .any(|cause| cause.contains("database is locked"))
            );
            assert_eq!(record.request_id, logging::request_id());
            assert!(!record.thread_id.is_empty());
        }
        "context" => {
            let _request = logging::request_scope();
            let _secret = logging::sensitive_scope(&["a-bare-sensitive-value"]);
            let id = logging::request_id();
            let runtime = veil_core::execution::ExecutionRuntime::global().unwrap();
            runtime
                .spawn_thread("诊断上下文测试", move |_| {
                    logging::error_chain(
                        "test",
                        "child",
                        &std::io::Error::other(
                            "写入失败 a-bare-sensitive-value /private/confidential.txt",
                        ),
                    );
                })
                .unwrap()
                .handle
                .join()
                .unwrap();
            let records = logging::snapshot().unwrap();
            assert_eq!(records[0].request_id, id);
            assert_eq!(records[0].code, "IO_ERROR");
            assert!(records[0].message.contains("Other"));
            let text = String::from_utf8(logging::export().unwrap()).unwrap();
            assert!(!text.contains("a-bare-sensitive-value"));
            assert!(!text.contains("confidential"));
        }
        "child" => {
            for _ in 0..50 {
                logging::error("test", "process", "TEST_FAILURE");
            }
        }
        "concurrent" => {
            let mut children: Vec<_> = (0..3)
                .map(|_| {
                    Command::new(std::env::current_exe().unwrap())
                        .args(["--exact", "logging_worker"])
                        .env("VEIL_LOG_TEST_MODE", "child")
                        .env("VEIL_LOG_TEST_ROOT", &root)
                        .stdout(std::process::Stdio::null())
                        .spawn()
                        .unwrap()
                })
                .collect();
            std::thread::scope(|scope| {
                for _ in 0..8 {
                    scope.spawn(|| {
                        for _ in 0..50 {
                            logging::error("test", "thread", "TEST_FAILURE");
                        }
                    });
                }
            });
            for child in &mut children {
                assert!(child.wait().unwrap().success());
            }
            assert_eq!(logging::snapshot().unwrap().len(), 550);
            assert_eq!(logging::health(), (0, None));
        }
        "rotation" => {
            for _ in 0..150 {
                logging::error("test", "rotation", "TEST_FAILURE");
            }
            logging::error("test", "last", "LAST_ERROR");
            let files: Vec<_> = fs::read_dir(&directory)
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
                .collect();
            assert_eq!(files.len(), 3);
            for file in files {
                assert!(file.metadata().unwrap().len() <= 4096);
            }
            let records: Vec<Record> = serde_json::from_slice(&logging::export().unwrap()).unwrap();
            assert!(records.len() < 150);
            assert_eq!(records.last().unwrap().code, "LAST_ERROR");
            config.max_files = 1;
            logging::configure(config.clone()).unwrap();
            logging::error("test", "new_retention", "AFTER_RECONFIGURE");
            assert!(!directory.join("errors-1.jsonl").exists());
            assert!(!directory.join("errors-2.jsonl").exists());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    directory.metadata().unwrap().permissions().mode() & 0o777,
                    0o700
                );
                assert_eq!(
                    directory
                        .join("errors-0.jsonl")
                        .metadata()
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
        "privacy" => {
            let secret = "secret-password /Users/private/confidential.txt";
            let _secrets = logging::sensitive_scope(&["secret-password"]);
            logging::error_chain(
                "test",
                "io",
                &VeilError::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    secret,
                )),
            );
            logging::error_chain(
                "test",
                "decrypt",
                &VeilError::DecryptionError(secret.into()),
            );
            logging::error_chain(
                "test",
                "database",
                &VeilError::Database(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                    Some(secret.into()),
                )),
            );
            logging::initialize();
            assert!(std::panic::catch_unwind(|| panic!("{secret}")).is_err());
            let bytes = logging::export().unwrap();
            let text = String::from_utf8(bytes).unwrap();
            assert!(!text.contains("secret-password"));
            assert!(!text.contains("/Users/"));
            assert!(!text.contains("confidential"));
            let records = logging::snapshot().unwrap();
            assert_eq!(records.len(), 4);
            assert_eq!(records[0].code, "IO_PERMISSION_DENIED");
            assert_eq!(records[2].code, "SQLITE_BUSY");
            assert_eq!(records[3].code, "PANIC");
        }
        "controls" => {
            logging::record(Level::Info, "test", "filtered", "INFO");
            config.enabled = false;
            logging::configure(config.clone()).unwrap();
            logging::error("test", "disabled", "ERROR");
            assert!(!directory.exists());
            config.enabled = true;
            config.minimum_level = Level::Debug;
            logging::configure(config.clone()).unwrap();
            logging::record(Level::Debug, "test", "enabled", "DEBUG");
            assert_eq!(logging::snapshot().unwrap().len(), 1);
            let blocked = root.join("blocked");
            fs::write(&blocked, b"file").unwrap();
            config.directory = blocked;
            logging::configure(config).unwrap();
            logging::error("test", "storage_failure", "ERROR");
            assert_eq!(logging::health().0, 1);
            assert!(logging::health().1.is_some());
        }
        "partial" => {
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("errors-0.jsonl"), b"{\"interrupted\":").unwrap();
            logging::error("test", "recovery", "RECOVERED");
            let records = logging::snapshot().unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].code, "RECOVERED");
        }
        _ => panic!("未知测试模式"),
    }
}
