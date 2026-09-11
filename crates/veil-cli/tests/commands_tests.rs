mod common;

use common::TestEnv;
use predicates::prelude::*;

#[test]
fn init_creates_workspace_and_link() {
    let env = TestEnv::new("test-password");

    env.command()
        .args(["init", "photos", "test-password"])
        .assert()
        .success()
        .stdout(predicate::str::contains("创建容器成功"));

    let link = env.link_path("photos");
    assert!(link.exists());

    let content = std::fs::read_to_string(link).unwrap();
    assert!(content.contains("veil_id = \"veil-"));
    assert!(content.contains("container_name = \"photos\""));
    assert!(content.contains("volume_id = "));
    assert!(content.contains("volume_label = "));
}

#[test]
fn duplicate_init_fails() {
    let env = TestEnv::new("test-password");
    env.init("photos");

    env.command()
        .args(["init", "photos", "test-password"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("已存在"));
}

#[test]
fn add_list_and_info_accept_link() {
    let env = TestEnv::new("test-password");
    let link = env.init("documents");
    let source = env.write_file("notes.txt", "hello veil");

    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success()
        .stdout(predicate::str::contains("文件已添加"));

    env.command()
        .args(["free", &env.path(&link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("notes.txt"))
        .stdout(predicate::str::contains("文件数量: 1"));

    env.command()
        .args(["list", &env.path(&link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("notes.txt"));

    env.command()
        .args(["info", &env.path(&link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("容器名称: documents"))
        .stdout(predicate::str::contains("notes.txt"));
}

#[test]
fn rename_and_remove_use_workspace_metadata() {
    let env = TestEnv::new("test-password");
    let link = env.init("archive");
    let source = env.write_file("old.txt", "content");

    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success();

    env.command()
        .args(["mv", &env.path(&link), "old.txt", "new.txt"])
        .assert()
        .success()
        .stdout(predicate::str::contains("new.txt"));

    env.command()
        .args(["rm", &env.path(&link), "new.txt"])
        .assert()
        .success()
        .stdout(predicate::str::contains("已删除"));

    env.command()
        .args(["free", &env.path(&link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("(空)"));
}

#[test]
fn export_round_trip() {
    let env = TestEnv::new("test-password");
    let link = env.init("exports");
    let source = env.write_file("source.txt", "original content");
    let output = env.work.path().join("exported.txt");

    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success();

    env.command()
        .args(["ex", &env.path(&link), "source.txt", &env.path(&output)])
        .assert()
        .success()
        .stdout(predicate::str::contains("文件已导出"));

    assert_eq!(std::fs::read_to_string(output).unwrap(), "original content");
}

#[test]
fn wrong_password_is_rejected() {
    let env = TestEnv::new("correct-password");
    let link = env.init("secure");

    env.command_with_password("wrong-password")
        .args(["free", &env.path(&link)])
        .assert()
        .failure()
        .stderr(predicate::str::contains("解密失败").or(predicate::str::contains("密码错误")));
}

#[test]
fn change_password_rekeys_workspace() {
    let env = TestEnv::new("old-password");
    let link = env.init("rotate");
    let source = env.write_file("secret.txt", "secret");

    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success();

    env.command()
        .args(["passwd", &env.path(&link), "old-password", "new-password"])
        .assert()
        .success()
        .stdout(predicate::str::contains("密码修改成功"));

    env.command_with_password("old-password")
        .args(["free", &env.path(&link)])
        .assert()
        .failure();

    env.command_with_password("new-password")
        .args(["free", &env.path(&link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("secret.txt"));
}

#[test]
fn pack_and_unpack_create_reusable_links() {
    let env = TestEnv::new("test-password");
    let link = env.init("package");
    let source = env.write_file("payload.txt", "payload");

    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success();

    env.command()
        .args(["pack", &env.path(&link)])
        .assert()
        .success();

    let package = env.work.path().join("package.vault.veil");
    assert!(package.exists());

    env.command()
        .args([
            "unpack",
            &env.path(&package),
            "-n",
            "restored",
            "-p",
            "test-password",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("已解包容器"));

    let restored_link = env.link_path("restored");
    assert!(restored_link.exists());

    env.command()
        .args(["free", &env.path(&restored_link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("payload.txt"));
}

#[test]
fn nonexistent_container_fails_cleanly() {
    let env = TestEnv::new("test-password");

    env.command()
        .args(["free", "missing.veil-link"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("找不到"));
}
