#[allow(dead_code)]
mod common;
use common::TestEnv;

#[test]
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
