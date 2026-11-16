//! 本机 SQLite 配置、链接缓存及跨安装重新登记。
//! CLI 配置、空间登记与链接缓存的集成测试。
//! 所属模块：本机 app.db 持久化；验证用户偏好、跨安装登记、缓存修复和链接身份保持。
//! 使用 TestEnv 创建隔离的 HOME 和容器目录，避免读写用户真实配置。
mod common;
use common::TestEnv;
use predicates::prelude::*;
use veil_service::{link::VeilLink, storage::app::AppStore};
#[test]
/// 验证 config 命令展示数据库配置，并能读取 / 更新有类型的用户设置。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
/// 验证旧版配置文件不会被新 CLI 读取或意外改写。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn old_config_is_not_read_or_modified() {
    let env = TestEnv::new("password");
    let path = env.home.path().join(".veil/config.jsonc");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"invalid old config").unwrap();
    env.command().args(["init", "demo"]).assert().success();
    assert_eq!(std::fs::read(path).unwrap(), b"invalid old config");
}
#[test]
/// 验证链接缓存丢失时可从数据库重建，容器稳定身份和创建时间保持不变。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
/// 验证另一个独立安装登记现有容器时需要通过解锁确认身份。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
/// 验证从缓存生成链接时沿用容器原始创建时间。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
/// 验证缓存内容无效时不把坏数据恢复成可用链接。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
/// 验证操作一个容器时不会顺带恢复同一空间内未选中的兄弟容器链接。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
/// 验证帮助和命令行偏好覆盖遵循当前界面语言。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
/// 验证重复容器登记必须显式移除冲突记录后才能恢复。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
/// 验证首次打包提示按用户偏好只显示和保存一次。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
/// 验证成功解锁后以加密容器数据库中的名称刷新缓存显示值。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
