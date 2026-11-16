//! 比较服务公开接口的集成测试。
//! 所属模块：跨容器 / 外部文件内容比对；验证哈希、内容、版本三类检查组合及不确定结果。
//! 测试会创建隔离容器和临时文件，确认比较操作不把缺失元数据误判为相同。
use tempfile::TempDir;
use veil_core::{database::CONTAINER_DB, version::VersionSnapshot};
use veil_service::{
    comparison::{ComparisonChecks, ComparisonState, ConflictOutcome, FileInput, compare_files},
    veil_ops::{AddFileSpec, VeilManager},
};

#[test]
/// 验证密文、明文和外部文件的各类输入组合能按请求的检查项返回正确状态。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn all_input_combinations_support_selected_checks_and_missing_versions() {
    let root = TempDir::new().unwrap();
    let a = VeilManager::new(root.path().join("a"));
    let b = VeilManager::new(root.path().join("b"));
    a.init_veil("a", "password").unwrap();
    b.init_veil("b", "password").unwrap();
    let source = root.path().join("source");
    let contents = vec![42u8; 1024 * 1024 + 37];
    std::fs::write(&source, &contents).unwrap();
    for manager in [&a, &b] {
        manager
            .add_files(&[AddFileSpec::new(&source, "file")], "password")
            .unwrap();
    }
    let meta = a.read_meta("password").unwrap();
    let version = VersionSnapshot {
        veil_id: meta.veil_id.clone(),
        file_id: meta.files[0].file_id.clone(),
        version_history: meta.files[0].version_history.clone(),
    };
    let all = ComparisonChecks {
        hash: true,
        content: true,
        version: true,
    };
    let cipher = |manager| FileInput::ContainerCiphertext {
        manager,
        path: "file",
        password: "password",
    };
    let plain = || FileInput::ContainerPlaintext {
        bytes: &contents,
        version: Some(&version),
    };
    let external = || FileInput::External {
        path: &source,
        version: Some(&version),
    };
    for report in [
        compare_files(cipher(&a), plain(), all).unwrap(),
        compare_files(plain(), cipher(&a), all).unwrap(),
        compare_files(cipher(&a), external(), all).unwrap(),
        compare_files(external(), cipher(&a), all).unwrap(),
        compare_files(plain(), external(), all).unwrap(),
        compare_files(external(), plain(), all).unwrap(),
        compare_files(plain(), plain(), all).unwrap(),
        compare_files(external(), external(), all).unwrap(),
        compare_files(cipher(&a), cipher(&a), all).unwrap(),
    ] {
        assert_eq!(report.outcome, ConflictOutcome::NoConflict);
        assert_eq!(report.hash, Some(ComparisonState::Same));
        assert_eq!(report.content, Some(ComparisonState::Same));
        assert_eq!(report.version, Some(ComparisonState::Same));
    }
    let report = compare_files(cipher(&a), cipher(&b), all).unwrap();
    assert_eq!(report.hash, Some(ComparisonState::Same));
    assert_eq!(report.content, Some(ComparisonState::Same));
    assert_eq!(report.version, Some(ComparisonState::Different));
    assert_eq!(report.outcome, ConflictOutcome::Conflict);
    let version_only = ComparisonChecks {
        version: true,
        ..Default::default()
    };
    let report = compare_files(cipher(&a), plain(), version_only).unwrap();
    assert_eq!(report.outcome, ConflictOutcome::NoConflict);
    assert_eq!((report.hash, report.content), (None, None));
    let report = compare_files(
        cipher(&a),
        FileInput::External {
            path: &source,
            version: None,
        },
        version_only,
    )
    .unwrap();
    assert_eq!(report.outcome, ConflictOutcome::Indeterminate);
    let report = compare_files(
        cipher(&a),
        FileInput::External {
            path: &source,
            version: None,
        },
        all,
    )
    .unwrap();
    assert_eq!(report.version, Some(ComparisonState::Unavailable));
    assert_eq!(report.outcome, ConflictOutcome::Indeterminate);
    std::fs::write(&source, b"different").unwrap();
    let report = compare_files(
        cipher(&a),
        FileInput::External {
            path: &source,
            version: None,
        },
        all,
    )
    .unwrap();
    assert_eq!(report.hash, Some(ComparisonState::Different));
    assert_eq!(report.content, Some(ComparisonState::Different));
    assert_eq!(report.version, Some(ComparisonState::Unavailable));
    assert_eq!(report.outcome, ConflictOutcome::Conflict);
    assert!(compare_files(plain(), plain(), ComparisonChecks::default()).is_err());
}

