//! 核心日志系统的跨线程、跨进程和故障恢复集成测试。
//! 所属模块：日志记录、轮转、隐私过滤和诊断导出；子进程覆盖崩溃与存储故障场景。
//! 测试核对错误上下文、隐私过滤及存储故障后的业务继续执行；不代表任何情况下日志调用都绝不阻塞。
//! 核心函数：worker 启动隔离进程，logging_worker 执行各场景，inspect 递归检查生产源码中的日志后端标记。
//! 依赖能力：核心日志配置/快照/导出接口、JSON、文件系统、线程与子进程；目标是验证可诊断性与故障隔离边界。
use std::{fs, process::Command};
use veil_core::{
    error::VeilError,
    logging::{self, Config, Level, Record},
};

/// 在隔离子进程中运行一个日志验证场景，避免全局日志配置互相干扰。
/// 入参：mode（&str）为 logging_worker 分支识别的场景名，通过 VEIL_LOG_TEST_MODE 传入；临时日志根目录另外由本函数创建。
/// 返回值：()；子进程成功退出后返回，场景断言由子进程执行。
/// 异常：临时目录创建、定位测试二进制、进程启动或收集输出失败会 panic；非成功退出会携带输出断言失败。
/// 注意事项：阻塞等待且没有超时；必须保留 logging_worker 测试入口。临时目录在子进程结束后随本函数退出清理。
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
/// 验证多线程和多进程并发记录时每条 JSON 日志仍完整可解析。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn concurrent_threads_and_processes_preserve_json_records() {
    worker("concurrent");
}
#[test]
/// 验证日志轮转与导出限制容量，避免磁盘占用无限增长。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn rotation_and_export_remain_bounded() {
    worker("rotation");
}
#[test]
/// 验证错误和 panic 诊断会过滤敏感输入文本。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
/// 异常：测试准备或断言失败会 panic；场景中预期的错误由测试体捕获或核对，不作为生产错误返回。
fn errors_and_panics_do_not_leak_sensitive_text() {
    worker("privacy");
}
#[test]
/// 验证日志级别开关和日志存储故障都能按配置被控制。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn disabled_levels_and_storage_failure_are_controllable() {
    worker("controls");
}
#[test]
/// 验证写入中断留下的不完整记录不会遮蔽后续错误。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
/// 异常：测试准备或断言失败会 panic；场景中预期的错误由测试体捕获或核对，不作为生产错误返回。
fn incomplete_record_does_not_swallow_the_next_error() {
    worker("partial");
}
#[test]
/// 验证重复错误可合并记录，并在导出时保留出现次数。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
/// 异常：测试准备或断言失败会 panic；场景中预期的错误由测试体捕获或核对，不作为生产错误返回。
fn repeated_errors_write_once_and_export_the_repeat_count() {
    worker("repeat");
}
#[test]
/// 验证数据库失败记录保留底层原因和请求上下文。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn database_failure_keeps_the_cause_and_request_context() {
    worker("database");
}
#[test]
/// 验证包装错误缺少安全描述时能回退到内部错误的可公开说明。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
/// 异常：测试准备或断言失败会 panic；场景中预期的错误由测试体捕获或核对，不作为生产错误返回。
fn wrapped_errors_fall_back_to_the_inner_safe_description() {
    worker("wrapper");
}
#[test]
/// 验证工作线程继承请求关联信息与敏感值标记，使过滤规则保持一致。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn worker_threads_inherit_request_and_sensitive_values() {
    worker("context");
}
#[test]
/// 验证日志锁超时和存储恢复过程不会阻塞客户端业务线程。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn logger_lock_timeout_and_storage_recovery_do_not_block_the_client() {
    worker("sink-lock");
}
#[test]
/// 验证关闭日志时不执行错误文本格式化，避免无用开销和敏感读取。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
/// 异常：测试准备或断言失败会 panic；场景中预期的错误由测试体捕获或核对，不作为生产错误返回。
fn disabled_logging_does_not_format_errors() {
    worker("lazy");
}
#[test]
/// 验证未标记的原始错误文本与客户端消息不会进入可导出诊断。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
/// 异常：测试准备或断言失败会 panic；场景中预期的错误由测试体捕获或核对，不作为生产错误返回。
fn arbitrary_unlabelled_error_text_and_client_messages_are_not_exported() {
    worker("unstructured");
}
#[test]
/// 验证进程被终止后已提交的日志记录仍可读取。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn completed_records_survive_a_killed_process() {
    worker("killed");
}
#[test]
/// 验证坏掉的 UTF-8 记录不会阻止其他日志导出。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn invalid_utf8_record_does_not_prevent_export() {
    worker("invalid-utf8");
}
#[test]
/// 验证业务冲突即使被包装，也不会误分类为数据库事务故障。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn wrapped_business_conflicts_do_not_become_transaction_errors() {
    worker("expected-batch");
}
#[test]
/// 验证淘汰失败或日志目录切换时，尚未落盘的重复次数摘要仍保留。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn pending_repeat_summary_survives_failed_eviction_and_directory_change() {
    worker("pending-summary");
}
#[test]
/// 验证旧格式记录可以按兼容规则导出。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn legacy_records_are_exported_as_stored() {
    worker("legacy");
}
#[test]
/// 验证旧记录中的占位和空消息会在导出时安全修复。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn legacy_placeholder_and_empty_messages_are_repaired() {
    worker("placeholder");
}
#[cfg(unix)]
#[test]
/// 验证日志路径中的符号链接不能覆盖外部文件或把外部内容导出。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn log_symlinks_cannot_overwrite_or_export_other_files() {
    worker("symlink");
}

