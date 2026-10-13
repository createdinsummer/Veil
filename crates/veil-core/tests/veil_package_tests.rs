//! 打包格式与缓存密码验证器的集成测试。

use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;
use veil_core::error::VeilError;
use veil_core::metadata::{AlgorithmId, MetaHeader};
use veil_core::veil_ops::VeilManager;
use veil_core::veil_package::{VEIL_PACKAGE_MAGIC, VeilPacker, VeilUnpacker};

fn package_fixture(temp: &TempDir) -> PathBuf {
    veil_core::kdf::enable_fast_test_kdf();
    let manager = VeilManager::new(temp.path().join("work"));
    manager.init_veil("test", "default", "1").unwrap();
    let source = temp.path().join("secret.txt");
    fs::write(&source, b"package content").unwrap();
    manager.add_file(&source, "1").unwrap();
    let metadata = manager.read_meta("1").unwrap();
    let bytes = fs::read(manager.veil_dir.join(".veil-meta")).unwrap();
    let package = temp.path().join("test.veil");
    VeilPacker::new(&package)
        .pack(&manager.veil_dir, &metadata, &bytes)
        .unwrap();
    package
}

/// 验证密码测试复用缓存，不依赖每次重新读取包文件。
#[test]
fn password_verifier_is_cached_and_read_only() {
    let temp = TempDir::new().unwrap();
    let package = package_fixture(&temp);
    let before = fs::read(&package).unwrap();
    let unpacker = VeilUnpacker::new(&package);
    let verifier = unpacker.read_password_verifier().unwrap();
    assert!(!verifier.matches_password("wrong"));
    assert!(verifier.matches_password("1"));
    let (metadata, _) = unpacker.read_metadata("1").unwrap();
    assert_eq!(metadata.files[0].original_name, "secret.txt");
    assert_eq!(fs::read(&package).unwrap(), before);
    fs::remove_file(package).unwrap();
    assert!(verifier.matches_password("1"));
}

/// 损坏的认证标签不能被正确密码验证通过。
#[test]
fn damaged_metadata_does_not_match_password() {
    let temp = TempDir::new().unwrap();
    let package = package_fixture(&temp);
    let mut bytes = fs::read(&package).unwrap();
    let metadata_size = u32::from_le_bytes(bytes[14..18].try_into().unwrap()) as usize;
    bytes[22 + metadata_size - 1] ^= 1;
    fs::write(&package, bytes).unwrap();
    let verifier = VeilUnpacker::new(package).read_password_verifier().unwrap();
    assert!(!verifier.matches_password("1"));
}

/// 无认证标签的元数据在载入阶段被拒绝。
#[test]
fn truncated_metadata_is_rejected_before_password_test() {
    let temp = TempDir::new().unwrap();
    let package = package_fixture(&temp);
    let mut bytes = fs::read(&package).unwrap();
    let header_len = MetaHeader::header_len(&bytes[22..]).unwrap();
    bytes[14..18].copy_from_slice(&(header_len as u32).to_le_bytes());
    bytes.truncate(22 + header_len);
    fs::write(&package, bytes).unwrap();
    let error = VeilUnpacker::new(package)
        .read_password_verifier()
        .err()
        .unwrap();
    assert!(matches!(error, VeilError::InvalidFormat(_)));
    assert!(error.to_string().contains("加密元数据不完整"));
}

/// 不能按当前算法解密头部声明的其他算法。
#[test]
fn unsupported_metadata_algorithm_is_rejected() {
    let temp = TempDir::new().unwrap();
    let package = package_fixture(&temp);
    let mut bytes = fs::read(&package).unwrap();
    let mut header = MetaHeader::from_bytes(&bytes[22..]).unwrap();
    header.algorithm = AlgorithmId::Aes256Gcm;
    let header_bytes = header.to_bytes().unwrap();
    bytes[22..22 + header_bytes.len()].copy_from_slice(&header_bytes);
    fs::write(&package, bytes).unwrap();
    let error = VeilUnpacker::new(package)
        .read_password_verifier()
        .err()
        .unwrap();
    assert!(error.to_string().contains("不支持的元数据加密算法"));
}

/// 旧独立格式即使魔数和版本相同，也不能被当成当前打包格式。
#[test]
fn legacy_single_file_header_is_rejected() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("legacy.veil");
    let mut bytes = VEIL_PACKAGE_MAGIC.to_vec();
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // 旧格式的 KDF flags。
    bytes.extend_from_slice(&5u32.to_le_bytes());
    bytes.extend_from_slice(b"1.1.2");
    bytes.extend_from_slice(&32u32.to_le_bytes());
    bytes.extend_from_slice(&[0u8; 32]);
    fs::write(&path, bytes).unwrap();
    let error = VeilUnpacker::new(path)
        .read_password_verifier()
        .err()
        .unwrap();
    assert!(error.to_string().contains("仅支持 veil pack"));
}
