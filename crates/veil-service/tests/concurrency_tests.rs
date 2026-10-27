//! 多进程文件写入与配置提交。

use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use veil_core::kdf;
use veil_core::version::VersionSnapshot;
use veil_service::veil_ops::{AddFileSpec, VeilManager};

#[test]
fn process_worker() {
    let Some(root) = std::env::var_os("VEIL_CONCURRENCY_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let index: usize = std::env::var("VEIL_CONCURRENCY_INDEX")
        .unwrap()
        .parse()
        .unwrap();
    kdf::enable_fast_test_kdf();
    let manager = VeilManager::new(root.join("veil"));
    fs::write(root.join(format!("ready-{index}")), b"ready").unwrap();
    let start = Instant::now();
    while !root.join("start").exists() {
        assert!(start.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(10));
    }
    // 读取触发机会式清理，与其他进程的密文写入竞争。
    manager.read_meta("password").unwrap();
    let target = if index < 4 {
        "same.txt".to_string()
    } else {
        format!("other-{index}.txt")
    };
    manager
        .add_files(
            &[AddFileSpec::new(
                root.join(format!("source-{index}")),
                target,
            )],
            "password",
        )
        .unwrap();
}

#[test]
fn checked_process_worker() {
    let Some(root) = std::env::var_os("VEIL_CHECKED_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let index: usize = std::env::var("VEIL_CHECKED_INDEX")
        .unwrap()
        .parse()
        .unwrap();
    kdf::enable_fast_test_kdf();
    let manager = VeilManager::new(root.join("veil"));
    let base: VersionSnapshot =
        serde_json::from_slice(&fs::read(root.join(format!("base-{index}"))).unwrap()).unwrap();
    fs::write(root.join(format!("ready-{index}")), b"ready").unwrap();
    let start = Instant::now();
    while !root.join("start").exists() {
        assert!(start.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(10));
    }
    let result = manager.add_files(
        &[
            AddFileSpec::new(root.join(format!("edit-{index}")), "file.txt")
                .with_base_version(base),
        ],
        "password",
    );
    let outcome = match result {
        Ok(_) => "ok",
        Err(veil_core::error::VeilError::VersionConflict(_)) => "conflict",
        Err(error) => panic!("意外错误: {error}"),
    };
    fs::write(root.join(format!("result-{index}")), outcome).unwrap();
}

#[test]
fn concurrent_checked_processes_only_commit_one_edit() {
    kdf::enable_fast_test_kdf();
    let root = TempDir::new().unwrap();
    let manager = VeilManager::new(root.path().join("veil"));
    manager.init_veil("concurrent", "password").unwrap();
    let original = root.path().join("original");
    fs::write(&original, b"original").unwrap();
    manager
        .add_files(&[AddFileSpec::new(&original, "file.txt")], "password")
        .unwrap();
    let mut children = Vec::new();
    for index in 0..2 {
        let edit = root.path().join(format!("edit-{index}"));
        let base = manager
            .extract_file_with_version("file.txt", &edit, "password")
            .unwrap();
        fs::write(
            root.path().join(format!("base-{index}")),
            serde_json::to_vec(&base).unwrap(),
        )
        .unwrap();
        fs::write(&edit, format!("edit-{index}")).unwrap();
        children.push(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "checked_process_worker", "--nocapture"])
                .env("VEIL_CHECKED_ROOT", root.path())
                .env("VEIL_CHECKED_INDEX", index.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
    }
    let start = Instant::now();
    while (0..2).any(|index| !root.path().join(format!("ready-{index}")).exists()) {
        assert!(start.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::write(root.path().join("start"), b"start").unwrap();
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let mut results: Vec<_> = (0..2)
        .map(|index| fs::read_to_string(root.path().join(format!("result-{index}"))).unwrap())
        .collect();
    results.sort();
    assert_eq!(results, ["conflict", "ok"]);
    assert_eq!(
        manager.list_files("password").unwrap()[0]
            .version_history
            .len(),
        2
    );
}

#[test]
fn concurrent_processes_preserve_same_file_versions_and_distinct_files() {
    kdf::enable_fast_test_kdf();
    let root = TempDir::new().unwrap();
    let manager = VeilManager::new(root.path().join("veil"));
    manager.init_veil("concurrent", "password").unwrap();
    let initial = root.path().join("initial");
    fs::write(&initial, b"initial").unwrap();
    manager
        .add_files(&[AddFileSpec::new(&initial, "same.txt")], "password")
        .unwrap();

    let mut children = Vec::new();
    for index in 0..6 {
        fs::write(
            root.path().join(format!("source-{index}")),
            format!("content-{index}"),
        )
        .unwrap();
        children.push(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "process_worker", "--nocapture"])
                .env("VEIL_CONCURRENCY_ROOT", root.path())
                .env("VEIL_CONCURRENCY_INDEX", index.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
    }
    let start = Instant::now();
    while (0..6).any(|index| !root.path().join(format!("ready-{index}")).exists()) {
        assert!(start.elapsed() < Duration::from_secs(20), "子进程未就绪");
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::write(root.path().join("start"), b"start").unwrap();
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let files = manager.list_files("password").unwrap();
    assert_eq!(files.len(), 3);
    let same = files
        .iter()
        .find(|entry| entry.original_name == "same.txt")
        .unwrap();
    assert_eq!(same.version_history.len(), 5);
    for name in ["same.txt", "other-4.txt", "other-5.txt"] {
        let output = root.path().join(format!("out-{name}"));
        manager.extract_file(name, &output, "password").unwrap();
        assert!(fs::read_to_string(output).unwrap().starts_with("content-"));
    }
}

#[test]
fn interrupted_writer_worker() {
    let Some(root) = std::env::var_os("VEIL_CRASH_TEST_ROOT") else {
        return;
    };
    kdf::enable_fast_test_kdf();
    let root = std::path::PathBuf::from(root);
    let manager = VeilManager::new(root.join("veil"));
    manager
        .add_files(
            &[AddFileSpec::new(root.join("large"), "same.txt")],
            "password",
        )
        .unwrap();
}

#[test]
fn killing_writer_keeps_last_committed_file_and_releases_lock() {
    kdf::enable_fast_test_kdf();
    let root = TempDir::new().unwrap();
    let manager = VeilManager::new(root.path().join("veil"));
    manager.init_veil("crash", "password").unwrap();
    let original = root.path().join("original");
    fs::write(&original, b"last committed").unwrap();
    manager
        .add_files(&[AddFileSpec::new(&original, "same.txt")], "password")
        .unwrap();
    fs::File::create(root.path().join("large"))
        .unwrap()
        .set_len(128 * 1024 * 1024)
        .unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "interrupted_writer_worker", "--nocapture"])
        .env("VEIL_CRASH_TEST_ROOT", root.path())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let temporary = loop {
        let found = fs::read_dir(root.path().join("veil"))
            .unwrap()
            .flatten()
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".veil-file-")
            });
        if let Some(entry) = found {
            break entry.path();
        }
        assert!(child.try_wait().unwrap().is_none(), "写入进程提前退出");
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "未观察到密文写入"
        );
        std::thread::sleep(Duration::from_millis(2));
    };
    manager.read_meta("password").unwrap();
    assert!(temporary.exists(), "清理不得删除正在写入的临时文件");
    child.kill().unwrap();
    child.wait().unwrap();
    let output = root.path().join("output");
    manager
        .extract_file("same.txt", &output, "password")
        .unwrap();
    assert_eq!(fs::read(output).unwrap(), b"last committed");
    assert_eq!(
        manager.list_files("password").unwrap()[0]
            .version_history
            .len(),
        1
    );
    assert!(!temporary.exists());
    manager
        .add_files(&[AddFileSpec::new(&original, "same.txt")], "password")
        .unwrap();
    assert_eq!(
        manager.list_files("password").unwrap()[0]
            .version_history
            .len(),
        2
    );
}