#[test]
/// 验证容器缺少摘要缓存时仍可解密内容完成比较，且普通读取不会写入摘要缓存。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn missing_container_file_hash_falls_back_to_decrypted_content() {
    let root = TempDir::new().unwrap();
    let manager = VeilManager::new(root.path().join("veil"));
    manager.init_veil("test", "password").unwrap();
    let source = root.path().join("source");
    std::fs::write(&source, b"plain content").unwrap();
    manager
        .add_files(&[AddFileSpec::new(&source, "file")], "password")
        .unwrap();
    let db = veil_core::database::open(
        &manager.veil_dir.join(CONTAINER_DB),
        Some("password"),
        false,
    )
    .unwrap();
    db.execute("DELETE FROM file_hash", []).unwrap();
    drop(db);
    assert!(manager.list_files("password").unwrap()[0].hash.is_none());
    let report = compare_files(
        FileInput::ContainerCiphertext {
            manager: &manager,
            password: "password",
            path: "file",
        },
        FileInput::External {
            path: &source,
            version: None,
        },
        ComparisonChecks {
            hash: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(report.hash, Some(ComparisonState::Same));
    assert_eq!(report.outcome, ConflictOutcome::NoConflict);
    // 普通读锁不能写缓存，但缺失缓存不影响本次比较。
    assert!(manager.list_files("password").unwrap()[0].hash.is_none());
}

#[test]
/// 验证空文件仍参与比较，并且密文损坏会被识别为错误而非相同内容。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
/// 异常：测试准备或断言失败会 panic；场景中预期的错误由测试体捕获或核对，不作为生产错误返回。
fn empty_content_is_compared_and_corruption_is_detected() {
    let root = TempDir::new().unwrap();
    let manager = VeilManager::new(root.path().join("veil"));
    manager.init_veil("test", "password").unwrap();
    let source = root.path().join("empty");
    std::fs::write(&source, b"").unwrap();
    manager
        .add_files(&[AddFileSpec::new(&source, "empty")], "password")
        .unwrap();
    let check = || {
        compare_files(
            FileInput::ContainerCiphertext {
                manager: &manager,
                password: "password",
                path: "empty",
            },
            FileInput::External {
                path: &source,
                version: None,
            },
            ComparisonChecks {
                content: true,
                ..Default::default()
            },
        )
    };
    assert_eq!(check().unwrap().outcome, ConflictOutcome::NoConflict);
    let entry = manager.list_files("password").unwrap().remove(0);
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(manager.veil_dir.join(entry.encrypted_name))
        .unwrap()
        .write_all(b"extra")
        .unwrap();
    assert!(check().is_err());
}

#[test]
/// 验证冲突结果能区分路径冲突、文件内容状态和因缺少信息无法执行的检查。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn conflict_entries_distinguish_paths_files_and_unavailable_checks() {
    use veil_service::comparison::{check_file_conflict, check_path_conflict};
    #[cfg(debug_assertions)]
    veil_core::kdf::enable_fast_test_kdf();
    let root = TempDir::new().unwrap();
    let manager = VeilManager::new(root.path().join("container"));
    manager.init_veil("test", "password").unwrap();
    let source = root.path().join("source");
    std::fs::write(&source, b"same").unwrap();
    let hash = ComparisonChecks {
        hash: true,
        ..Default::default()
    };
    let conflict = |path: &str, checks| {
        check_file_conflict(
            &manager,
            path,
            "password",
            FileInput::External {
                path: &source,
                version: None,
            },
            checks,
            None,
        )
    };
    assert!(!check_path_conflict(&manager, "file", "password", None).unwrap());
    assert_eq!(conflict("file", hash).unwrap(), ConflictOutcome::NoConflict);
    assert!(conflict("file", ComparisonChecks::default()).is_err());
    manager
        .add_files(&[AddFileSpec::new(&source, "file")], "password")
        .unwrap();
    manager.create_directory("folder", "password").unwrap();
    assert!(check_path_conflict(&manager, "file", "password", None).unwrap());
    assert!(check_path_conflict(&manager, "folder", "password", None).unwrap());
    assert_eq!(conflict("folder", hash).unwrap(), ConflictOutcome::Conflict);
    assert_eq!(conflict("file", hash).unwrap(), ConflictOutcome::NoConflict);
    let combined = ComparisonChecks {
        hash: true,
        version: true,
        ..Default::default()
    };
    assert_eq!(
        conflict("file", combined).unwrap(),
        ConflictOutcome::Indeterminate
    );
    std::fs::write(&source, b"diff").unwrap();
    assert_eq!(conflict("file", hash).unwrap(), ConflictOutcome::Conflict);
    assert_eq!(
        conflict("file", combined).unwrap(),
        ConflictOutcome::Conflict
    );
}

#[test]
/// 验证只请求元数据时按容器身份和版本比较，不额外读取文件内容。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn metadata_inputs_compare_identity_and_versions_without_reading_content() {
    let base = VersionSnapshot {
        veil_id: "container".into(),
        file_id: "file".into(),
        version_history: vec!["00000000-0000-4000-8000-000000000001".into()],
    };
    let compare = |other: &VersionSnapshot, checks| {
        compare_files(
            FileInput::ContainerMetadata {
                version: &base,
                hash: None,
            },
            FileInput::ContainerMetadata {
                version: other,
                hash: None,
            },
            checks,
        )
    };
    let version_only = ComparisonChecks {
        version: true,
        ..Default::default()
    };
    assert_eq!(
        compare(&base, version_only).unwrap().version,
        Some(ComparisonState::Same)
    );
    let mut other = base.clone();
    other.veil_id = "another-container".into();
    assert_eq!(
        compare(&other, version_only).unwrap().version,
        Some(ComparisonState::Different)
    );
    other = base.clone();
    other.file_id = "another-file".into();
    assert_eq!(
        compare(&other, version_only).unwrap().version,
        Some(ComparisonState::Different)
    );
    other = base.clone();
    other
        .version_history
        .push("00000000-0000-4000-8000-000000000002".into());
    assert_eq!(
        compare(&other, version_only).unwrap().version,
        Some(ComparisonState::Different)
    );
    other.version_history.clear();
    assert!(compare(&other, version_only).is_err());
    assert!(
        compare(
            &base,
            ComparisonChecks {
                hash: true,
                ..Default::default()
            }
        )
        .is_err()
    );
    assert!(
        compare(
            &base,
            ComparisonChecks {
                content: true,
                ..Default::default()
            }
        )
        .is_err()
    );
}
