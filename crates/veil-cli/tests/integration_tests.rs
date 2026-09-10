mod common;

use assert_cmd::Command;
use common::TestEnv;
use predicates::prelude::*;

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

#[test]
fn version_matches_workspace_release() {
    Command::cargo_bin("veil")
        .unwrap()
        .args(["--version"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2.0.0"));
}

#[test]
fn init_accepts_custom_link_path() {
    let env = TestEnv::new("test-password");
    let custom_link = env.work.path().join("project/photos.veil-link");
    std::fs::create_dir_all(custom_link.parent().unwrap()).unwrap();

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
        .stdout(predicate::str::contains("专属工作区"));

    assert!(link.exists());
    assert!(workspace.join(".veil-meta").exists());

    let source = env.write_file("secret.txt", "secret");
    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success();
}

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

    assert!(portable_dir
        .join(".veil/workspaces/default/photos/.veil-meta")
        .exists());

    let link_content = std::fs::read_to_string(link).unwrap();
    assert!(link_content.contains(".veil/workspaces/default/photos"));
    assert!(!link_content.contains("mount_path = "));
}

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
