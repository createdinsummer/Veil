//! CLI 配置、链接恢复和本地化行为测试。

mod common;

use common::TestEnv;
use predicates::prelude::*;
use veil_core::config::GlobalConfig;
use veil_core::link::VeilLink;

/// 验证不带参数的配置命令与显式 `show` 输出一致。
#[test]
fn config_show_matches_documented_command() {
    let env = TestEnv::new("test-password");

    env.command()
        .args(["config"])
        .assert()
        .success()
        .stdout(predicate::str::contains("当前配置"))
        .stdout(predicate::str::contains("提示级别: off"))
        .stdout(predicate::str::contains("配置路径:"))
        .stdout(predicate::str::contains("自定义工作区数量: 0"));

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

    let config = std::fs::read_to_string(env.home.path().join(".veil/config.jsonc")).unwrap();
    let config: GlobalConfig = veil_core::jsonc::from_str(&config).unwrap();
    assert_eq!(config.version, "1.0");
    assert_eq!(config.user_settings.hint_level.as_str(), "off");

    env.command()
        .env("VEIL_HINTS", "")
        .args(["init", "demo", "test-password"])
        .assert()
        .success()
        .stdout(predicate::str::contains("只是数据入口，不保存数据").not());
}

/// 手写的 JSONC 注释和尾逗号可被读取，修改用户设置及登记容器时仍保留注释。
#[test]
fn config_updates_preserve_user_comments() {
    let env = TestEnv::new("test-password");
    let path = env.home.path().join(".veil/config.jsonc");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"// 我的配置
{
  "user_settings": {
    // 提示级别
    "hint_level": "full", /* 保留这条说明 */
  },
}
"#,
    )
    .unwrap();

    env.command()
        .env("VEIL_HINTS", "")
        .args(["config", "--hints", "brief"])
        .assert()
        .success();
    env.init("demo");

    let content = std::fs::read_to_string(path).unwrap();
    for comment in ["// 我的配置", "// 提示级别", "/* 保留这条说明 */"] {
        assert!(content.contains(comment));
    }
    let config: GlobalConfig = veil_core::jsonc::from_str(&content).unwrap();
    assert_eq!(config.user_settings.hint_level.as_str(), "brief");
    assert_eq!(config.veils.len(), 1);
    assert_eq!(config.links.len(), 1);
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
        .stdout(predicate::str::contains("Veil created successfully: demo"))
        .stdout(predicate::str::contains(
            "demo.veil-link is just a data entry point and does not store data",
        ))
        .stdout(predicate::str::contains("Data is stored in the work:"))
        .stdout(predicate::str::contains("/.veil/works/default/veil-"))
        .stdout(predicate::str::contains("veil pack demo.veil-link"))
        .stdout(predicate::str::contains("veil unpack <file.veil>"))
        .stdout(predicate::str::contains("veil config --hints off"));

    let link_path = env.link_path("demo");
    assert!(link_path.exists());
    let link = VeilLink::load(&link_path).unwrap();
    assert!(link.veil.veil_id.starts_with("veil-"));
    assert_eq!(link.veil.veil_name, "demo");
    let veil_dir = env.veil_dir(&link_path);
    assert!(!veil_dir.parent().unwrap().join(".work-id").exists());
    let document = serde_json::to_value(&link).unwrap();
    assert!(document.get("work").is_none());

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