#[test]
/// 验证生产 Rust / Swift 源码只通过核心日志门面调用日志，不绕过隐私和格式规则。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn production_code_only_uses_the_core_logging_facade() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    /// 递归扫描生产源码，检查是否出现绕过核心日志门面的后端标记。
    /// 入参：path（&Path）为生产源码文件或目录；只检查扩展名为 rs、swift 的文件，跳过核心 logging.rs 自身。
    /// 返回值：()；当前路径及递归子项均未命中禁止标记时返回。
    /// 异常：遍历目录、读取 UTF-8 源码失败或命中任一标记会 panic，使测试失败。
    /// 注意事项：按文本包含判断，不解析语言语法，因此注释和字符串也会被检查；没有符号链接环检测。
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
/// 测试二进制的子进程入口；根据环境变量执行指定日志故障场景，不作为普通测试断言入口。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
            logging::flush().unwrap();
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
            // 旧版记录按存储内容原样导出，不再替换 message。
            assert!(exported.contains("任意用户明文"));
            assert!(!exported.contains("历史日志文本已省略"));
            let records = logging::snapshot().unwrap();
            assert_eq!(records[0].event, "OLD_FAILURE");
            assert_eq!(records[0].message, "任意用户明文");
            assert_eq!(records[0].causes, ["任意用户明文"]);
            assert_eq!(records[0].context["attempts"], 3);
            assert_eq!(records[0].context["unsafe_label"], "任意用户明文");
        }
        "placeholder" => {
            logging::error("test", "placeholder", "OLD_PLACEHOLDER");
            logging::flush().unwrap();
            let path = directory.join("errors-0.jsonl");
            let mut first: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            first["message"] = "错误详情已省略（非结构化文本）".into();
            first["causes"] = serde_json::json!(["DATABASE", "Error code 5: database is locked"]);
            let mut second = first.clone();
            second["message"] = "".into();
            second["causes"] = serde_json::json!([]);
            fs::write(&path, format!("{first}\n{second}\n")).unwrap();
            let records = logging::snapshot().unwrap();
            assert_eq!(records.len(), 2);
            // 把存量 safe 原因提升为 message，并删掉重复的 cause。
            assert_eq!(records[0].message, "Error code 5: database is locked");
            assert_eq!(records[0].causes, ["DATABASE"]);
            // 完全没有文本时退回错误码。
            assert_eq!(records[1].message, "OLD_PLACEHOLDER");
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
            logging::flush().unwrap();
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
            // 写盘已移到独立线程：目录锁被占用时调用方也不再等待。
            assert!(started.elapsed() < std::time::Duration::from_millis(200));
            logging::flush().unwrap();
            assert_eq!(logging::health().0, 1);
            let started = std::time::Instant::now();
            logging::error("test", "backoff", "WRITE_FAILED");
            assert!(started.elapsed() < std::time::Duration::from_millis(80));
            logging::flush().unwrap();
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
            /// 用于验证关闭日志时不会调用错误格式化或遍历底层错误。
            #[derive(Debug)]
            struct ExpensiveError;
            impl std::fmt::Display for ExpensiveError {
                /// 通过故意失败的格式化器，验证关闭日志后不会计算错误文案。
                /// 入参：self（&ExpensiveError）为无数据测试错误；_（&mut Formatter）为格式化目标，此处不会写入。
                /// 返回值：声明为 fmt::Result，实际永远不返回。
                /// 异常：每次调用都会 panic；若日志关闭路径调用此方法，测试应立即失败。
                /// 注意事项：仅供 lazy 场景验证惰性求值，不是生产错误类型。
                fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    panic!("关闭的日志不能格式化错误")
                }
            }
            impl std::error::Error for ExpensiveError {
                /// 通过故意失败的错误链访问器，验证关闭日志后不会查询根因。
                /// 入参：self（&ExpensiveError）为当前测试错误，无其他参数。
                /// 返回值：声明为可选底层错误引用，实际永远不返回。
                /// 异常：每次调用都会 panic；用来暴露关闭日志时不应发生的错误链遍历。
                /// 注意事项：仅供 lazy 场景使用；不会提供真实底层错误。
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
            logging::flush().unwrap();
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
            logging::flush().unwrap();
            let file = fs::read_to_string(directory.join("errors-0.jsonl")).unwrap();
            assert_eq!(file.lines().count(), 2, "重复故障只落首现与摘要两条");
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
            // 无法归类的外层错误被跳过，根因描述进入 message。
            assert!(
                record.message.contains("database is locked"),
                "{}",
                record.message
            );
            assert_eq!(record.request_id, logging::request_id());
            assert!(!record.thread_id.is_empty());
        }
        "wrapper" => {
            /// 包装 I/O 错误以验证日志能跳过外层描述并保留安全根因。
            #[derive(Debug)]
            struct Wrapped(std::io::Error);
            impl std::fmt::Display for Wrapped {
                /// 为包装错误提供不含底层细节的固定文案，以测试日志根因提取。
                /// 入参：self（&Wrapped）持有 I/O 错误但本方法不读取；formatter（&mut Formatter）为调用方提供的格式化目标。
                /// 返回值：fmt::Result；固定文案写入成功为 Ok(())，目标拒绝写入则透传 fmt::Error。
                /// 异常：不主动 panic；格式化错误通过返回值传播。
                /// 注意事项：该文案用于验证日志能够继续查找底层错误，不作为实际 I/O 根因说明。
                fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    write!(formatter, "未分类包装错误")
                }
            }
            impl std::error::Error for Wrapped {
                /// 向日志分类逻辑暴露包装错误内部的真实 I/O 根因。
                /// 入参：self（&Wrapped）为持有 std::io::Error 的测试包装对象，无其他参数。
                /// 返回值：始终 Some(&self.0)，引用有效期受 self 约束，不复制或转移错误。
                /// 异常：无 Result 错误，也不主动 panic。
                /// 注意事项：只暴露一层底层错误；调用方负责决定是否继续遍历及如何隐藏敏感文案。
                fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                    Some(&self.0)
                }
            }
            logging::failure(
                Level::Error,
                "test",
                "wrapped",
                "WRAPPED_FAILURE",
                &Wrapped(std::io::Error::new(std::io::ErrorKind::TimedOut, "inner")),
                &[],
            );
            let records = logging::snapshot().unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].code, "IO_TIMED_OUT");
            assert_eq!(records[0].message, "I/O 错误：TimedOut");
            assert!(records[0].causes.is_empty());
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
            // 子进程退出前必须排空队列，否则并发计数会丢失尾部记录。
            logging::flush().unwrap();
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
            logging::flush().unwrap();
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
            logging::flush().unwrap();
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
            logging::flush().unwrap();
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
