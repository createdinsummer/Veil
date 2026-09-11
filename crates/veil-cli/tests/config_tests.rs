mod common;

use common::TestEnv;
use predicates::prelude::*;

#[test]
fn config_show_matches_documented_command() {
    let env = TestEnv::new("test-password");

    env.command()
        .args(["config"])
        .assert()
        .success()
        .stdout(predicate::str::contains("当前配置"))
        .stdout(predicate::str::contains("提示级别: off"));

    env.command()
        .args(["config", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("当前配置"))
        .stdout(predicate::str::contains("提示级别: off"));
}

#[test]
fn config_hints_updates_future_command_output() {
    let env = TestEnv::new("test-password");

    env.command()
        .env("VEIL_HINTS", "")
        .args(["config", "--hints", "off"])
        .assert()
        .success()
        .stdout(predicate::str::contains("已设置提示级别: off"));

    let config = std::fs::read_to_string(env.home.path().join(".veil/config.toml")).unwrap();
    assert!(config.contains("version = \"1.0\""));
    assert!(config.contains("hints_level = \"off\""));

    env.command()
        .env("VEIL_HINTS", "")
        .args(["init", "demo", "test-password"])
        .assert()
        .success()
        .stdout(predicate::str::contains("只是数据入口，不保存数据").not());
}

#[test]
fn init_creates_a_reusable_veil_link() {
    let env = TestEnv::new("test-password");

    env.command()
        .env("VEIL_LANG", "en")
        .env("VEIL_HINTS", "full")
        .args(["init", "demo", "test-password"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Container created successfully: demo",
        ))
        .stdout(predicate::str::contains(
            "demo.veil-link is just a data entry point and does not store data",
        ))
        .stdout(predicate::str::contains("Data is stored in the workspace:"))
        .stdout(predicate::str::contains("/.veil/workspaces/default/veil-"))
        .stdout(predicate::str::contains("veil pack demo.veil-link"))
        .stdout(predicate::str::contains("veil unpack <file.veil>"))
        .stdout(predicate::str::contains("veil config --hints off"));

    let link_path = env.link_path("demo");
    assert!(link_path.exists());
    let link = std::fs::read_to_string(&link_path).unwrap();
    assert!(link.contains("veil_id = \"veil-"));
    assert!(link.contains("container_name = \"demo\""));
    assert!(link.contains("volume_id = "));

    let source = env.write_file("note.txt", "hello");
    env.command()
        .args(["add", &env.path(&link_path), &env.path(&source)])
        .assert()
        .success();

    env.command()
        .args(["info", &env.path(&link_path)])
        .assert()
        .success()
        .stdout(predicate::str::contains("note.txt"));
}

#[test]
fn init_guidance_is_shown_for_each_new_container() {
    let env = TestEnv::new("test-password");

    for name in ["first", "second"] {
        env.command()
            .env("VEIL_LANG", "en")
            .env("VEIL_HINTS", "full")
            .args(["init", name])
            .assert()
            .success()
            .stdout(predicate::str::contains(
                "is just a data entry point and does not store data",
            ))
            .stdout(predicate::str::contains("veil config --hints off"))
            .stdout(predicate::str::contains("veil pack"))
            .stdout(predicate::str::contains("veil unpack <file.veil>"));
    }
}

#[test]
fn missing_link_is_rebuilt_from_config() {
    let env = TestEnv::new("test-password");
    let primary_link = env.init("demo");

    env.command()
        .args(["link", &env.path(&primary_link), "-o", "custom.veil-link"])
        .assert()
        .success();

    let link_path = env.work.path().join("custom.veil-link");
    let original = std::fs::read(&link_path).unwrap();
    let original_hex = original
        .iter()
        .map(|byte| format!("{:02x}", byte))
        .collect::<String>();
    let config = std::fs::read_to_string(env.home.path().join(".veil/config.toml")).unwrap();
    assert!(config.contains(&format!("raw_hex = \"{}\"", original_hex)));
    assert!(config.contains("volume_id = "));
    assert!(config.contains("mount_path = "));

    std::fs::rename(&link_path, env.work.path().join("custom.veil-link.bak")).unwrap();

    env.command()
        .env("VEIL_HINTS", "brief")
        .args(["free", "custom.veil-link"])
        .assert()
        .success()
        .stdout(predicate::str::contains("已为你重建"));

    assert!(link_path.exists());
    assert_eq!(std::fs::read(&link_path).unwrap(), original);
}

#[test]
fn help_files_and_hints_override_are_localized() {
    let env = TestEnv::new("test-password");

    env.command()
        .env("VEIL_LANG", "en")
        .args(["help", "files"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Shortcut / bookmark"));

    env.command()
        .env("VEIL_LANG", "en")
        .env("VEIL_HINTS", "brief")
        .args(["config", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Hint level: brief"))
        .stdout(predicate::str::contains("VEIL_HINTS=brief"));
}
