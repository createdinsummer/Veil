//! CLI 与共享日志服务之间的错误归属和诊断导出测试。
//! 所属模块：命令行错误处理；验证参数错误、交互 Shell 退出和服务错误不会被重复记录。
//! 同时覆盖错误日志导出保留 I/O 原因并拒绝覆盖已有文件的约定。
#[allow(dead_code)]
mod common;
use common::TestEnv;

#[test]
/// 验证命令行参数校验失败只向用户报告，不写入生产错误日志。
/// 入参：无，由测试运行器调用；内部使用独占 TestEnv 和故意无效的选项、缺失的 ex 参数。
/// 返回值：()；两个命令均失败且日志文件不存在或内容为空时正常结束。
/// 异常：进程断言、临时环境创建或日志读取失败会 panic，使测试失败。
/// 注意事项：固定敏感路径和密码仅用于测试；不执行真实导出，也不证明其他错误路径都不会记录敏感信息。
fn argument_errors_do_not_fill_production_logs() {
    let env = TestEnv::new("secret-log-test-password");
    env.command()
        .args(["--unknown-secret-path=/private/confidential.txt"])
        .assert()
        .failure();
    env.command().args(["ex"]).assert().failure();
    let path = env.home.path().join("app/logs/errors-0.jsonl");
    assert!(!path.exists() || std::fs::read_to_string(path).unwrap().is_empty());
}

#[test]
/// 验证 Shell 中单条输入校验失败只提示用户，交互会话继续且不重复记为系统错误。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
/// 异常：测试准备或断言失败会 panic；场景中预期的错误由测试体捕获或核对，不作为生产错误返回。
fn shell_validation_errors_do_not_end_session_or_emit_errors() {
    let env = TestEnv::new("secret-log-test-password");
    let link = env.init("logging-shell");
    env.command()
        .args(["shell", &env.path(link)])
        .write_stdin("unknown-secret-command\nadd\nexit\n")
        .assert()
        .success();
    let text = std::fs::read_to_string(env.home.path().join("app/logs/errors-0.jsonl"))
        .unwrap_or_default();
    assert!(!text.contains("UNKNOWN_COMMAND"));
    assert!(!text.contains("MISSING_ARGUMENT"));
    assert!(!text.contains("unknown-secret-command"));
    assert!(!text.contains("secret-log-test-password"));
}

#[test]
/// 验证 Shell 收到输入结束信号后正常退出，不出现空循环占用 CPU。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn shell_eof_finishes_without_spinning() {
    let env = TestEnv::new("secret-log-test-password");
    let link = env.init("logging-eof");
    env.command()
        .args(["shell", &env.path(link)])
        .write_stdin("help\n")
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success();
}

#[test]
/// 验证服务层已记录的失败不会再被 CLI 重复写入一次。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn service_failure_is_not_logged_again_by_cli() {
    let env = TestEnv::new("secret-log-test-password");
    let link = env.init("logging-service");
    env.command_with_password("wrong-sensitive-password")
        .args(["list", &env.path(link)])
        .assert()
        .failure();
    let export = env.work.path().join("diagnostics.json");
    env.command()
        .args(["logs", "--output", &env.path(&export)])
        .assert()
        .success();
    let text = std::fs::read_to_string(export).unwrap();
    let records: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
    assert!(
        records
            .iter()
            .any(|record| record["component"] == "service")
    );
    assert!(!records.iter().any(|record| record["component"] == "cli"));
    assert!(!text.contains("wrong-sensitive-password"));
    assert!(!text.contains("secret-log-test-password"));
}

#[test]
/// 验证日志导出保留底层文件错误原因，并拒绝覆盖已有目标文件。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
/// 异常：测试准备或断言失败会 panic；场景中预期的错误由测试体捕获或核对，不作为生产错误返回。
fn diagnostic_export_preserves_io_cause_and_never_overwrites_existing_files() {
    let env = TestEnv::new("secret-log-test-password");
    env.command()
        .args([
            "logs",
            "--output",
            "missing-private-parent/private-export.json",
        ])
        .assert()
        .failure();
    let export = env.work.path().join("diagnostics.json");
    env.command()
        .args(["logs", "--output", &env.path(&export)])
        .assert()
        .success();
    let text = std::fs::read_to_string(&export).unwrap();
    let records: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap();
    let record = records
        .iter()
        .find(|record| record["event"] == "LOG_EXPORT_FAILED")
        .unwrap();
    assert_eq!(record["operation"], "create_log_export");
    assert_eq!(record["code"], "IO_NOT_FOUND");
    assert!(record["native_code"].is_i64());
    assert!(record["request_id"].is_string());
    assert!(!text.contains("private-export.json"));
    assert!(!text.contains("secret-log-test-password"));
    env.command()
        .args(["logs", "--output", &env.path(&export)])
        .assert()
        .failure();
    assert_eq!(std::fs::read_to_string(&export).unwrap(), text);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(export).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
