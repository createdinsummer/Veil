//! Shell 模式集成测试

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

fn setup_test_env() -> TempDir {
    TempDir::new().unwrap()
}

fn cmd_with_password(password: &str) -> Command {
    let mut cmd = Command::cargo_bin("veil").unwrap();
    cmd.env("VEIL_PASSWORD", password);
    cmd
}

#[test]
fn test_shell_help_command() {
    let temp_dir = setup_test_env();
    let container_path = temp_dir.path().join("test.veil");

    // 创建容器
    cmd_with_password("testpass")
        .args(&["init", &container_path.to_string_lossy(), "testpass"])
        .assert()
        .success();

    // 测试 help 命令
    let mut cmd = cmd_with_password("testpass");
    cmd.args(&["shell", &container_path.to_string_lossy()]);
    cmd.write_stdin("help\nexit\n");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("可用命令"))
        .stdout(predicate::str::contains("文件操作"))
        .stdout(predicate::str::contains("add"))
        .stdout(predicate::str::contains("rm"))
        .stdout(predicate::str::contains("mv"))
        .stdout(predicate::str::contains("free"))
        .stdout(predicate::str::contains("info"))
        .stdout(predicate::str::contains("exit"));
}

#[test]
fn test_shell_add_and_list() {
    let temp_dir = setup_test_env();
    let container_path = temp_dir.path().join("test.veil");
    let test_file = temp_dir.path().join("test.txt");

    fs::write(&test_file, "test content").unwrap();

    // 创建容器
    cmd_with_password("testpass")
        .args(&["init", &container_path.to_string_lossy(), "testpass"])
        .assert()
        .success();

    // Shell 模式：添加文件并查看
    let mut cmd = cmd_with_password("testpass");
    cmd.args(&["shell", &container_path.to_string_lossy()]);
    cmd.write_stdin(format!("add {}\nfree\nexit\n", test_file.display()));

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("文件已添加"))
        .stdout(predicate::str::contains("test.txt"))
        .stdout(predicate::str::contains("容器内容"));
}

#[test]
fn test_shell_batch_operations() {
    let temp_dir = setup_test_env();
    let container_path = temp_dir.path().join("test.veil");
    let file1 = temp_dir.path().join("file1.txt");
    let file2 = temp_dir.path().join("file2.txt");

    fs::write(&file1, "content 1").unwrap();
    fs::write(&file2, "content 2").unwrap();

    // 创建容器
    cmd_with_password("testpass")
        .args(&["init", &container_path.to_string_lossy(), "testpass"])
        .assert()
        .success();

    // 批量操作
    let mut cmd = cmd_with_password("testpass");
    cmd.args(&["shell", &container_path.to_string_lossy()]);
    cmd.write_stdin(format!(
        "add {}\nadd {} data/file2.txt\nfree\nexit\n",
        file1.display(),
        file2.display()
    ));

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("file1.txt"))
        .stdout(predicate::str::contains("data/file2.txt"))
        .stdout(predicate::str::contains("文件数量: 2"));
}

#[test]
fn test_shell_rm_command() {
    let temp_dir = setup_test_env();
    let container_path = temp_dir.path().join("test.veil");
    let test_file = temp_dir.path().join("test.txt");

    fs::write(&test_file, "content").unwrap();

    // 创建容器并添加文件
    cmd_with_password("testpass")
        .args(&["init", &container_path.to_string_lossy(), "testpass"])
        .assert()
        .success();

    cmd_with_password("testpass")
        .args(&["add", &container_path.to_string_lossy(), &test_file.to_string_lossy()])
        .assert()
        .success();

    // Shell 模式：删除文件
    let mut cmd = cmd_with_password("testpass");
    cmd.args(&["shell", &container_path.to_string_lossy()]);
    cmd.write_stdin("rm test.txt\nfree\nexit\n");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("已删除"))
        .stdout(predicate::str::contains("文件数量: 0"));
}

