#![cfg(feature = "fault-injection")]
use std::{fs, path::Path, process::Command, time::Duration};
use tempfile::TempDir;
use veil_service::{
    storage::{
        container::ContainerDb,
        locks::{LockManager, LockMode, LockOptions},
    },
    veil_ops::{AddFileSpec, VeilManager},
};
fn manager(root: &Path) -> VeilManager {
    VeilManager::new(root.join("veil"))
}
#[test]
fn fault_worker() {
    let Some(root) = std::env::var_os("VEIL_SQL_FAULT_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    let m = manager(root);
    match std::env::var("VEIL_SQL_FAULT_OPERATION").unwrap().as_str() {
        "write" => {
            m.add_files(&[AddFileSpec::new(root.join("changed"), "a.txt")], "old")
                .unwrap();
        }
        "rotate" => m.change_password_full("old", "new").unwrap(),
        _ => unreachable!(),
    }
}
fn fail(root: &Path, operation: &str, stage: &str) {
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "fault_worker", "--nocapture"])
        .env("VEIL_SQL_FAULT_ROOT", root)
        .env("VEIL_SQL_FAULT_OPERATION", operation)
        .env("VEIL_TEST_FAILPOINT", stage)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(86), "{stage}");
}
fn fixture() -> TempDir {
    let root = TempDir::new().unwrap();
    let m = manager(root.path());
    m.init_veil("test", "old").unwrap();
    fs::write(root.path().join("original"), b"original").unwrap();
    m.add_files(
        &[AddFileSpec::new(root.path().join("original"), "a.txt")],
        "old",
    )
    .unwrap();
    fs::write(root.path().join("changed"), b"changed").unwrap();
    root
}
#[test]
fn interrupted_writes_preserve_the_confirmed_database_version() {
    for stage in [
        "before_cipher_write",
        "during_cipher_write",
        "after_cipher_write",
        "after_cipher_persist",
        "before_metadata_commit",
        "after_metadata_commit",
    ] {
        let root = fixture();
        fail(root.path(), "write", stage);
        let m = manager(root.path());
        let expected = if stage == "after_metadata_commit" {
            b"changed".as_slice()
        } else {
            b"original".as_slice()
        };
        assert_eq!(
            m.read_file_bytes("a.txt", "old", 1024).unwrap(),
            expected,
            "{stage}"
        );
        m.cleanup("old").unwrap();
        assert_eq!(m.read_file_bytes("a.txt", "old", 1024).unwrap(), expected);
    }
}
#[test]
fn rotation_recovers_before_and_after_password_switch() {
    for stage in [
        "after_rotation_keys",
        "after_rotation_file",
        "before_database_rekey",
        "after_database_rekey",
        "before_retiring_key_removal",
        "after_retiring_key_removal",
    ] {
        let root = fixture();
        let m = manager(root.path());
        let original = m.list_files("old").unwrap().remove(0);
        fail(root.path(), "rotate", stage);
        let password = if m.read_meta("old").is_ok() {
            "old"
        } else {
            "new"
        };
        assert!(
            ContainerDb::open(&m.veil_dir, password, None)
                .unwrap()
                .keys()
                .unwrap()
                .len()
                <= 2
        );
        m.change_password_full("old", "new").unwrap();
        assert!(m.read_meta("old").is_err());
        let after = m.list_files("new").unwrap().remove(0);
        assert_eq!(after.file_id, original.file_id);
        assert_eq!(after.version_history, original.version_history);
        assert_eq!(
            ContainerDb::open(&m.veil_dir, "new", None)
                .unwrap()
                .keys()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            m.read_file_bytes("a.txt", "new", 1024).unwrap(),
            b"original"
        );
    }
}
#[test]
fn clock_worker() {
    if std::env::var_os("VEIL_SQL_CLOCK_WORKER").is_none() {
        return;
    }
    let root = fixture();
    let manager = LockManager::new(root.path().join("veil"), "old").with_options(LockOptions {
        timeout: Some(Duration::from_millis(250)),
        cancellation: None,
    });
    let guard = manager
        .acquire(&["resource".into()], LockMode::Write)
        .unwrap();
    let mut old = guard.credentials().unwrap();
    veil_service::storage::set_test_clock_offset(3600_000);
    assert!(manager.renew(&mut old).is_err());
    let replacement = manager
        .acquire(&["resource".into()], LockMode::Write)
        .unwrap();
    assert!(manager.release(&old).is_err());
    replacement.release().unwrap();
    veil_service::storage::set_test_clock_offset(-3600_000);
    assert!(manager.renew(&mut old).is_err());
    veil_service::storage::set_test_clock_offset(0);
}
#[test]
fn expired_leases_and_wall_clock_changes_do_not_revive_credentials() {
    assert!(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "clock_worker", "--nocapture"])
            .env("VEIL_SQL_CLOCK_WORKER", "1")
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn move_worker() {
    let Some(root) = std::env::var_os("VEIL_SQL_MOVE_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    let ids: Vec<String> =
        serde_json::from_slice(&fs::read(root.join("move-ids")).unwrap()).unwrap();
    veil_service::VeilService::default()
        .execute(veil_service::ServiceRequest::MoveContainer {
            id: &ids[0],
            target_space: &ids[1],
            password: "pw",
        })
        .unwrap();
}
#[test]
fn moving_container_recovers_after_directory_publish() {
    use veil_service::{config::AppConfig, storage::app::AppStore};
    let root = TempDir::new().unwrap();
    let app = root.path().join("app");
    let mut store = AppStore::open(&app).unwrap();
    let source = store.create_space("source", None, false).unwrap();
    let target = store.create_space("target", None, false).unwrap();
    let identity = veil_core::metadata::generate_veil_id();
    let directory = source.local_path.unwrap().join(&identity);
    let m = VeilManager::new(directory.clone());
    let metadata = m.init_veil_with_id(&identity, "test", "pw").unwrap();
    AppConfig::load_from(&app.join("app.db"))
        .unwrap()
        .register_directory(&directory, &identity, "test", &metadata.created_at)
        .unwrap();
    fs::write(
        root.path().join("move-ids"),
        serde_json::to_vec(&[&identity, &target.id]).unwrap(),
    )
    .unwrap();
    let mut first = Command::new(std::env::current_exe().unwrap());
    first
        .args(["--exact", "move_worker", "--nocapture"])
        .env("VEIL_SQL_MOVE_ROOT", root.path())
        .env("VEIL_APP_DATA_DIR", &app)
        .env("VEIL_TEST_FAILPOINT", "after_container_move");
    assert_eq!(first.status().unwrap().code(), Some(86));
    assert!(!directory.exists());
    let destination = target.local_path.unwrap().join(&identity);
    assert!(destination.join("veil.db").exists());
    assert_eq!(store.registrations().unwrap()[0].space_id, source.id);
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "move_worker", "--nocapture"])
        .env("VEIL_SQL_MOVE_ROOT", root.path())
        .env("VEIL_APP_DATA_DIR", &app)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(store.registrations().unwrap()[0].space_id, target.id);
    assert_eq!(
        ContainerDb::open(&destination, "pw", Some(&identity))
            .unwrap()
            .info()
            .unwrap()
            .0,
        identity
    );
}

