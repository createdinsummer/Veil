//! 使用真实打包文件验证密码强度工具的完整输入流程。

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use veil_core::veil_ops::VeilManager;
use veil_core::veil_package::VeilPacker;

fn create_package(temp: &TempDir) {
    veil_core::kdf::enable_fast_test_kdf();
    let manager = VeilManager::new(temp.path().join("work"));
    manager.init_veil("test", "default", "1").unwrap();
    let metadata = manager.read_meta("1").unwrap();
    let bytes = fs::read(manager.veil_dir.join(".veil-meta")).unwrap();
    VeilPacker::new(temp.path().join("test.veil"))
        .pack(&manager.veil_dir, &metadata, &bytes)
        .unwrap();
}

fn run_tool(temp: &TempDir, target: &Path, input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_veil-brute-force"))
        .arg(target)
        .env("HOME", temp.path())
        .env("VEIL_TEST_KDF", "fast")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input.as_bytes()).unwrap();
    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "密码测试工具超时：{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn dictionary_attack_reads_package_metadata_and_finds_password() {
    let temp = TempDir::new().unwrap();
    create_package(&temp);
    let package = temp.path().join("test.veil");
    let before = fs::read(&package).unwrap();
    let dictionary = temp.path().join("words.txt");
    fs::write(&dictionary, "wrong\n1\n").unwrap();
    let output = run_tool(
        &temp,
        &package,
        &format!("1\n{}\n0\n", dictionary.display()),
    );
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("已加载加密元数据"), "{text}");
    assert!(text.contains("密码已破解"), "{text}");
    assert!(text.contains("🔑 密码: 1"), "{text}");
    assert_eq!(fs::read(package).unwrap(), before);
}

#[test]
fn dictionary_miss_finishes_without_modifying_package() {
    let temp = TempDir::new().unwrap();
    create_package(&temp);
    let package = temp.path().join("test.veil");
    let before = fs::read(&package).unwrap();
    let dictionary = temp.path().join("words.txt");
    fs::write(&dictionary, "wrong\n2\n").unwrap();
    let output = run_tool(
        &temp,
        &package,
        &format!("1\n{}\n0\n", dictionary.display()),
    );
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("未能破解密码"), "{text}");
    assert_eq!(fs::read(package).unwrap(), before);
}

#[test]
fn raw_metadata_is_rejected_with_package_usage_hint() {
    let temp = TempDir::new().unwrap();
    create_package(&temp);
    let output = run_tool(&temp, &temp.path().join("work/.veil-meta"), "q\n");
    assert!(output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("仅支持 veil pack"), "{error}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("选择攻击模式"));
}