/// 重建链接更新链接时间，容器时间始终来自信息文件，且不要求输入密码。
#[test]
fn regenerated_links_preserve_container_creation_time() {
    veil_core::kdf::enable_fast_test_kdf();
    let env = TestEnv::new("test-password");
    let original_path = env.init("demo");
    let original = VeilLink::load(&original_path).unwrap();
    let veil_dir = env.veil_dir(&original_path);
    let manager =
        veil_core::veil_ops::VeilManager::for_veil(veil_dir.clone(), original.veil.veil_id.clone());
    assert_eq!(
        original.veil.created_at,
        manager.read_meta(&env.password).unwrap().created_at
    );

    // 使用固定的历史时间模拟已有容器，不依赖等待或时钟精度。
    let created_at = "2020-01-02T03:04:05Z";
    manager
        .update_meta(&env.password, |metadata| {
            metadata.created_at = created_at.to_string()
        })
        .unwrap();
    assert_eq!(manager.read_meta_header().unwrap().created_at, created_at);

    let registered_path = env.work.path().join("registered.veil-link");
    env.command()
        .env_remove("VEIL_PASSWORD")
        .args(["link", "demo", "-o", &env.path(&registered_path)])
        .assert()
        .success();

    // 丢失全局配置后，直接从容器目录重建也要保留容器 ID 和容器时间。
    std::fs::remove_file(env.home.path().join(".veil/config.jsonc")).unwrap();
    let detached_path = env.work.path().join("detached.veil-link");
    env.command()
        .env_remove("VEIL_PASSWORD")
        .args([
            "link",
            &env.path(&veil_dir),
            "-o",
            &env.path(&detached_path),
        ])
        .assert()
        .success();

    for path in [registered_path, detached_path] {
        let link = VeilLink::load(&path).unwrap();
        assert_eq!(link.veil.veil_id, original.veil.veil_id);
        assert_eq!(link.veil.created_at, created_at);
        assert_ne!(link.created_at, created_at);
    }
}

/// 容器整体移动到另一个已登记工作区后，原链接和 ID 继续访问相同数据。
#[test]
fn moving_container_preserves_original_link_and_id() {
    let env = TestEnv::new("test-password");
    let link_path = env.init("original");
    let link = VeilLink::load(&link_path).unwrap();
    let original_body = std::fs::read(&link_path).unwrap();
    let source = env.write_file("note.txt", "moved data");
    env.command()
        .args(["add", &env.path(&link_path), &env.path(&source)])
        .assert()
        .success();
    let old_dir = env.veil_dir(&link_path);
    let config_path = env.home.path().join(".veil/config.jsonc");
    let mut config = GlobalConfig::load_from(&config_path).unwrap();
    config
        .register_custom_work("archive".into(), "works/archive".into(), None)
        .unwrap();
    config.save().unwrap();
    let target = env.home.path().join(".veil/works/archive");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::rename(&old_dir, target.join(&link.veil.veil_id)).unwrap();
    for selector in [
        env.path(&link_path),
        link.veil.veil_id.clone(),
        "original".into(),
    ] {
        env.command()
            .args(["info", &selector])
            .assert()
            .success()
            .stdout(predicate::str::contains("note.txt"));
    }
    assert_eq!(std::fs::read(&link_path).unwrap(), original_body);
    assert!(!old_dir.exists());

    // 原位置出现同 ID 副本后，各种入口都必须拒绝默认选择。
    std::fs::create_dir_all(&old_dir).unwrap();
    std::fs::copy(
        target.join(&link.veil.veil_id).join(".veil-info"),
        old_dir.join(".veil-info"),
    )
    .unwrap();
    for selector in [
        env.path(&link_path),
        link.veil.veil_id.clone(),
        "original".into(),
    ] {
        env.command()
            .args(["info", &selector])
            .assert()
            .failure()
            .stderr(predicate::str::contains("[9017]"));
    }
    std::fs::remove_dir_all(&old_dir).unwrap();

    // 已登记位置不再有目标时，不创建替代容器，也不改写链接。
    std::fs::rename(&target, env.home.path().join("offline-archive")).unwrap();
    env.command()
        .args(["info", &env.path(&link_path)])
        .assert()
        .failure()
        .stderr(predicate::str::contains("当前未找到容器"));
    assert!(!target.exists());
    assert!(!old_dir.exists());
}

