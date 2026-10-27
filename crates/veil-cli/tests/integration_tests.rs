//! 主 CLI 参数、帮助、链接和便携工作区集成测试。

mod common;

use assert_cmd::Command;
use common::TestEnv;
use predicates::prelude::*;

/// 命名工作区按配置目录及相对路径完成初始化、名称访问和解包。
#[test]
fn named_work_uses_config_relative_location() {
    let env = TestEnv::new("test-password");
    env.init("default-demo");
    let config_path = env.home.path().join(".veil/config.jsonc");
    let mut config = veil_service::config::GlobalConfig::load_from(&config_path).unwrap();
    let target = env.work.path().join("archive");
    config
        .register_custom_work("archive".to_string(), target.clone(), None)
        .unwrap();
    let work = &config.work.custom["archive"];
    assert_eq!(work.path, target);
    let work_root = config.resolve_work_root(work).unwrap();
    std::fs::write(&config_path, serde_json::to_string(&config).unwrap()).unwrap();

    env.command()
        .args(["init", "named-demo", "--work", "archive"])
        .assert()
        .success();
    let link = veil_service::link::VeilLink::load(&env.link_path("named-demo")).unwrap();
    assert!(
        work_root
            .join(&link.veil.veil_id)
            .join(".veil-info")
            .exists()
    );
    env.command()
        .args(["list", "named-demo"])
        .assert()
        .success();

    env.command()
        .args(["pack", "named-demo", "-o", "named.veil"])
        .assert()
        .success();
    // 清除原容器注册记录，使解包可以复用包内稳定 ID。
    let mut config = veil_service::config::GlobalConfig::load_from(&config_path).unwrap();
    config
        .veils
        .retain(|_, veil| veil.veil_id != link.veil.veil_id);
    config
        .links
        .retain(|record| record.link.veil.veil_id != link.veil.veil_id);
    std::fs::write(&config_path, serde_json::to_string(&config).unwrap()).unwrap();
    let output = env.work.path().join("unpacked.veil-link");
    env.command()
        .args([
            "unpack",
            "named.veil",
            "--work",
            "archive",
            "--link",
            &env.path(&output),
        ])
        .assert()
        .failure();
    assert!(
        work_root
            .join(&link.veil.veil_id)
            .join(".veil-info")
            .exists()
    );
    std::fs::remove_dir_all(work_root.join(&link.veil.veil_id)).unwrap();
    env.command()
        .args([
            "unpack",
            "named.veil",
            "--work",
            "archive",
            "--link",
            &env.path(&output),
        ])
        .assert()
        .success();
    let unpacked = veil_service::link::VeilLink::load(&output).unwrap();
    assert_eq!(unpacked.veil.veil_id, link.veil.veil_id);
    assert!(
        env.veil_dir(&output)
            .starts_with(std::fs::canonicalize(&work_root).unwrap())
    );
}

