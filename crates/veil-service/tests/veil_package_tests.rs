//! SQLCipher 加密数据库快照的便携包及密码验证集成测试。
//! 所属模块：veil-service 备份/还原；检查包验证器使用的快照和受损输入处理。
//! 核心函数：fixture 创建包含 secret.txt 的包；其余用例检查密码、元数据和包文件状态。
//! 依赖能力：VeilManager、服务层和核心包读取器、临时目录及文件系统。
//! 业务目标：确认便携包可以复用加密快照验证密码，并守住不应改变源文件的边界。
use std::fs;
use tempfile::TempDir;
use veil_core::veil_package::VeilUnpacker as RawUnpacker;
use veil_service::{
    package::{VeilPacker, VeilUnpacker},
    veil_ops::VeilManager,
};
/// 创建包含 secret.txt 的容器并打包，供快照和密码校验测试使用。
/// 入参：root（&TempDir）为本用例独占临时根目录，需可写且没有同名测试产物。
/// 返回值：PathBuf，指向生成的 test.veil 包文件；源容器位于 root/veil。
/// 异常：容器初始化、写入明文、导入或打包失败会通过 unwrap panic。
/// 注意事项：固定密码为“1”，仅用于测试；返回路径的有效期取决于调用方持有的 TempDir。
fn fixture(root: &TempDir) -> std::path::PathBuf {
    let m = VeilManager::new(root.path().join("veil"));
    m.init_veil("测试", "1").unwrap();
    let source = root.path().join("secret.txt");
    fs::write(&source, b"content").unwrap();
    m.add_file(&source, "1").unwrap();
    let package = root.path().join("test.veil");
    VeilPacker::new(&package).pack_container(&m, "1").unwrap();
    package
}
#[test]
/// 验证包校验器从已读取的加密快照工作，包文件被移除后仍能完成当前验证。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn verifier_uses_encrypted_snapshot_after_package_removed() {
    let root = TempDir::new().unwrap();
    let package = fixture(&root);
    let before = fs::read(&package).unwrap();
    let u = VeilUnpacker::new(&package);
    let v = u.read_password_verifier().unwrap();
    assert!(!v.matches_password("wrong"));
    assert!(v.matches_password("1"));
    assert_eq!(
        u.read_metadata("1").unwrap().0.files[0].original_name,
        "secret.txt"
    );
    assert_eq!(before, fs::read(&package).unwrap());
    fs::remove_file(package).unwrap();
    assert!(v.matches_password("1"));
}
#[test]
/// 验证损坏、截断和旧格式包在解包前被识别并拒绝。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn corruption_truncation_and_old_format_are_rejected() {
    let root = TempDir::new().unwrap();
    let package = fixture(&root);
    let before = fs::read(&package).unwrap();
    let mut corrupt = before.clone();
    corrupt[22 + 128] ^= 1;
    fs::write(&package, corrupt).unwrap();
    assert!(
        !RawUnpacker::new(&package)
            .read_password_verifier()
            .unwrap()
            .matches_password("1")
    );
    fs::write(&package, &before[..25]).unwrap();
    assert!(RawUnpacker::new(&package).read_password_verifier().is_err());
    let mut old = before.clone();
    old[8..10].copy_from_slice(&1u16.to_le_bytes());
    fs::write(&package, old).unwrap();
    assert!(RawUnpacker::new(&package).read_password_verifier().is_err());
    fs::write(&package, [before, b"extra".to_vec()].concat()).unwrap();
    assert!(VeilUnpacker::new(&package)
        .unpack_verified(&root.path().join("out"), "1")
        .is_err());
}

#[test]
/// 验证解包只恢复持久化容器数据，不把源环境中的运行时锁带到新容器。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn unpack_discards_runtime_locks_from_package() {
    let root = TempDir::new().unwrap();
    let package = fixture(&root);
    let raw = root.path().join("raw");
    RawUnpacker::new(&package).unpack(&raw).unwrap();
    let raw_db = veil_core::database::open(&raw.join("veil.db"), Some("1"), false).unwrap();
    assert!(
        raw_db
            .query_row("SELECT count(*) FROM operation_locks", [], |row| row
                .get::<_, i64>(0))
            .unwrap()
            > 0
    );

    let restored = root.path().join("restored");
    VeilUnpacker::new(&package)
        .unpack_verified(&restored, "1")
        .unwrap();
    let restored_db =
        veil_core::database::open(&restored.join("veil.db"), Some("1"), false).unwrap();
    assert_eq!(
        restored_db
            .query_row("SELECT count(*) FROM operation_locks", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
