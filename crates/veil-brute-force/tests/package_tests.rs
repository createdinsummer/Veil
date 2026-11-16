//! 使用真实打包文件验证密码强度工具的完整输入流程。

//! 密码验证工具读取加密容器包的集成测试。
//! 所属模块：veil-brute-force 命令行；通过真实 Veil 包验证字典命中、未命中和不支持输入的提示。
//! 所有包都在测试临时目录创建，测试不会修改其他容器。
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use veil_service::package::VeilPacker;
use veil_service::veil_ops::VeilManager;

/// 为密码工具集成测试创建密码为“1”的真实容器包。
/// 入参：temp（&TempDir）为本用例独占的临时根目录，需允许创建 work 子目录和 test.veil。
/// 返回值：()；完成后 work 保存源容器，test.veil 保存打包结果。
/// 异常：初始化容器或打包失败时 unwrap 触发 panic，令测试失败。
/// 注意事项：启用测试 KDF 配置；使用固定测试密码，不用于生产数据。临时根目录由调用方维持生命周期。
fn create_package(temp: &TempDir) {
    veil_core::kdf::enable_fast_test_kdf();
    let manager = VeilManager::new(temp.path().join("work"));
    manager.init_veil("test", "1").unwrap();
    VeilPacker::new(temp.path().join("test.veil"))
        .pack_container(&manager, "1")
        .unwrap();
}

/// 驱动密码工具的交互输入，并收集完整进程结果供测试断言。
/// 入参：temp（&TempDir）作为子进程 HOME；target（&Path）为待验证包或故意传入的无效文件；input（&str）为含换行的菜单回答和候选字典路径。
/// 返回值：Output，包含子进程退出状态、标准输出和标准错误；非零退出本身不在此处断言。
/// 异常：启动、写入、等待失败会 panic；轮询超过 15 秒会终止子进程并携带标准错误 panic。
/// 注意事项：输入写完立即关闭 stdin；仅轮询阶段有期限，写输入和最终读取管道不受该期限保护。输出先由管道缓存，大量输出可能使子进程阻塞。
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
/// 验证字典攻击从容器包读取验证信息，并在候选密码命中时报告成功。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
/// 验证没有候选密码匹配时正常结束，原始包内容保持不变。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
/// 验证把元数据文件误当容器包时拒绝继续，并提示正确的包使用方式。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn raw_metadata_is_rejected_with_package_usage_hint() {
    let temp = TempDir::new().unwrap();
    create_package(&temp);
    let output = run_tool(&temp, &temp.path().join("work/veil.db"), "q\n");
    assert!(output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("仅支持 veil pack"), "{error}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("选择攻击模式"));
}
