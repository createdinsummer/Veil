//! 集成测试：测试 CLI 的各个命令功能

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

/// 创建临时测试环境
fn setup_test_env() -> TempDir {
    TempDir::new().unwrap()
}

/// 创建带密码环境变量的命令
fn cmd_with_password(password: &str) -> Command {
    let mut cmd = Command::cargo_bin("veil").unwrap();
    cmd.env("VEIL_PASSWORD", password);
    cmd
}

#[test]
fn test_help_command() {
    let mut cmd = Command::cargo_bin("veil").unwrap();
    cmd.arg("--help");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Veil"))
        .stdout(predicate::str::contains("init"))
        .stdout(predicate::str::contains("add"))
        .stdout(predicate::str::contains("free"));
}

#[test]
fn test_version_command() {
    let mut cmd = Command::cargo_bin("veil").unwrap();
    cmd.arg("--version");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("veil 0.1.0"));
}

#[test]
fn test_init_creates_container() {
    let temp_dir = setup_test_env();
    let container_path = temp_dir.path().join("test.veil");

    let mut cmd = cmd_with_password("testpassword");
    cmd.arg("init").arg(&container_path);

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("容器创建成功"));

    // 验证文件已创建
    assert!(container_path.exists());
}

#[test]
fn test_init_duplicate_fails() {
    let temp_dir = setup_test_env();
    let container_path = temp_dir.path().join("test.veil");

    // 第一次创建成功
    cmd_with_password("testpassword")
        .arg("init")
        .arg(&container_path)
        .assert()
        .success();

    // 第二次创建应该失败
    let mut cmd = cmd_with_password("testpassword");
    cmd.arg("init").arg(&container_path);

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("容器文件已存在"));
}

#[test]
fn test_add_file() {
    let temp_dir = setup_test_env();
    let container_path = temp_dir.path().join("test.veil");
    let test_file = temp_dir.path().join("test.txt");

    // 创建测试文件
    fs::write(&test_file, "Hello, Veil!").unwrap();

    // 创建容器
    cmd_with_password("testpass")
        .arg("init")
        .arg(&container_path)
        .assert()
        .success();

    // 添加文件
    let mut cmd = cmd_with_password("testpass");
    cmd.arg("add")
        .arg(&container_path)
        .arg(&test_file)
        .arg("myfile.txt");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("文件已添加"));
}

#[test]
fn test_free_shows_content() {
    let temp_dir = setup_test_env();
    let container_path = temp_dir.path().join("test.veil");
    let test_file = temp_dir.path().join("test.txt");

    fs::write(&test_file, "test content").unwrap();

    // 创建容器并添加文件
    cmd_with_password("testpass")
        .arg("init")
        .arg(&container_path)
        .assert()
        .success();

    cmd_with_password("testpass")
        .arg("add")
        .arg(&container_path)
        .arg(&test_file)
        .assert()
        .success();

    // 查看内容
    let mut cmd = cmd_with_password("testpass");
    cmd.arg("free").arg(&container_path);

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("容器内容"))
        .stdout(predicate::str::contains("test.txt"));
}

#[test]
fn test_export_file() {
    let temp_dir = setup_test_env();
    let container_path = temp_dir.path().join("test.veil");
    let test_file = temp_dir.path().join("input.txt");
    let output_file = temp_dir.path().join("output.txt");

    let test_content = "Export test content";
    fs::write(&test_file, test_content).unwrap();

    // 创建容器并添加文件
    cmd_with_password("testpass")
        .arg("init")
        .arg(&container_path)
        .assert()
        .success();

    cmd_with_password("testpass")
        .arg("add")
        .arg(&container_path)
        .arg(&test_file)
        .arg("myfile.txt")
        .assert()
        .success();

    // 导出文件
    cmd_with_password("testpass")
        .arg("ex")
        .arg(&container_path)
        .arg("myfile.txt")
        .arg(&output_file)
        .assert()
        .success()
        .stdout(predicate::str::contains("文件已导出"));

    // 验证导出内容
    let exported_content = fs::read_to_string(&output_file).unwrap();
    assert_eq!(exported_content, test_content);
}

#[test]
fn test_remove_file() {
    let temp_dir = setup_test_env();
    let container_path = temp_dir.path().join("test.veil");
    let test_file = temp_dir.path().join("test.txt");

    fs::write(&test_file, "content").unwrap();

    // 创建容器并添加文件
    cmd_with_password("testpass")
        .arg("init")
        .arg(&container_path)
        .assert()
        .success();

    cmd_with_password("testpass")
        .arg("add")
        .arg(&container_path)
        .arg(&test_file)
        .assert()
        .success();

    // 删除文件
    cmd_with_password("testpass")
        .arg("rm")
        .arg(&container_path)
        .arg("test.txt")
        .assert()
        .success()
        .stdout(predicate::str::contains("已删除"));
}

#[test]
fn test_move_file() {
    let temp_dir = setup_test_env();
    let container_path = temp_dir.path().join("test.veil");
    let test_file = temp_dir.path().join("test.txt");

    fs::write(&test_file, "content").unwrap();

    // 创建容器并添加文件
    cmd_with_password("testpass")
        .arg("init")
        .arg(&container_path)
        .assert()
        .success();

    cmd_with_password("testpass")
        .arg("add")
        .arg(&container_path)
        .arg(&test_file)
        .assert()
        .success();

    // 移动文件
    cmd_with_password("testpass")
        .arg("mv")
        .arg(&container_path)
        .arg("test.txt")
        .arg("moved.txt")
        .assert()
        .success()
        .stdout(predicate::str::contains("已移动"));
}

#[test]
fn test_wrong_password_fails() {
    let temp_dir = setup_test_env();
    let container_path = temp_dir.path().join("test.veil");

    // 创建容器
    cmd_with_password("correctpass")
        .arg("init")
        .arg(&container_path)
        .assert()
        .success();

    // 使用错误密码尝试查看内容
    let mut cmd = cmd_with_password("wrongpass");
    cmd.arg("free").arg(&container_path);

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("密码错误").or(predicate::str::contains("解密失败")));
}

#[test]
fn test_change_password() {
    let temp_dir = setup_test_env();
    let container_path = temp_dir.path().join("test.veil");

    // 创建容器
    cmd_with_password("oldpass")
        .arg("init")
        .arg(&container_path)
        .assert()
        .success();

    // 修改密码
    let mut cmd = cmd_with_password("oldpass");
    cmd.env("VEIL_NEW_PASSWORD", "newpass");
    cmd.arg("passwd").arg(&container_path);

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("密码修改成功"));

    // 使用新密码验证
    cmd_with_password("newpass")
        .arg("free")
        .arg(&container_path)
        .assert()
        .success();
}

#[test]
fn test_info_command() {
    let temp_dir = setup_test_env();
    let container_path = temp_dir.path().join("test.veil");

    // 创建容器
    cmd_with_password("testpass")
        .arg("init")
        .arg(&container_path)
        .assert()
        .success();

    // 查看信息
    cmd_with_password("testpass")
        .arg("info")
        .arg(&container_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("容器信息"))
        .stdout(predicate::str::contains("文件数量"));
}
