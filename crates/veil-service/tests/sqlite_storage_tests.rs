use tempfile::TempDir;
use veil_core::database::CONTAINER_DB;
use veil_service::{
    package::{VeilPacker, VeilUnpacker},
    storage::{app::AppStore, container::ContainerDb},
    veil_ops::{AddFileSpec, VeilManager},
};

#[test]
fn snapshot_copies_multiple_files_with_progress() {
    let root = TempDir::new().unwrap();
    let manager = VeilManager::new(root.path().join("source"));
    manager.init_veil("test", "password").unwrap();
    let mut files = Vec::new();
    for index in 0..8 {
        let source = root.path().join(format!("input-{index}"));
        std::fs::write(&source, vec![index as u8; 4096]).unwrap();
        files.push(AddFileSpec::new(source, format!("folder/file-{index}")));
    }
    manager.add_files(&files, "password").unwrap();
    let progress = veil_core::progress::Progress::default();
    let _scope = progress.begin("export_package");
    let destination = root.path().join("snapshot");
    assert_eq!(manager.snapshot_to(&destination, "password").unwrap(), 8);
    let snapshot = progress.snapshot();
    assert_eq!(snapshot.phase, "snapshot");
    assert!(
        snapshot
            .total_bytes
            .is_some_and(|total| total > 0 && total == snapshot.processed_bytes)
    );
    assert!((1..=4).contains(&snapshot.parallel_workers));
    let restored = VeilManager::new(destination);
    for index in [0, 3, 7] {
        assert_eq!(
            restored
                .read_file_bytes(&format!("folder/file-{index}"), "password", 4096)
                .unwrap(),
            vec![index as u8; 4096]
        );
    }
}
#[test]
fn encrypted_container_roundtrip_tree_versions_rekey_and_package() {
    let root = TempDir::new().unwrap();
    let dir = root.path().join("source");
    let manager = VeilManager::new(dir.clone());
    manager.init_veil("测试", "password").unwrap();
    let bytes = std::fs::read(dir.join(CONTAINER_DB)).unwrap();
    assert_ne!(&bytes[..16], b"SQLite format 3\0");
    assert!(manager.read_meta("wrong").is_err());
    let input = root.path().join("plain");
    std::fs::write(&input, b"hello").unwrap();
    manager
        .add_files(&[AddFileSpec::new(&input, "folder/a.txt")], "password")
        .unwrap();
    manager.create_directory("empty/sub", "password").unwrap();
    assert_eq!(
        manager
            .read_file_bytes("folder/a.txt", "password", 1024)
            .unwrap(),
        b"hello"
    );
    let first = manager.list_files("password").unwrap().remove(0);
    manager.move_path("folder", "renamed", "password").unwrap();
    assert_eq!(
        manager.list_files("password").unwrap()[0].version_history,
        first.version_history
    );
    manager.change_password("password", "new-password").unwrap();
    assert!(manager.read_meta("password").is_err());
    assert_eq!(
        manager.list_files("new-password").unwrap()[0].encrypted_name,
        first.encrypted_name
    );
    manager
        .change_password_full("new-password", "final-password")
        .unwrap();
    let final_file = manager.list_files("final-password").unwrap().remove(0);
    assert_eq!(first.file_id, final_file.file_id);
    assert_eq!(first.version_history, final_file.version_history);
    assert_ne!(first.encrypted_name, final_file.encrypted_name);
    let package = root.path().join("backup.veil");
    VeilPacker::new(&package)
        .pack_container(&manager, "final-password")
        .unwrap();
    let destination = root.path().join("restored");
    VeilUnpacker::new(package)
        .unpack_verified(&destination, "final-password")
        .unwrap();
    assert_eq!(
        VeilManager::new(destination.clone())
            .read_file_bytes("renamed/a.txt", "final-password", 1024)
            .unwrap(),
        b"hello"
    );
    assert!(
        ContainerDb::open(&destination, "final-password", None)
            .unwrap()
            .resolve("empty/sub")
            .unwrap()
            .is_some()
    );
}
#[test]
fn app_identity_settings_spaces_and_records() {
    let root = TempDir::new().unwrap();
    let mut store = AppStore::open(root.path()).unwrap();
    let device = store.device_id().unwrap();
    let default = store.default_space().unwrap();
    assert_ne!(default.id, "default");
    assert_eq!(store.default_space().unwrap().id, default.id);
    store.rename_space(&default.id, "改名").unwrap();
    assert!(
        store
            .create_space("重复", default.local_path.as_deref(), false)
            .is_err()
    );
    let dedicated = store.create_space("专属", None, true).unwrap();
    store.register(&dedicated.id, "veil-a", "A").unwrap();
    assert!(store.register(&dedicated.id, "veil-b", "B").is_err());
    store
        .set_user_setting("layout", &serde_json::json!("list"))
        .unwrap();
    assert!(
        store
            .set_user_setting("layout", &serde_json::json!(7))
            .is_err()
    );
    store.set_record("hint_state", "a", &true).unwrap();
    store.set_record("hint_state", "a", &false).unwrap();
    assert_eq!(store.records::<bool>("hint_state", 10).unwrap(), [false]);
    drop(store);
    assert_eq!(
        AppStore::open(root.path()).unwrap().device_id().unwrap(),
        device
    );
}

