//! 主 CLI 参数、帮助、链接和便携工作区集成测试。

mod common;

use assert_cmd::Command;
use common::TestEnv;
use predicates::prelude::*;

/// 验证顶层帮助包含全部工作区命令。
#[test]
fn help_lists_workspace_model_commands() {
    Command::cargo_bin("veil")
        .unwrap()
        .args(["--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("init"))
        .stdout(predicate::str::contains("pack"))
        .stdout(predicate::str::contains("unpack"))
        .stdout(predicate::str::contains("link"))
        .stdout(predicate::str::contains("config"))
        .stdout(predicate::str::contains("help"));
}

/// 验证版本输出与 workspace 发布版本一致。
#[test]
fn version_matches_workspace_release() {
    Command::cargo_bin("veil")
        .unwrap()
        .args(["--version"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2.0.0"));
}

/// 验证初始化可使用自定义链接路径。
#[test]
fn init_accepts_custom_link_path() {
    let env = TestEnv::new("test-password");
    let custom_link = env.work.path().join("project/photos.veil-link");
    std::fs::create_dir_all(custom_link.parent().unwrap()).unwrap();

    // 从主链接复制出第二个链接，再通过第二个链接写入共享工作区。
    env.command()
        .args([
            "init",
            "photos",
            "test-password",
            "--link",
            &env.path(&custom_link),
        ])
        .assert()
        .success();

    assert!(custom_link.exists());
    let source = env.write_file("photo.txt", "image");

    env.command()
        .args(["add", &env.path(&custom_link), &env.path(&source)])
        .assert()
        .success();
}

/// 验证多个链接可以指向同一工作区并共享内容。
#[test]
fn multiple_links_can_target_the_same_workspace() {
    let env = TestEnv::new("test-password");
    let primary_link = env.init("shared");
    let secondary_link = env.work.path().join("second.veil-link");

    env.command()
        .args([
            "link",
            &env.path(&primary_link),
            "-o",
            &env.path(&secondary_link),
        ])
        .assert()
        .success();

    let source = env.write_file("shared.txt", "content");
    env.command()
        .args(["add", &env.path(&secondary_link), &env.path(&source)])
        .assert()
        .success();

    env.command()
        .args(["free", &env.path(&primary_link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("shared.txt"));
}

/// 验证主 CLI 支持创建专属工作区容器。
#[test]
fn dedicated_workspace_init_is_available_from_main_cli() {
    let env = TestEnv::new("test-password");
    let workspace = env.work.path().join("dedicated-volume");
    let link = env.work.path().join("vault.veil-link");

    env.command()
        .args([
            "init",
            "vault",
            "test-password",
            "--workspace-path",
            &env.path(&workspace),
            "--dedicated",
            "--link",
            &env.path(&link),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("创建容器成功"));

    // 专属模式下列表项与 .veil-meta 都应直接位于指定工作区路径。
    assert!(link.exists());
    assert!(workspace.join(".veil-meta").exists());

    let source = env.write_file("secret.txt", "secret");
    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success();
}

/// 验证同名容器由不同 `veil_id` 区分且内容互不干扰。
#[test]
fn duplicate_container_names_are_distinguished_by_veil_id() {
    let env = TestEnv::new("test-password");
    let first_workspace = env.work.path().join("disk-a");
    let second_workspace = env.work.path().join("disk-b");
    let first_link = env.work.path().join("first.veil-link");
    let second_link = env.work.path().join("second.veil-link");

    env.command()
        .args([
            "init",
            "photos",
            "test-password",
            "--workspace-path",
            &env.path(&first_workspace),
            "--link",
            &env.path(&first_link),
        ])
        .assert()
        .success();
    env.command()
        .args([
            "init",
            "photos",
            "test-password",
            "--workspace-path",
            &env.path(&second_workspace),
            "--link",
            &env.path(&second_link),
        ])
        .assert()
        .success();

    // 两个容器展示名相同，只有链接中持久化的 veil_id 能区分它们。
    let first = std::fs::read_to_string(&first_link).unwrap();
    let second = std::fs::read_to_string(&second_link).unwrap();
    let first_id = first
        .lines()
        .find(|line| line.starts_with("veil_id = "))
        .unwrap();
    let second_id = second
        .lines()
        .find(|line| line.starts_with("veil_id = "))
        .unwrap();
    assert_ne!(first_id, second_id);

    let source = env.write_file("note.txt", "content");
    env.command()
        .args(["add", &env.path(&first_link), &env.path(&source)])
        .assert()
        .success();

    env.command()
        .args(["free", &env.path(&first_link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("note.txt"));
    env.command()
        .args(["free", &env.path(&second_link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("(空)"));
}

/// 验证便携模式把链接和工作区放在同一路径下。
#[test]
fn portable_mode_keeps_link_and_workspace_on_the_same_path() {
    let env = TestEnv::new("test-password");
    let portable_dir = env.work.path().join("portable");
    std::fs::create_dir_all(&portable_dir).unwrap();
    let link = portable_dir.join("photos.veil-link");

    env.command()
        .args([
            "init",
            "photos",
            "test-password",
            "--portable",
            "--link",
            &env.path(&link),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("工作区将创建在"));

    // 便携模式的默认工作区根应位于链接所在目录内部。
    let workspace_root = portable_dir.join(".veil/workspaces/default");
    let container_dirs: Vec<_> = std::fs::read_dir(&workspace_root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("veil-"))
                && path.join(".veil-meta").exists()
        })
        .collect();
    assert_eq!(container_dirs.len(), 1);

    let link_content = std::fs::read_to_string(link).unwrap();
    assert!(link_content.contains(".veil/workspaces/default/veil-"));
    assert!(!link_content.contains("mount_path = "));
}

/// 验证文件类型帮助在英文环境下可用。
#[test]
fn help_files_is_available_in_english() {
    Command::cargo_bin("veil")
        .unwrap()
        .env("VEIL_LANG", "en")
        .args(["help", "files"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Shortcut / bookmark"))
        .stdout(predicate::str::contains("ZIP archive"));
}
