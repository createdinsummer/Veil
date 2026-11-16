//! 交互式 shell 子命令的端到端测试。
//! 所属模块：veil-cli 会话命令；覆盖帮助、退出、文件增删、列表、导出和未知命令处理。
//! 核心函数：各 shell_* 用例使用 TestEnv 构造进程，写入多行命令并核对输出或导出结果。
//! 依赖能力：公共隔离测试环境、CLI 二进制、标准输入管道及 predicates 断言。
//! 业务目标：确认多条命令可在同一会话衔接执行，单次输入错误按约定报告。

mod common;

use common::TestEnv;
use predicates::prelude::*;

/// 验证 shell 的帮助输出和退出命令。
#[test]
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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

/// 验证 shell 内可以添加文件并列出结果。
#[test]
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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

/// 验证 shell 删除命令会更新容器内容。
#[test]
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn shell_remove_updates_work() {
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

/// 验证 shell 导出命令与原始内容一致。
#[test]
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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

/// 验证未知命令只输出提示，不会提前退出 shell。
#[test]
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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

#[test]
/// 验证交互 Shell 导出目录时复用共享文件流水线并保留目录层级。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn shell_exports_a_folder_through_the_shared_pipeline() {
    let env = TestEnv::new("test-password");
    let link = env.init("shell-folder-export");
    env.write_file("tree/nested/one.txt", "one");
    env.write_file("tree/two.txt", "two");
    let source = env.work.path().join("tree");
    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success();
    let output = env.work.path().join("out");
    env.command()
        .args(["shell", &env.path(&link)])
        .write_stdin(format!("ex tree {}\nexit\n", output.display()))
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(output.join("tree/nested/one.txt")).unwrap(),
        "one"
    );
    assert_eq!(
        std::fs::read_to_string(output.join("tree/two.txt")).unwrap(),
        "two"
    );
}
