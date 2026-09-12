//! 主 CLI 参数、帮助、链接和便携工作区集成测试。

mod common;

use assert_cmd::Command;
use common::TestEnv;
use predicates::prelude::*;

/// 验证顶层帮助包含全部工作区命令。
#[test]
fn help_lists_workspace_model_commands() {
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

/// 验证版本输出与 workspace 发布版本一致。
#[test]
fn version_matches_workspace_release() {
    Command::cargo_bin("veil")
        .unwrap()
        .args(["--version"])
        .assert()
        .success()
        .stdout(predicate::str::contains("2.0.0"));
}

/// 验证 `init` 帮助明确区分命名工作区和显式路径。
#[test]
fn init_help_describes_workspace_options() {
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
        .stdout(predicate::str::contains("需配合 --workspace-path"));
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
fn multiple_links_can_target_the_same_workspace() {
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

/// 验证主 CLI 支持创建专属工作区容器。
#[test]
fn dedicated_workspace_init_is_available_from_main_cli() {
    let env = TestEnv::new("test-password");
    let workspace = env.work.path().join("dedicated-volume");
    let link = env.work.path().join("vault.veil-link");

    env.command()
        .args([
            "init",
            "vault",
            "test-password",
            "--workspace-path",
            &env.path(&workspace),
            "--dedicated",
            "--link",
            &env.path(&link),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("创建容器成功"));

    // 专属模式下列表项与 .veil-meta 都应直接位于指定工作区路径。
    assert!(link.exists());
    assert!(workspace.join(".veil-meta").exists());

    let source = env.write_file("secret.txt", "secret");
    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success();
}

/// 验证同名容器由不同 `veil_id` 区分且内容互不干扰。
#[test]
fn duplicate_container_names_are_distinguished_by_veil_id() {
    let env = TestEnv::new("test-password");
    let first_workspace = env.work.path().join("disk-a");
    let second_workspace = env.work.path().join("disk-b");
    let first_link = env.work.path().join("first.veil-link");
    let second_link = env.work.path().join("second.veil-link");

    env.command()
        .args([
            "init",
            "photos",
            "test-password",
            "--workspace-path",
            &env.path(&first_workspace),
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
            "--workspace-path",
            &env.path(&second_workspace),
            "--link",
            &env.path(&second_link),
        ])
        .assert()
        .success();

    // 两个容器展示名相同，只有链接中持久化的 veil_id 能区分它们。
    let first = std::fs::read_to_string(&first_link).unwrap();
    let second = std::fs::read_to_string(&second_link).unwrap();
    let first_id = first
        .lines()
        .find(|line| line.starts_with("veil_id = "))
        .unwrap();
    let second_id = second
        .lines()
        .find(|line| line.starts_with("veil_id = "))
        .unwrap();
    assert_ne!(first_id, second_id);

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

/// 验证便携模式把链接和工作区放在同一路径下。
#[test]
fn portable_mode_keeps_link_and_workspace_on_the_same_path() {
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

    // 便携模式的默认工作区根应位于链接所在目录内部。
    let workspace_root = portable_dir.join(".veil/workspaces/default");
    let container_dirs: Vec<_> = std::fs::read_dir(&workspace_root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("veil-"))
                && path.join(".veil-meta").exists()
        })
        .collect();
    assert_eq!(container_dirs.len(), 1);

    let link_content = std::fs::read_to_string(link).unwrap();
    assert!(link_content.contains(".veil/workspaces/default/veil-"));
    assert!(!link_content.contains("mount_path = "));

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
fn init_rejects_conflicting_workspace_options() {
    let env = TestEnv::new("test-password");

    env.command()
        .args([
            "init",
            "conflict",
            "--workspace",
            "default",
            "--workspace-path",
            &env.path(env.work.path().join("workspace")),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("不能同时使用"));

    env.command()
        .args([
            "init",
            "portable-conflict",
            "--portable",
            "--workspace-path",
            &env.path(env.work.path().join("workspace")),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--portable"));

    env.command()
        .args(["init", "dedicated-without-path", "--dedicated"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--workspace-path"));
}

/// 验证相对工作区路径会固定为绝对位置，离开原目录后链接仍可解析。
#[test]
fn init_resolves_relative_workspace_path_from_another_directory() {
    let env = TestEnv::new("test-password");
    let link = env.work.path().join("relative.veil-link");

    env.command()
        .args([
            "init",
            "relative",
            "--workspace-path",
            "relative-workspace",
            "--link",
            &env.path(&link),
        ])
        .assert()
        .success();

    let link_data = veil_core::link::VeilLink::load(&link).unwrap();
    assert_ne!(
        link_data.workspace.path,
        std::path::PathBuf::from("relative-workspace")
    );
    assert!(
        link_data
            .workspace
            .path
            .to_string_lossy()
            .contains("relative-workspace")
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
    assert!(!env.home.path().join(".veil/config.toml").exists());
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
    assert!(!env.home.path().join(".veil/config.toml").exists());

    let workspace_root = env.home.path().join(".veil/workspaces/default");
    if workspace_root.exists() {
        assert_eq!(std::fs::read_dir(workspace_root).unwrap().count(), 0);
    }
}

/// 验证专属工作区允许复用空目录，但拒绝覆盖已有内容。
#[test]
fn init_dedicated_workspace_requires_empty_directory() {
    let env = TestEnv::new("test-password");
    let empty_workspace = env.work.path().join("empty-workspace");
    std::fs::create_dir_all(&empty_workspace).unwrap();
    let empty_link = env.work.path().join("empty.veil-link");

    env.command()
        .args([
            "init",
            "empty",
            "--workspace-path",
            &env.path(&empty_workspace),
            "--dedicated",
            "--link",
            &env.path(&empty_link),
        ])
        .assert()
        .success();
    assert!(empty_workspace.join(".veil-meta").exists());

    let non_empty_workspace = env.work.path().join("non-empty-workspace");
    std::fs::create_dir_all(&non_empty_workspace).unwrap();
    let existing_file = non_empty_workspace.join("keep.txt");
    std::fs::write(&existing_file, "keep").unwrap();
    let non_empty_link = env.work.path().join("non-empty.veil-link");

    env.command()
        .args([
            "init",
            "non-empty",
            "--workspace-path",
            &env.path(&non_empty_workspace),
            "--dedicated",
            "--link",
            &env.path(&non_empty_link),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("必须为空目录"));

    assert_eq!(std::fs::read_to_string(existing_file).unwrap(), "keep");
    assert!(!non_empty_workspace.join(".veil-meta").exists());
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
    assert!(!env.home.path().join(".veil/config.toml").exists());
    assert!(!env.home.path().join(".veil/workspaces/default").exists());
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
        .stdout(predicate::str::contains("--workspace-path"));

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