#[test]
fn uncertain_commit_is_confirmed_before_returning_success() {
    let root = fixture();
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "fault_worker", "--nocapture"])
        .env("VEIL_SQL_FAULT_ROOT", root.path())
        .env("VEIL_SQL_FAULT_OPERATION", "write")
        .env("VEIL_TEST_COMMIT_UNCERTAIN", "1")
        .status()
        .unwrap();
    assert!(status.success());
    let m = manager(root.path());
    assert_eq!(m.read_file_bytes("a.txt", "old", 1024).unwrap(), b"changed");
    assert_eq!(m.list_files("old").unwrap()[0].version_history.len(), 2);
}

#[test]
fn read_lease_worker() {
    let Some(root) = std::env::var_os("VEIL_READ_LEASE_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    assert!(
        manager(root)
            .extract_file("a.txt", &root.join("output"), "old")
            .is_err()
    );
}
#[test]
fn expired_read_lease_never_publishes_plaintext_or_overwrites_an_existing_file() {
    let root = fixture();
    fs::write(root.path().join("output"), b"keep").unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "read_lease_worker", "--nocapture"])
        .env("VEIL_READ_LEASE_ROOT", root.path())
        .env("VEIL_TEST_PAUSEPOINT", "before_plaintext_publish")
        .env("VEIL_FAULT_TEST_ROOT", root.path())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    while !root.path().join("paused").exists() {
        assert!(start.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(10));
    }
    let db =
        veil_core::database::open(&root.path().join("veil/veil.db"), Some("old"), false).unwrap();
    db.execute("UPDATE operation_locks SET expires_at=0", [])
        .unwrap();
    fs::write(root.path().join("resume"), b"resume").unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(fs::read(root.path().join("output")).unwrap(), b"keep");
}