#[test]
fn missing_legacy_and_future_schema_are_explicitly_rejected() {
    let root = TempDir::new().unwrap();
    let dir = root.path().join("legacy");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join(".veil-info"), b"old").unwrap();
    let m = VeilManager::new(dir.clone());
    assert!(m.read_meta("pw").is_err());
    assert!(m.init_veil("test", "pw").is_err());
    assert!(!dir.join(CONTAINER_DB).exists());
    assert_eq!(std::fs::read(dir.join(".veil-info")).unwrap(), b"old");
    let app = root.path().join("app");
    drop(AppStore::open(&app).unwrap());
    let db = veil_core::database::open(&app.join("app.db"), None, false).unwrap();
    db.execute_batch("PRAGMA user_version=99").unwrap();
    assert!(AppStore::open(&app).is_err());
}
#[test]
fn directory_constraints_and_content_base_identity_are_checked() {
    use rusqlite::params;
    let root = TempDir::new().unwrap();
    let m = VeilManager::new(root.path().join("veil"));
    let meta = m.init_veil("test", "pw").unwrap();
    m.create_directory("a/b/c", "pw").unwrap();
    m.create_directory("other", "pw").unwrap();
    assert!(m.move_path("a", "a/b/c", "pw").is_err());
    let db = ContainerDb::open(&m.veil_dir, "pw", None).unwrap();
    let root_id = db.root().unwrap().id;
    let before = db.resolve("a/b/c").unwrap().unwrap().id;
    drop(db);
    m.move_path("a", "renamed", "pw").unwrap();
    assert_eq!(
        ContainerDb::open(&m.veil_dir, "pw", None)
            .unwrap()
            .resolve("renamed/b/c")
            .unwrap()
            .unwrap()
            .id,
        before
    );
    let sql = veil_core::database::open(&m.veil_dir.join(CONTAINER_DB), Some("pw"), false).unwrap();
    assert!(sql.execute("INSERT INTO file_info(id,parent_id,name,kind,created_at,updated_at) VALUES('second-root',NULL,'','directory',0,0)",[]).is_err());
    assert!(sql.execute("INSERT INTO file_info(id,parent_id,name,kind,created_at,updated_at) VALUES('bad',?1,'..','directory',0,0)",[&root_id]).is_err());
    assert!(
        sql.execute("DELETE FROM file_info WHERE id=?1", [&root_id])
            .is_err()
    );
    drop(sql);
    let source = root.path().join("source");
    std::fs::write(&source, b"data").unwrap();
    m.add_files(
        &[veil_service::veil_ops::AddFileSpec::new(&source, "file")],
        "pw",
    )
    .unwrap();
    let file = m.list_files("pw").unwrap().remove(0);
    let valid = veil_core::version::VersionSnapshot {
        veil_id: meta.veil_id.clone(),
        file_id: file.file_id.clone(),
        version_history: file.version_history.clone(),
    };
    for base in [
        veil_core::version::VersionSnapshot {
            veil_id: "other".into(),
            ..valid.clone()
        },
        veil_core::version::VersionSnapshot {
            file_id: "other".into(),
            ..valid.clone()
        },
        veil_core::version::VersionSnapshot {
            version_history: vec![veil_core::metadata::generate_version_id()],
            ..valid.clone()
        },
        veil_core::version::VersionSnapshot {
            version_history: [
                valid.version_history.clone(),
                vec![veil_core::metadata::generate_version_id()],
            ]
            .concat(),
            ..valid.clone()
        },
    ] {
        assert!(
            m.add_files(
                &[veil_service::veil_ops::AddFileSpec::new(&source, "file")
                    .with_base_version(base)],
                "pw"
            )
            .is_err()
        );
    }
    let sql = veil_core::database::open(&m.veil_dir.join(CONTAINER_DB), Some("pw"), false).unwrap();
    assert!(sql.execute("INSERT INTO file_info(id,parent_id,name,kind,created_at,updated_at) VALUES('child',?1,'child','directory',0,0)",[&file.file_id]).is_err());
    assert!(
        sql.execute(
            "UPDATE file_info SET nonce=NULL WHERE id=?1",
            [&file.file_id]
        )
        .is_err()
    );
    assert!(
        sql.execute(
            "UPDATE file_info SET nonce=?1 WHERE id=?2",
            params![vec![0u8; 11], file.file_id]
        )
        .is_err()
    );
}
#[test]
fn cleanup_preserves_active_referenced_and_unknown_objects() {
    use veil_service::storage::locks::{LockManager, LockMode};
    let root = TempDir::new().unwrap();
    let m = VeilManager::new(root.path().join("veil"));
    m.init_veil("test", "pw").unwrap();
    let input = root.path().join("source");
    std::fs::write(&input, b"safe").unwrap();
    m.add_file(&input, "pw").unwrap();
    let current = m.list_files("pw").unwrap().remove(0).encrypted_name;
    let guard = LockManager::new(&m.veil_dir, "pw")
        .acquire(&["new-file".into()], LockMode::Write)
        .unwrap();
    let lock = guard.credentials().unwrap().remove(0).lock_id;
    let candidate = m.veil_dir.join(format!(
        "{lock}_{}.enc",
        veil_core::metadata::generate_version_id()
    ));
    std::fs::write(&candidate, b"uncommitted").unwrap();
    let unknown = m.veil_dir.join("unknown.enc");
    std::fs::write(&unknown, b"unknown").unwrap();
    m.cleanup("pw").unwrap();
    assert!(candidate.exists());
    assert!(unknown.exists());
    assert!(m.veil_dir.join(&current).exists());
    drop(guard);
    m.cleanup("pw").unwrap();
    assert!(!candidate.exists());
    assert!(unknown.exists());
    std::fs::remove_file(m.veil_dir.join(current)).unwrap();
    assert!(m.read_file_bytes("source", "pw", 1024).is_err());
}
#[test]
fn exported_database_omits_source_locks() {
    use veil_service::storage::locks::{LockManager, LockMode};
    let root = TempDir::new().unwrap();
    let m = VeilManager::new(root.path().join("veil"));
    m.init_veil("test", "pw").unwrap();
    let input = root.path().join("source");
    std::fs::write(&input, b"safe").unwrap();
    m.add_file(&input, "pw").unwrap();
    let file = m.list_files("pw").unwrap().remove(0);
    let guard = LockManager::new(&m.veil_dir, "pw")
        .acquire(&[file.file_id], LockMode::Read)
        .unwrap();
    let destination = root.path().join("snapshot");
    m.snapshot_to(&destination, "pw").unwrap();
    let source =
        veil_core::database::open(&m.veil_dir.join(CONTAINER_DB), Some("pw"), false).unwrap();
    let snapshot =
        veil_core::database::open(&destination.join(CONTAINER_DB), Some("pw"), false).unwrap();
    assert_eq!(
        source
            .query_row("SELECT count(*) FROM operation_locks", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        snapshot
            .query_row("SELECT count(*) FROM operation_locks", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(guard);
}
#[test]
fn wide_deep_tree_pagination_and_query_plans() {
    use rusqlite::params;
    let root = TempDir::new().unwrap();
    let m = VeilManager::new(root.path().join("veil"));
    m.init_veil("scale", "pw").unwrap();
    let root_id = ContainerDb::open(&m.veil_dir, "pw", None)
        .unwrap()
        .root()
        .unwrap()
        .id;
    let mut sql =
        veil_core::database::open(&m.veil_dir.join(CONTAINER_DB), Some("pw"), false).unwrap();
    let start = std::time::Instant::now();
    let tx = sql.transaction().unwrap();
    for index in 0..2000 {
        tx.execute("INSERT INTO file_info(id,parent_id,name,kind,created_at,updated_at) VALUES(?1,?2,?3,'directory',0,0)",params![format!("wide-{index}"),root_id,format!("wide-{index:04}")]).unwrap();
    }
    let mut parent = root_id.clone();
    for index in 0..256 {
        let identity = format!("deep-{index}");
        tx.execute("INSERT INTO file_info(id,parent_id,name,kind,created_at,updated_at) VALUES(?1,?2,?3,'directory',0,0)",params![identity,parent,format!("d-{index}")]).unwrap();
        parent = identity;
    }
    tx.commit().unwrap();
    let plan: String = sql
        .query_row(
            "EXPLAIN QUERY PLAN SELECT id FROM file_info WHERE parent_id=?1 AND name=?2",
            params![root_id, "wide-0010"],
            |r| r.get(3),
        )
        .unwrap();
    assert!(plan.contains("file_siblings"), "{plan}");
    drop(sql);
    let db = ContainerDb::open(&m.veil_dir, "pw", None).unwrap();
    assert_eq!(db.children(&root_id, 100, 50).unwrap().len(), 50);
    assert_eq!(db.subtree(&root_id).unwrap().len(), 2257);
    let app = AppStore::open(&root.path().join("app")).unwrap();
    drop(app);
    let sql = veil_core::database::open(&root.path().join("app/app.db"), None, false).unwrap();
    let mut stmt=sql.prepare("EXPLAIN QUERY PLAN SELECT value_json FROM local_record WHERE record_type='recent_div' ORDER BY updated_at DESC,record_key LIMIT 6").unwrap();
    let plans = stmt
        .query_map([], |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        plans.iter().any(|p| p.contains("local_record_recent")),
        "{plans:?}"
    );
    eprintln!(
        "规模: 宽目录2000，深度256，子树2257；环境 {} / {}；耗时 {:?}；计划: {plan}; {plans:?}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        start.elapsed()
    );
}

#[test]
fn identical_content_keeps_version_and_old_database_connections_cannot_write_after_rekey() {
    let root = TempDir::new().unwrap();
    let m = VeilManager::new(root.path().join("veil"));
    m.init_veil("test", "old").unwrap();
    let input = root.path().join("source");
    std::fs::write(&input, b"same").unwrap();
    m.add_file(&input, "old").unwrap();
    let before = m.list_files("old").unwrap().remove(0);
    m.add_file(&input, "old").unwrap();
    let after = m.list_files("old").unwrap().remove(0);
    assert_eq!(before.version_history, after.version_history);
    let old =
        veil_core::database::open(&m.veil_dir.join(CONTAINER_DB), Some("old"), false).unwrap();
    m.change_password("old", "new").unwrap();
    assert!(
        old.execute("UPDATE div_info SET name='stale writer'", [])
            .is_err()
    );
    assert_eq!(m.read_meta("new").unwrap().veil_name, "test");
}
#[cfg(unix)]
#[test]
fn directory_aliases_cannot_create_duplicate_spaces() {
    let root = TempDir::new().unwrap();
    let mut store = AppStore::open(&root.path().join("app")).unwrap();
    let space = store.create_space("original", None, false).unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(space.local_path.unwrap(), &alias).unwrap();
    assert!(store.create_space("alias", Some(&alias), false).is_err());
}

#[test]
fn competing_new_files_never_replace_or_delete_the_winner() {
    use std::sync::{Arc, Barrier};
    let root = TempDir::new().unwrap();
    let m = VeilManager::new(root.path().join("veil"));
    m.init_veil("test", "pw").unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let mut workers = Vec::new();
    for index in 0..2 {
        let source = root.path().join(format!("source-{index}"));
        std::fs::write(&source, format!("content-{index}")).unwrap();
        let barrier = barrier.clone();
        let directory = m.veil_dir.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            VeilManager::new(directory).add_files(
                &[AddFileSpec::new(source, "same.txt").without_overwrite()],
                "pw",
            )
        }));
    }
    let results = workers
        .into_iter()
        .map(|w| w.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        results.iter().filter(|r| r.is_ok()).count(),
        1,
        "{results:?}"
    );
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(veil_core::error::VeilError::FileAlreadyExists(_))))
            .count(),
        1,
        "{results:?}"
    );
    m.cleanup("pw").unwrap();
    let files = m.list_files("pw").unwrap();
    assert_eq!(files.len(), 1);
    assert!(m.veil_dir.join(&files[0].encrypted_name).exists());
    let content = m.read_file_bytes("same.txt", "pw", 1024).unwrap();
    assert!(content == b"content-0" || content == b"content-1");
}