#[test]
fn test_shell_mv_command() {
    let temp_dir = setup_test_env();
    let container_path = temp_dir.path().join("test.veil");
    let test_file = temp_dir.path().join("test.txt");

    fs::write(&test_file, "content").unwrap();

    // 创建容器并添加文件
    cmd_with_password("testpass")
        .args(&["init", &container_path.to_string_lossy(), "testpass"])
        .assert()
        .success();

    cmd_with_password("testpass")
        .args(&["add", &container_path.to_string_lossy(), &test_file.to_string_lossy()])
        .assert()
        .success();

    // Shell 模式：移动文件
    let mut cmd = cmd_with_password("testpass");
    cmd.args(&["shell", &container_path.to_string_lossy()]);
    cmd.write_stdin("mv test.txt renamed.txt\nfree\nexit\n");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("已移动"))
        .stdout(predicate::str::contains("renamed.txt"));
}

#[test]
fn test_shell_info_command() {
    let temp_dir = setup_test_env();
    let container_path = temp_dir.path().join("test.veil");

    // 创建容器
    cmd_with_password("testpass")
        .args(&["init", &container_path.to_string_lossy(), "testpass"])
        .assert()
        .success();

    // Shell 模式：查看信息
    let mut cmd = cmd_with_password("testpass");
    cmd.args(&["shell", &container_path.to_string_lossy()]);
    cmd.write_stdin("info\nexit\n");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("容器信息"))
        .stdout(predicate::str::contains("文件数量"));
}

#[test]
fn test_shell_export_command() {
    let temp_dir = setup_test_env();
    let container_path = temp_dir.path().join("test.veil");
    let input_file = temp_dir.path().join("input.txt");
    let output_file = temp_dir.path().join("output.txt");

    let test_content = "export test content";
    fs::write(&input_file, test_content).unwrap();

    // 创建容器并添加文件
    cmd_with_password("testpass")
        .args(&["init", &container_path.to_string_lossy(), "testpass"])
        .assert()
        .success();

    cmd_with_password("testpass")
        .args(&["add", &container_path.to_string_lossy(), &input_file.to_string_lossy(), "test.txt"])
        .assert()
        .success();

    // Shell 模式：导出文件
    let mut cmd = cmd_with_password("testpass");
    cmd.args(&["shell", &container_path.to_string_lossy()]);
    cmd.write_stdin(format!("ex test.txt {}\nexit\n", output_file.display()));

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("文件已导出"));

    // 验证导出内容
    let exported_content = fs::read_to_string(&output_file).unwrap();
    assert_eq!(exported_content, test_content);
}

#[test]
fn test_shell_unknown_command() {
    let temp_dir = setup_test_env();
    let container_path = temp_dir.path().join("test.veil");

    // 创建容器
    cmd_with_password("testpass")
        .args(&["init", &container_path.to_string_lossy(), "testpass"])
        .assert()
        .success();

    // Shell 模式：未知命令（错误输出到 stderr）
    let mut cmd = cmd_with_password("testpass");
    cmd.args(&["shell", &container_path.to_string_lossy()]);
    cmd.write_stdin("unknown_command\nexit\n");

    cmd.assert()
        .success()
        .stderr(predicate::str::contains("未知命令"));
}

#[test]
fn test_shell_ls_alias() {
    let temp_dir = setup_test_env();
    let container_path = temp_dir.path().join("test.veil");

    // 创建容器
    cmd_with_password("testpass")
        .args(&["init", &container_path.to_string_lossy(), "testpass"])
        .assert()
        .success();

    // Shell 模式：使用 ls 别名
    let mut cmd = cmd_with_password("testpass");
    cmd.args(&["shell", &container_path.to_string_lossy()]);
    cmd.write_stdin("ls\nexit\n");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("容器内容"))
        .stdout(predicate::str::contains("统计信息"));
}

#[test]
fn test_shell_quit_alias() {
    let temp_dir = setup_test_env();
    let container_path = temp_dir.path().join("test.veil");

    // 创建容器
    cmd_with_password("testpass")
        .args(&["init", &container_path.to_string_lossy(), "testpass"])
        .assert()
        .success();

    // Shell 模式：使用 quit 别名
    let mut cmd = cmd_with_password("testpass");
    cmd.args(&["shell", &container_path.to_string_lossy()]);
    cmd.write_stdin("quit\n");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("正在退出"));
}