/// 验证顶层帮助包含全部工作区命令。
#[test]
fn help_lists_work_model_commands() {
    Command::cargo_bin("veil")
        .unwrap()
        .args(["--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("init"))
        .stdout(predicate::str::contains("exists"))
        .stdout(predicate::str::contains("pack"))
        .stdout(predicate::str::contains("unpack"))
        .stdout(predicate::str::contains("link"))
        .stdout(predicate::str::contains("config"))
        .stdout(predicate::str::contains("help"));
}

/// 验证版本输出与 work 发布版本一致。
#[test]
fn version_matches_work_release() {
    Command::cargo_bin("veil")
        .unwrap()
        .args(["--version"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2.0.0"));
}

/// 验证输出管道提前关闭时返回稳定 I/O 错误，而不是 panic。
#[cfg(unix)]
#[test]
fn broken_stdout_pipe_is_reported_without_panic() {
    use std::os::fd::FromRawFd;
    use std::process::Stdio;

    let mut pipe_fds = [-1; 2];
    // SAFETY: `pipe` 初始化两个 owned fd；随后立即关闭读端，只把写端交给子进程。
    assert_eq!(unsafe { libc::pipe(pipe_fds.as_mut_ptr()) }, 0);
    // SAFETY: `pipe_fds[0]` 由 `pipe` 返回且尚未关闭。
    unsafe { libc::close(pipe_fds[0]) };
    // SAFETY: 写端所有权转交给 `OwnedFd`，之后只使用该 owned 值。
    let writer = unsafe { std::os::fd::OwnedFd::from_raw_fd(pipe_fds[1]) };

    let child = std::process::Command::new(assert_cmd::cargo::cargo_bin("veil"))
        .env("VEIL_HINTS", "off")
        .arg("--help")
        .stdout(Stdio::from(writer))
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("[9001]"), "stderr: {stderr}");
    assert!(!stderr.contains("panicked"), "stderr: {stderr}");
}

/// 验证 `init` 帮助明确区分命名工作区和显式路径。
#[test]
fn init_help_describes_work_options() {
    Command::cargo_bin("veil")
        .unwrap()
        .args(["init", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("<已注册工作区>"))
        .stdout(predicate::str::contains("<工作区根路径（自动注册）>"))
        .stdout(predicate::str::contains("已注册的命名工作区"))
        .stdout(predicate::str::contains("名称，不是路径"))
        .stdout(predicate::str::contains("自动注册给当前容器"))
        .stdout(predicate::str::contains("--dedicated"))
        .stdout(predicate::str::contains("需配合 --work-root"));
}

/// 验证未知参数使用本地化提示，而不是 clap 的英文建议。
#[test]
fn init_rejects_unknown_option_with_localized_error() {
    let env = TestEnv::new("test-password");

    env.command()
        .args(["init", "test-bad-option", "--not-exist"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "❌ 错误 [1001]: 未知参数: --not-exist",
        ))
        .stderr(predicate::str::contains("tip:").not())
        .stderr(predicate::str::contains("unexpected argument").not());
}

/// 验证互斥密码参数使用本地化提示。
#[test]
fn init_rejects_password_conflict_with_localized_error() {
    let env = TestEnv::new("test-password");

    env.command()
        .args(["init", "photo-conflict", "Test-Pass-A", "-p", "Test-Pass-B"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("❌ 错误 [1002]: 参数冲突"))
        .stderr(predicate::str::contains("不能与"))
        .stderr(predicate::str::contains("同时使用"))
        .stderr(predicate::str::contains("cannot be used with").not());
}

/// 验证初始化可使用自定义链接路径。
#[test]
fn init_accepts_custom_link_path() {
    let env = TestEnv::new("test-password");
    let custom_link = env.work.path().join("project/photos.veil-link");
    std::fs::create_dir_all(custom_link.parent().unwrap()).unwrap();

    // 从主链接复制出第二个链接，再通过第二个链接写入共享工作区。
    env.command()
        .args([
            "init",
            "photos",
            "test-password",
            "--link",
            &env.path(&custom_link),
        ])
        .assert()
        .success();

    assert!(custom_link.exists());
    let source = env.write_file("photo.txt", "image");

    env.command()
        .args(["add", &env.path(&custom_link), &env.path(&source)])
        .assert()
        .success();
}

/// 验证多个链接可以指向同一工作区并共享内容。
#[test]
fn multiple_links_can_target_the_same_work() {
    let env = TestEnv::new("test-password");
    let primary_link = env.init("shared");
    let secondary_link = env.work.path().join("second.veil-link");

    env.command()
        .args([
            "link",
            &env.path(&primary_link),
            "-o",
            &env.path(&secondary_link),
        ])
        .assert()
        .success();

    let source = env.write_file("shared.txt", "content");
    env.command()
        .args(["add", &env.path(&secondary_link), &env.path(&source)])
        .assert()
        .success();

    env.command()
        .args(["free", &env.path(&primary_link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("shared.txt"));
}

/// 验证普通命令接受名称、ID 和显式链接，但拒绝直接工作区目录。
#[test]
fn veil_selector_accepts_name_id_and_link_only() {
    let env = TestEnv::new("test-password");
    let link = env.init("selector");
    let veil_id = veil_service::link::VeilLink::load(&link)
        .unwrap()
        .veil
        .veil_id;

    env.command().args(["list", "selector"]).assert().success();
    env.command().args(["list", &veil_id]).assert().success();
    env.command()
        .args(["list", &env.path(&link)])
        .assert()
        .success();

    let work_root = env.home.path().join(".veil/works/default");
    let work = std::fs::read_dir(work_root)
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| entry.path().join(".veil-info").is_file())
        .unwrap()
        .path();
    env.command()
        .args(["list", &env.path(&work)])
        .assert()
        .failure()
        .stderr(predicate::str::contains("工作区目录不能直接作为容器参数"));

    let rebuilt_link = env.work.path().join("rebuilt.veil-link");
    env.command()
        .args(["link", &env.path(&work), "-o", &env.path(&rebuilt_link)])
        .assert()
        .success();
    assert!(rebuilt_link.exists());
}

/// 配置缺失时没有已登记工作区，所有入口均报告未找到且不创建配置。
#[test]
fn missing_config_does_not_use_link_location_or_create_config() {
    let env = TestEnv::new("test-password");
    let link = env.init("configless");
    let veil_id = veil_service::link::VeilLink::load(&link)
        .unwrap()
        .veil
        .veil_id;

    std::fs::remove_file(env.home.path().join(".veil/config.jsonc")).unwrap();

    env.command()
        .args(["list", &env.path(&link)])
        .assert()
        .failure();
    env.command()
        .args(["list", "configless"])
        .assert()
        .failure();
    env.command().args(["list", &veil_id]).assert().failure();
    assert!(!env.home.path().join(".veil/config.jsonc").exists());
}

/// 验证主 CLI 支持创建专属工作区容器。
#[test]
fn dedicated_work_init_is_available_from_main_cli() {
    let env = TestEnv::new("test-password");
    let work = env.work.path().join("dedicated-volume");
    let link = env.work.path().join("vault.veil-link");

    env.command()
        .args([
            "init",
            "vault",
            "test-password",
            "--work-root",
            &env.path(&work),
            "--dedicated",
            "--link",
            &env.path(&link),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("创建容器成功"));

    // 专属模式仍使用 work_root/veil_id，工作区本身只负责容器归属。
    assert!(link.exists());
    let data = veil_service::link::VeilLink::load(&link).unwrap();
    assert!(work.join(&data.veil.veil_id).join(".veil-info").exists());

    let source = env.write_file("secret.txt", "secret");
    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success();
}

/// 验证同名容器由不同 `veil_id` 区分且内容互不干扰。
#[test]
fn duplicate_veil_names_are_distinguished_by_veil_id() {
    let env = TestEnv::new("test-password");
    let first_work = env.work.path().join("disk-a");
    let second_work = env.work.path().join("disk-b");
    let first_link = env.work.path().join("first.veil-link");
    let second_link = env.work.path().join("second.veil-link");

    env.command()
        .args([
            "init",
            "photos",
            "test-password",
            "--work-root",
            &env.path(&first_work),
            "--link",
            &env.path(&first_link),
        ])
        .assert()
        .success();
    env.command()
        .args([
            "init",
            "photos",
            "test-password",
            "--work-root",
            &env.path(&second_work),
            "--link",
            &env.path(&second_link),
        ])
        .assert()
        .success();

    // 两个容器展示名相同，只有链接中持久化的 veil_id 能区分它们。
    let first = veil_service::link::VeilLink::load(&first_link).unwrap();
    let second = veil_service::link::VeilLink::load(&second_link).unwrap();
    assert_ne!(first.veil.veil_id, second.veil.veil_id);

    // 同名时仅输入展示名无法确定目标，必须通过 ID 或链接区分。
    env.command()
        .args(["list", "photos"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("[9018]"));

    let source = env.write_file("note.txt", "content");
    env.command()
        .args(["add", &env.path(&first_link), &env.path(&source)])
        .assert()
        .success();

    env.command()
        .args(["free", &env.path(&first_link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("note.txt"));
    env.command()
        .args(["free", &env.path(&second_link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("(空)"));
}

/// 本机的便携选项仍使用固定的本机配置和默认工作区。
#[test]
fn portable_mode_uses_fixed_local_config() {
    let env = TestEnv::new("test-password");
    let portable_dir = env.work.path().join("portable");
    std::fs::create_dir_all(&portable_dir).unwrap();
    let link = portable_dir.join("photos.veil-link");

    env.command()
        .args([
            "init",
            "photos",
            "test-password",
            "--portable",
            "--link",
            &env.path(&link),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("工作区将创建在"));

    let work_root = env.home.path().join(".veil/works/default");
    assert!(!portable_dir.join(".veil").exists());
    let veil_dirs: Vec<_> = std::fs::read_dir(&work_root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("veil-"))
                && path.join(".veil-info").exists()
        })
        .collect();
    assert_eq!(veil_dirs.len(), 1);

    let link_content = std::fs::read_to_string(link).unwrap();
    let link: serde_json::Value = veil_core::jsonc::from_str(&link_content).unwrap();
    assert!(link.get("work").is_none());
    assert!(link.get("root_path").is_none());
    assert!(link["veil"].get("root_path").is_none());

    // 链接不可用时应能仅凭配置中的实际工作区路径按名称恢复。
    env.command().args(["list", "photos"]).assert().success();
}

/// 验证文件类型帮助在英文环境下可用。
#[test]
fn help_files_is_available_in_english() {
    Command::cargo_bin("veil")
        .unwrap()
        .env("VEIL_LANG", "en")
        .args(["help", "files"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Shortcut / bookmark"))
        .stdout(predicate::str::contains("ZIP archive"));
}

/// 验证互斥的工作区参数不会有一项被静默忽略。
#[test]
fn init_rejects_conflicting_work_options() {
    let env = TestEnv::new("test-password");

    env.command()
        .args([
            "init",
            "conflict",
            "--work",
            "default",
            "--work-root",
            &env.path(env.work.path().join("work")),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("不能同时使用"));

    env.command()
        .args([
            "init",
            "portable-conflict",
            "--portable",
            "--work-root",
            &env.path(env.work.path().join("work")),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--portable"));

    env.command()
        .args(["init", "dedicated-without-path", "--dedicated"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--work-root"));
}

/// 验证相对工作区路径会固定为绝对位置，离开原目录后链接仍可解析。
#[test]
fn init_resolves_relative_work_root_from_another_directory() {
    let env = TestEnv::new("test-password");
    let link = env.work.path().join("relative.veil-link");

    env.command()
        .args([
            "init",
            "relative",
            "--work-root",
            "relative-work",
            "--link",
            &env.path(&link),
        ])
        .assert()
        .success();

    assert_eq!(
        std::fs::canonicalize(env.veil_dir(&link).parent().unwrap()).unwrap(),
        std::fs::canonicalize(env.work.path().join("relative-work")).unwrap()
    );

    let mut list = env.command();
    list.current_dir(env.home.path())
        .args(["list", &env.path(&link)])
        .assert()
        .success();

    let mut by_name = env.command();
    by_name
        .current_dir(env.home.path())
        .args(["list", "relative"])
        .assert()
        .success();
}

/// 验证 `--link` 只能生成 CLI 可识别的 `.veil-link` 文件。
#[test]
fn init_rejects_link_path_with_unknown_extension() {
    let env = TestEnv::new("test-password");
    let output = env.work.path().join("not-a-link");

    env.command()
        .args(["init", "bad-link", "--link", &env.path(&output)])
        .assert()
        .failure()
        .stderr(predicate::str::contains(".veil-link"));

    assert!(!output.exists());
    assert!(!env.home.path().join(".veil/config.jsonc").exists());
}

/// 链接中的 ID 被修改后，不会按名称或链接位置选择其他容器。
#[test]
fn link_does_not_fall_back_from_missing_id_to_name() {
    let env = TestEnv::new("test-password");
    let link = env.init("missing-link");
    let mut rewritten = veil_service::link::VeilLink::load(&link).unwrap();
    rewritten.veil.veil_id = "veil-missing".into();
    rewritten.save(&link).unwrap();
    env.command()
        .args(["list", &env.path(&link)])
        .assert()
        .failure()
        .stderr(predicate::str::contains("当前未找到容器"));
}

/// 验证链接写入失败时会清理工作区，且不会留下孤立容器记录。
#[test]
fn init_rolls_back_when_link_registration_fails() {
    let env = TestEnv::new("test-password");
    let blocker = env.work.path().join("blocker");
    std::fs::write(&blocker, "not a directory").unwrap();
    let link = blocker.join("nested.veil-link");

    env.command()
        .args(["init", "rollback", "--link", &env.path(&link)])
        .assert()
        .failure()
        .stderr(predicate::str::contains("❌ 错误 [5010]"))
        .stderr(predicate::str::contains("链接注册失败"));

    assert!(!link.exists());
    assert!(!env.home.path().join(".veil/config.jsonc").exists());

    let work_root = env.home.path().join(".veil/works/default");
    if work_root.exists() {
        assert_eq!(std::fs::read_dir(&work_root).unwrap().count(), 0);
    }

    // 已有工作区发生同类失败时，原配置和原容器都必须保留。
    env.init("keep");
    let config_path = env.home.path().join(".veil/config.jsonc");
    let config = std::fs::read(&config_path).unwrap();
    let entry_count = std::fs::read_dir(&work_root).unwrap().count();
    env.command()
        .args(["init", "rollback-again", "--link", &env.path(&link)])
        .assert()
        .failure();
    assert_eq!(std::fs::read(config_path).unwrap(), config);
    assert_eq!(std::fs::read_dir(work_root).unwrap().count(), entry_count);
}

/// 验证专属工作区允许复用空目录，但拒绝覆盖已有内容。
#[test]
fn init_dedicated_work_requires_empty_directory() {
    let env = TestEnv::new("test-password");
    let empty_work = env.work.path().join("empty-work");
    std::fs::create_dir_all(&empty_work).unwrap();
    let empty_link = env.work.path().join("empty.veil-link");

    env.command()
        .args([
            "init",
            "empty",
            "--work-root",
            &env.path(&empty_work),
            "--dedicated",
            "--link",
            &env.path(&empty_link),
        ])
        .assert()
        .success();
    let data = veil_service::link::VeilLink::load(&empty_link).unwrap();
    assert!(!empty_work.join(".work-id").exists());
    assert!(
        empty_work
            .join(&data.veil.veil_id)
            .join(".veil-info")
            .exists()
    );

    let non_empty_work = env.work.path().join("non-empty-work");
    std::fs::create_dir_all(&non_empty_work).unwrap();
    let existing_file = non_empty_work.join("keep.txt");
    std::fs::write(&existing_file, "keep").unwrap();
    let non_empty_link = env.work.path().join("non-empty.veil-link");

    env.command()
        .args([
            "init",
            "non-empty",
            "--work-root",
            &env.path(&non_empty_work),
            "--dedicated",
            "--link",
            &env.path(&non_empty_link),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("必须为空目录"));

    assert_eq!(std::fs::read_to_string(existing_file).unwrap(), "keep");
    assert!(!non_empty_work.join(".veil-info").exists());
    assert!(!non_empty_link.exists());
}

/// 验证空密码不会创建无法提供任何口令强度的新容器。
#[test]
fn init_rejects_empty_password_without_side_effects() {
    let env = TestEnv::new("test-password");
    let link = env.link_path("empty-password");

    env.command_with_password("")
        .args(["init", "empty-password"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("❌ 错误 [2002]: 新密码不能为空"));

    assert!(!link.exists());
    assert!(!env.home.path().join(".veil/config.jsonc").exists());
    assert!(!env.home.path().join(".veil/works/default").exists());
}

/// 验证 link 支持自动创建父目录并限制输出扩展名。
#[test]
fn link_creates_parent_and_validates_extension() {
    let env = TestEnv::new("test-password");
    let primary = env.init("link-source");
    let output = env.work.path().join("nested/links/copy.veil-link");

    env.command()
        .args(["link", &env.path(&primary), "-o", &env.path(&output)])
        .assert()
        .success();
    assert!(output.exists());
    assert_eq!(
        std::fs::read(&primary).unwrap(),
        std::fs::read(output).unwrap()
    );

    let invalid = env.work.path().join("nested/links/invalid.txt");
    env.command()
        .args(["link", &env.path(&primary), "-o", &env.path(&invalid)])
        .assert()
        .failure()
        .stderr(predicate::str::contains("[6003]"));
    assert!(!invalid.exists());
}

/// 验证 help 可以显示指定命令的帮助。
#[test]
fn help_accepts_command_topics() {
    let env = TestEnv::new("test-password");

    env.command()
        .args(["help", "init"])
        .assert()
        .success()
        .stdout(predicate::str::contains("用法: veil init"))
        .stdout(predicate::str::contains("--work-root"));

    env.command()
        .args(["help", "not-a-command"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("[8001]"));
}

/// 验证 help all 和 --all 输出全部命令帮助。
#[test]
fn help_all_prints_every_command() {
    let env = TestEnv::new("test-password");

    env.command()
        .args(["help", "all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("===== init ====="))
        .stdout(predicate::str::contains("用法: veil init"))
        .stdout(predicate::str::contains("===== add ====="))
        .stdout(predicate::str::contains("用法: veil add"))
        .stdout(predicate::str::contains("===== exists ====="))
        .stdout(predicate::str::contains("===== help ====="));

    env.command()
        .args(["help", "--all"])
        .assert()
        .success()
        .stdout(predicate::str::contains("===== init ====="))
        .stdout(predicate::str::contains("===== unpack ====="));
}

/// 验证导出不存在的容器内文件返回文件未找到错误码。
#[test]
fn export_missing_file_returns_file_not_found_code() {
    let env = TestEnv::new("test-password");
    let link = env.init("missing-export");
    let output = env.work.path().join("missing-output.bin");

    env.command()
        .args(["ex", &env.path(&link), "missing.bin", &env.path(&output)])
        .assert()
        .failure()
        .stderr(predicate::str::contains("❌ 错误 [9014]"))
        .stderr(predicate::str::contains("文件未找到"));

    assert!(!output.exists());
}
