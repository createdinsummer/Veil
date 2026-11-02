//! 本机 SQLite 配置、链接缓存及跨安装重新登记。
mod common;
use common::TestEnv;
use predicates::prelude::*;
use veil_service::{link::VeilLink, storage::app::AppStore};
#[test]
fn config_show_and_typed_settings() {
    let env = TestEnv::new("password");
    env.command()
        .args(["config"])
        .assert()
        .success()
        .stdout(predicate::str::contains("app.db"));
    env.command()
        .env("VEIL_HINTS", "")
        .args(["config", "--hints", "brief"])
        .assert()
        .success();
    let store = AppStore::open(&env.home.path().join("app")).unwrap();
    assert_eq!(store.user_setting("hint_level").unwrap(), "brief");
    assert!(!env.home.path().join(".veil/config.jsonc").exists());
}
#[test]
fn old_config_is_not_read_or_modified() {
    let env = TestEnv::new("password");
    let path = env.home.path().join(".veil/config.jsonc");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"invalid old config").unwrap();
    env.command().args(["init", "demo"]).assert().success();
    assert_eq!(std::fs::read(path).unwrap(), b"invalid old config");
}
#[test]
fn missing_link_is_rebuilt_from_database_and_keeps_identity() {
    let env = TestEnv::new("password");
    let path = env.init("demo");
    let original = VeilLink::load(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    env.command()
        .args(["list", &env.path(&path)])
        .assert()
        .success();
    let recovered = VeilLink::load(&path).unwrap();
    assert_eq!(original.veil.veil_id, recovered.veil.veil_id);
    assert_eq!(original.veil.created_at, recovered.veil.created_at);
    let store = AppStore::open(&env.home.path().join("app")).unwrap();
    assert_eq!(
        store
            .records::<veil_service::config::LinkRecord>("link_cache", 100)
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn independent_install_requires_unlock_for_registration() {
    let env = TestEnv::new("password");
    let original = env.init("demo");
    let identity = VeilLink::load(&original).unwrap();
    let directory = env.veil_dir(&original);
    let fresh = tempfile::TempDir::new().unwrap();
    let link = env.work.path().join("fresh.veil-link");
    env.command()
        .env("VEIL_APP_DATA_DIR", fresh.path())
        .args(["link", &env.path(&directory), "-o", &env.path(&link)])
        .assert()
        .success();
    assert_eq!(
        VeilLink::load(&link).unwrap().veil.veil_id,
        identity.veil.veil_id
    );
    let store = AppStore::open(fresh.path()).unwrap();
    assert_eq!(store.registrations().unwrap().len(), 1);
}
#[test]
fn original_creation_time_survives_cached_link_generation() {
    let env = TestEnv::new("password");
    let original = env.init("demo");
    let identity = VeilLink::load(&original).unwrap();
    let output = env.work.path().join("second.veil-link");
    env.command()
        .env_remove("VEIL_PASSWORD")
        .args(["link", "demo", "-o", &env.path(&output)])
        .assert()
        .success();
    assert_eq!(
        VeilLink::load(&output).unwrap().veil.created_at,
        identity.veil.created_at
    );
}
#[test]
fn invalid_cached_body_is_not_restored() {
    let env = TestEnv::new("password");
    let path = env.init("demo");
    std::fs::remove_file(&path).unwrap();
    let store = AppStore::open(&env.home.path().join("app")).unwrap();
    assert!(
        store
            .set_record(
                "link_cache",
                &env.path(&path),
                &serde_json::json!({"invalid":true})
            )
            .is_err()
    );
    let sql = veil_core::database::open(&env.home.path().join("app/app.db"), None, false).unwrap();
    sql.execute(
        "UPDATE local_record SET value_json='{\"invalid\":true}' WHERE record_type='link_cache'",
        [],
    )
    .unwrap();
    env.command()
        .args(["list", &env.path(&path)])
        .assert()
        .failure();
    assert!(!path.exists());
}
#[test]
fn operating_one_container_does_not_restore_sibling() {
    let env = TestEnv::new("password");
    let first = env.init("first");
    let second = env.init("second");
    std::fs::remove_file(&second).unwrap();
    env.command()
        .args(["list", &env.path(first)])
        .assert()
        .success();
    assert!(!second.exists());
}
#[test]
fn help_and_preference_override_are_localized() {
    let env = TestEnv::new("password");
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
        .stdout(predicate::str::contains("Hint level: brief"));
}
#[test]
fn duplicate_registration_must_be_explicitly_removed_before_restore() {
    let env = TestEnv::new("password");
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
        .failure();
    AppStore::open(&env.home.path().join("app"))
        .unwrap()
        .unregister(&identity.veil.veil_id)
        .unwrap();
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
    assert_eq!(
        VeilLink::load(&env.link_path("restored"))
            .unwrap()
            .veil
            .veil_id,
        identity.veil.veil_id
    );
}
#[test]
fn pack_hint_is_persisted_once() {
    let env = TestEnv::new("password");
    env.init("demo");
    env.command()
        .env("VEIL_HINTS", "full")
        .args(["pack", "demo", "-o", "first.veil"])
        .assert()
        .success()
        .stdout(predicate::str::contains(".veil-link 和 .veil 的区别"));
    env.command()
        .env("VEIL_HINTS", "full")
        .args(["pack", "demo", "-o", "second.veil"])
        .assert()
        .success()
        .stdout(predicate::str::contains(".veil-link 和 .veil 的区别").not());
}

#[test]
fn unlocking_refreshes_the_authoritative_container_name() {
    let env = TestEnv::new("password");
    let link = env.init("original");
    let directory = env.veil_dir(&link);
    veil_service::veil_ops::VeilManager::new(directory)
        .rename_container("renamed", "password")
        .unwrap();
    env.command()
        .args(["info", &env.path(&link)])
        .assert()
        .success();
    env.command().args(["list", "renamed"]).assert().success();
    let store = AppStore::open(&env.home.path().join("app")).unwrap();
    assert_eq!(store.registrations().unwrap()[0].cached_name, "renamed");
}