#[test]
fn link_competition_worker() {
    let Some(root) = std::env::var_os("VEIL_LINK_COMPETITION_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    let name = std::env::var("VEIL_LINK_COMPETITION_NAME").unwrap();
    let output = root.join("shared.veil-link");
    let service = veil_service::VeilService::default();
    let plan = match service
        .execute(veil_service::ServiceRequest::PrepareCreate {
            options: veil_service::workflow::CreateOptions {
                target: &name,
                link_output: output.to_str(),
                work_name: None,
                work_root: None,
                dedicated: false,
                portable: false,
            },
        })
        .unwrap()
    {
        veil_service::ServiceResponse::CreatePrepared(p) => p,
        _ => unreachable!(),
    };
    let result = service.execute(veil_service::ServiceRequest::CommitCreate {
        plan,
        password: "pw",
    });
    if name == "alpha" {
        assert!(result.is_ok());
    } else {
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("容器保留"));
    }
}
#[test]
fn losing_creation_never_removes_the_winners_link_or_a_committed_container() {
    use veil_service::storage::app::AppStore;
    let root = TempDir::new().unwrap();
    let mut children = Vec::new();
    for name in ["alpha", "beta"] {
        let pause = root.path().join(name);
        fs::create_dir(&pause).unwrap();
        children.push(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "link_competition_worker", "--nocapture"])
                .env("VEIL_LINK_COMPETITION_ROOT", root.path())
                .env("VEIL_LINK_COMPETITION_NAME", name)
                .env("VEIL_APP_DATA_DIR", root.path().join("app"))
                .env("VEIL_TEST_PAUSEPOINT", "after_container_init")
                .env("VEIL_FAULT_TEST_ROOT", &pause)
                .spawn()
                .unwrap(),
        );
    }
    let start = std::time::Instant::now();
    while ["alpha", "beta"]
        .iter()
        .any(|n| !root.path().join(n).join("paused").exists())
    {
        assert!(start.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::write(root.path().join("alpha/resume"), b"resume").unwrap();
    assert!(children[0].wait().unwrap().success());
    fs::write(root.path().join("beta/resume"), b"resume").unwrap();
    assert!(children[1].wait().unwrap().success());
    let link = veil_service::link::VeilLink::load(&root.path().join("shared.veil-link")).unwrap();
    assert_eq!(link.veil.veil_name, "alpha");
    let store = AppStore::open(&root.path().join("app")).unwrap();
    let registrations = store.registrations().unwrap();
    assert_eq!(registrations.len(), 2);
    let spaces = store.spaces().unwrap();
    for r in registrations {
        let directory = spaces
            .iter()
            .find(|s| s.id == r.space_id)
            .unwrap()
            .local_path
            .as_ref()
            .unwrap()
            .join(&r.div_id);
        assert!(ContainerDb::open(&directory, "pw", Some(&r.div_id)).is_ok());
    }
}
