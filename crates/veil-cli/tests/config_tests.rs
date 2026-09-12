//! CLI 配置、链接恢复和本地化行为测试。

mod common;

use common::TestEnv;
use predicates::prelude::*;

/// 验证不带参数的配置命令与显式 `show` 输出一致。
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

/// 验证修改提示级别会写入配置并影响后续命令输出。
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

/// 验证初始化生成的链接可再次用于访问同一容器。
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
    assert!(link.contains("algorithm = \"ChaCha20-Poly1305\""));

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

/// 验证每个新容器都会显示独立的首次使用引导。
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

/// 验证链接缺失时会依据配置中的原始字节副本恢复。
#[test]
fn missing_link_is_rebuilt_from_config() {
    let env = TestEnv::new("test-password");
    let primary_link = env.init("demo");

    env.command()
        .args(["link", &env.path(&primary_link), "-o", "custom.veil-link"])
        .assert()
        .success();

    let link_path = env.work.path().join("custom.veil-link");
    // 保存原始链接字节，并确认配置确实缓存了同一份十六进制内容。
    let original = std::fs::read(&link_path).unwrap();
    let original_hex = original
        .iter()
        .map(|byte| format!("{:02x}", byte))
        .collect::<String>();
    let config = std::fs::read_to_string(env.home.path().join(".veil/config.toml")).unwrap();
    assert!(config.contains(&format!("raw_hex = \"{}\"", original_hex)));
    assert!(config.contains("volume_id = "));
    assert!(config.contains("mount_path = "));

    // 移走链接而非删除，便于必要时人工检查备份。
    std::fs::rename(&link_path, env.work.path().join("custom.veil-link.bak")).unwrap();

    env.command()
        .env("VEIL_HINTS", "brief")
        .args(["free", "custom.veil-link"])
        .assert()
        .success()
        .stdout(predicate::str::contains("已为你重建"));

    // 恢复后必须与原始字节完全一致，而不是重新序列化出的近似内容。
    assert!(link_path.exists());
    assert_eq!(std::fs::read(&link_path).unwrap(), original);
}

/// 验证操作一个容器不会顺带恢复其他缺失链接。
#[test]
fn operating_one_container_does_not_restore_sibling_link() {
    let env = TestEnv::new("test-password");
    let first_link = env.init("first");
    let second_link = env.init("second");
    let second_backup = env.work.path().join("second.veil-link.bak");

    std::fs::rename(&second_link, &second_backup).unwrap();

    env.command()
        .args(["free", &env.path(&first_link)])
        .assert()
        .success();

    assert!(!second_link.exists());
    assert!(second_backup.exists());
}

/// 验证文件帮助和提示级别环境变量均支持英文输出。
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
