//! SQLCipher 快照的便携包与密码验证。
use std::fs;
use tempfile::TempDir;
use veil_core::veil_package::VeilUnpacker as RawUnpacker;
use veil_service::{
    package::{VeilPacker, VeilUnpacker},
    veil_ops::VeilManager,
};
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
    assert!(
        VeilUnpacker::new(&package)
            .unpack_verified(&root.path().join("out"), "1")
            .is_err()
    );
}