/// 验证每个新容器都会显示独立的首次使用引导。
#[test]
fn init_guidance_is_shown_for_each_new_veil() {
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

/// 验证链接缺失时会依据缓存正文重建，链接时间及容器信息保持一致。
#[test]
fn missing_link_is_rebuilt_from_config() {
    let env = TestEnv::new("test-password");
    let primary_link = env.init("demo");
    let content = std::fs::read_to_string(&primary_link).unwrap();
    std::fs::write(&primary_link, format!("// 原链接的自定义注释\n{content}")).unwrap();

    env.command()
        .args(["link", &env.path(&primary_link), "-o", "custom.veil-link"])
        .assert()
        .success();

    let link_path = env.work.path().join("custom.veil-link");
    let original = VeilLink::load(&link_path).unwrap();
    let config = std::fs::read_to_string(env.home.path().join(".veil/config.jsonc")).unwrap();
    let config: GlobalConfig = veil_core::jsonc::from_str(&config).unwrap();
    let stored_path = std::fs::canonicalize(&link_path).unwrap();
    let record = config
        .links
        .iter()
        .find(|record| std::fs::canonicalize(&record.link_path).unwrap() == stored_path)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&record.link).unwrap(),
        serde_json::to_value(&original).unwrap()
    );
    assert!(
        serde_json::to_value(&config)
            .unwrap()
            .get("volumes")
            .is_none()
    );

    // 移走链接而非删除，便于必要时人工检查备份。
    std::fs::rename(&link_path, env.work.path().join("custom.veil-link.bak")).unwrap();

    env.command()
        .env("VEIL_HINTS", "brief")
        .args(["free", "custom.veil-link"])
        .assert()
        .success()
        .stdout(predicate::str::contains("已为你重建"));

    // 重建保留完整正文，不要求保留原文件的注释和排版。
    assert!(link_path.exists());
    let restored = VeilLink::load(&link_path).unwrap();
    assert_eq!(
        serde_json::to_value(&restored).unwrap(),
        serde_json::to_value(&original).unwrap()
    );
    assert!(
        !std::fs::read_to_string(&link_path)
            .unwrap()
            .contains("原链接的自定义注释")
    );
}

/// 缓存正文中的身份或链接版本无效时，拒绝重建且不写出链接文件。
#[test]
fn invalid_cached_link_is_not_written() {
    for (section, field, value) in [("veil", "veil_id", "../outside"), ("veil", "veil_id", "")] {
        let env = TestEnv::new("test-password");
        let link_path = env.init("demo");
        let config_path = env.home.path().join(".veil/config.jsonc");
        let content = std::fs::read_to_string(&config_path).unwrap();
        let mut config: serde_json::Value = veil_core::jsonc::from_str(&content).unwrap();
        config["links"][0]["link"][section][field] = value.into();
        std::fs::write(&config_path, serde_json::to_string(&config).unwrap()).unwrap();
        std::fs::remove_file(&link_path).unwrap();

        env.command()
            .args(["free", &env.path(&link_path)])
            .assert()
            .failure();

        assert!(!link_path.exists());
    }
}

/// 验证操作一个容器不会顺带恢复其他缺失链接。
#[test]
fn operating_one_veil_does_not_restore_sibling_link() {
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

/// 容器删除后，旧缓存和旧链接不阻止解包恢复原 ID。
#[test]
fn unpack_restores_original_id_despite_stale_cache() {
    let env = TestEnv::new("test-password");
    let original = env.init("source");
    let identity = VeilLink::load(&original).unwrap();
    env.command()
        .args(["pack", "source", "-o", "backup.veil"])
        .assert()
        .success();
    std::fs::remove_dir_all(env.veil_dir(&original)).unwrap();
    env.command()
        .args([
            "unpack",
            "backup.veil",
            "-n",
            "restored",
            "--link",
            "restored.veil-link",
        ])
        .assert()
        .success();
    let restored = VeilLink::load(&env.link_path("restored")).unwrap();
    assert_eq!(restored.veil.veil_id, identity.veil.veil_id);
    assert_eq!(restored.veil.created_at, identity.veil.created_at);
    assert_eq!(restored.veil.veil_name, "restored");
    for selector in [
        "restored".into(),
        env.path(&original),
        identity.veil.veil_id,
    ] {
        env.command()
            .args(["info", &selector])
            .assert()
            .success()
            .stdout(predicate::str::contains("restored"));
    }
}