#[test]
fn simultaneous_first_start_uses_one_persistent_install_identity() {
    use std::sync::{Arc, Barrier};
    let root = TempDir::new().unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let mut workers = Vec::new();
    for _ in 0..8 {
        let directory = root.path().to_path_buf();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            let mut store = AppStore::open(&directory).unwrap();
            (
                store.device_id().unwrap(),
                store.default_space().unwrap().id,
            )
        }));
    }
    let identities = workers
        .into_iter()
        .map(|w| w.join().unwrap())
        .collect::<Vec<_>>();
    assert!(identities.iter().all(|i| i == &identities[0]));
    assert_eq!(
        AppStore::open(root.path()).unwrap().spaces().unwrap().len(),
        1
    );
}

#[test]
fn schema_validation_accepts_cross_platform_newlines_and_rejects_changed_structure() {
    let root = TempDir::new().unwrap();
    let m = VeilManager::new(root.path().join("veil"));
    m.init_veil("test", "pw").unwrap();
    let sql = veil_core::database::open(&m.veil_dir.join(CONTAINER_DB), Some("pw"), false).unwrap();
    sql.execute_batch("PRAGMA writable_schema=ON; UPDATE sqlite_master SET sql=replace(replace(sql,char(13),''),char(10),char(13)||char(10)) WHERE sql IS NOT NULL; PRAGMA writable_schema=OFF;").unwrap();
    drop(sql);
    assert_eq!(m.read_meta("pw").unwrap().veil_name, "test");
    let sql = veil_core::database::open(&m.veil_dir.join(CONTAINER_DB), Some("pw"), false).unwrap();
    sql.execute_batch("CREATE TRIGGER sqlitex_evil AFTER INSERT ON file_info BEGIN SELECT 1; END;")
        .unwrap();
    drop(sql);
    assert!(m.read_meta("pw").is_err());
    let sql = veil_core::database::open(&m.veil_dir.join(CONTAINER_DB), Some("pw"), false).unwrap();
    sql.execute_batch("DROP TRIGGER sqlitex_evil;").unwrap();
    drop(sql);
    assert_eq!(m.read_meta("pw").unwrap().veil_name, "test");
    let sql = veil_core::database::open(&m.veil_dir.join(CONTAINER_DB), Some("pw"), false).unwrap();
    sql.execute_batch("DROP INDEX file_siblings;").unwrap();
    drop(sql);
    assert!(m.read_meta("pw").is_err());
}

#[test]
fn display_refresh_cannot_overwrite_a_pending_container_move() {
    use veil_service::config::AppConfig;
    let root = TempDir::new().unwrap();
    let app = root.path().join("app");
    let mut store = AppStore::open(&app).unwrap();
    let space = store.default_space().unwrap();
    let identity = veil_core::metadata::generate_veil_id();
    let directory = space.local_path.unwrap().join(&identity);
    let m = VeilManager::new(directory.clone());
    let metadata = m.init_veil_with_id(&identity, "test", "pw").unwrap();
    let mut config = AppConfig::load_from(&app.join("app.db")).unwrap();
    config
        .register_directory(&directory, &identity, "test", &metadata.created_at)
        .unwrap();
    let pending = serde_json::json!({"created_at":metadata.created_at,"movement":{"source":directory,"destination":root.path().join("target").join(&identity)}});
    store.set_record("div_state", &identity, &pending).unwrap();
    assert!(
        config
            .register_directory(&directory, &identity, "test", &metadata.created_at)
            .is_err()
    );
    assert_eq!(
        store
            .record::<serde_json::Value>("div_state", &identity)
            .unwrap()
            .unwrap(),
        pending
    );
}
