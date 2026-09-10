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
