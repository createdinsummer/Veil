mod common;

use common::TestEnv;
use predicates::prelude::*;

#[test]
fn shell_help_and_exit_work() {
    let env = TestEnv::new("test-password");
    let link = env.init("shell-help");

    env.command()
        .args(["shell", &env.path(&link)])
        .write_stdin("help\nexit\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("可用命令"))
        .stdout(predicate::str::contains("ls"))
        .stdout(predicate::str::contains("info"))
        .stdout(predicate::str::contains("add"))
        .stdout(predicate::str::contains("rm"))
        .stdout(predicate::str::contains("ex"))
        .stdout(predicate::str::contains("退出 shell"));
}

#[test]
fn shell_add_and_list_work() {
    let env = TestEnv::new("test-password");
    let link = env.init("shell-add");
    let source = env.write_file("note.txt", "shell content");

    env.command()
        .args(["shell", &env.path(&link)])
        .write_stdin(format!("add {}\nls\nexit\n", source.display()))
        .assert()
        .success()
        .stdout(predicate::str::contains("已添加"))
        .stdout(predicate::str::contains("note.txt"));
}

#[test]
fn shell_remove_updates_workspace() {
    let env = TestEnv::new("test-password");
    let link = env.init("shell-rm");
    let source = env.write_file("delete-me.txt", "content");

    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success();

    env.command()
        .args(["shell", &env.path(&link)])
        .write_stdin("rm delete-me.txt\nls\nexit\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("已删除"))
        .stdout(predicate::str::contains("(空)"));
}

#[test]
fn shell_export_round_trip() {
    let env = TestEnv::new("test-password");
    let link = env.init("shell-export");
    let source = env.write_file("input.txt", "export me");
    let output = env.work.path().join("output.txt");

    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success();

    env.command()
        .args(["shell", &env.path(&link)])
        .write_stdin(format!("ex input.txt {}\nexit\n", output.display()))
        .assert()
        .success()
        .stdout(predicate::str::contains("已导出"));

    assert_eq!(std::fs::read_to_string(output).unwrap(), "export me");
}

#[test]
fn shell_reports_unknown_commands_without_exiting() {
    let env = TestEnv::new("test-password");
    let link = env.init("shell-unknown");

    env.command()
        .args(["shell", &env.path(&link)])
        .write_stdin("not-a-command\nquit\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("未知命令"))
        .stdout(predicate::str::contains("再见"));
}
