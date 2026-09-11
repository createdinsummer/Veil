//! 工作区 CLI 命令端到端测试。
//!
//! 覆盖初始化、重复名称、增删改查、导出、改密以及打包解包流程，断言命令输出和
//! 最终文件状态。

mod common;

use common::TestEnv;
use predicates::prelude::*;

/// 验证初始化会创建工作区和可解析的链接文件。
#[test]
fn init_creates_workspace_and_link() {
    let env = TestEnv::new("test-password");

    env.command()
        .args(["init", "photos", "test-password"])
        .assert()
        .success()
        .stdout(predicate::str::contains("创建容器成功"));

    let link = env.link_path("photos");
    assert!(link.exists());

    let content = std::fs::read_to_string(link).unwrap();
    assert!(content.contains("veil_id = \"veil-"));
    assert!(content.contains("container_name = \"photos\""));
    assert!(content.contains("volume_id = "));
    assert!(content.contains("volume_label = "));
}

/// 验证同名容器使用不同 ID、目录和链接，且内容互相隔离。
#[test]
fn duplicate_container_names_use_distinct_directories_and_links() {
    let env = TestEnv::new("test-password");
    let first_link = env.init("photos");

    env.command()
        .env("VEIL_HINTS", "full")
        .args(["init", "photos", "test-password"])
        .assert()
        .success()
        .stdout(predicate::str::contains("veil pack photos-"));

    // 收集同名前缀的链接，确认第二次 init 没有覆盖第一次生成的文件。
    let mut links: Vec<_> = std::fs::read_dir(env.work.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().and_then(|extension| extension.to_str()) == Some("veil-link")
                && path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("photos"))
        })
        .collect();
    links.sort();
    assert_eq!(links.len(), 2);
    assert!(links.contains(&first_link));
    assert_eq!(
        first_link.file_name().and_then(|name| name.to_str()),
        Some("photos.veil-link")
    );

    let second_link = links
        .iter()
        .find(|path| *path != &first_link)
        .unwrap()
        .clone();
    assert!(
        second_link
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("photos-"))
    );
    let first_workspace = std::fs::read_to_string(&first_link).unwrap();
    let second_workspace = std::fs::read_to_string(&second_link).unwrap();
    assert_ne!(first_workspace, second_workspace);
    assert!(first_workspace.contains("/workspaces/default/veil-"));
    assert!(second_workspace.contains("/workspaces/default/veil-"));

    // 只向第二个容器写入文件，第一个容器应继续为空。
    let source = env.write_file("duplicate-name.txt", "second container");
    env.command()
        .args(["add", &env.path(&second_link), &env.path(&source)])
        .assert()
        .success();

    env.command()
        .args(["free", &env.path(&first_link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("(空)"));
    env.command()
        .args(["free", &env.path(&second_link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("duplicate-name.txt"));
}

/// 验证添加、列表和信息命令都能接受链接路径。
#[test]
fn add_list_and_info_accept_link() {
    let env = TestEnv::new("test-password");
    let link = env.init("documents");
    let source = env.write_file("notes.txt", "hello veil");

    // 添加后分别检查 free/list/info，确保三种查询入口都使用同一元数据源。
    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success()
        .stdout(predicate::str::contains("文件已添加"));

    env.command()
        .args(["free", &env.path(&link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("notes.txt"))
        .stdout(predicate::str::contains("文件数量: 1"));

    env.command()
        .args(["list", &env.path(&link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("notes.txt"));

    env.command()
        .args(["info", &env.path(&link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("容器名称: documents"))
        .stdout(predicate::str::contains("notes.txt"));
}

/// 验证重命名和删除会更新工作区元数据。
#[test]
fn rename_and_remove_use_workspace_metadata() {
    let env = TestEnv::new("test-password");
    let link = env.init("archive");
    let source = env.write_file("old.txt", "content");

    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success();

    env.command()
        .args(["mv", &env.path(&link), "old.txt", "new.txt"])
        .assert()
        .success()
        .stdout(predicate::str::contains("new.txt"));

    env.command()
        .args(["rm", &env.path(&link), "new.txt"])
        .assert()
        .success()
        .stdout(predicate::str::contains("已删除"));

    env.command()
        .args(["free", &env.path(&link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("(空)"));
}

/// 验证导出文件与原始内容逐字节一致。
#[test]
fn export_round_trip() {
    let env = TestEnv::new("test-password");
    let link = env.init("exports");
    let source = env.write_file("source.txt", "original content");
    let output = env.work.path().join("exported.txt");

    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success();

    env.command()
        .args(["ex", &env.path(&link), "source.txt", &env.path(&output)])
        .assert()
        .success()
        .stdout(predicate::str::contains("文件已导出"));

    assert_eq!(std::fs::read_to_string(output).unwrap(), "original content");
}

/// 验证错误密码会使容器访问失败。
#[test]
fn wrong_password_is_rejected() {
    let env = TestEnv::new("correct-password");
    let link = env.init("secure");

    env.command_with_password("wrong-password")
        .args(["free", &env.path(&link)])
        .assert()
        .failure()
        .stderr(predicate::str::contains("解密失败").or(predicate::str::contains("密码错误")));
}

/// 验证修改密码后旧密码失效、新密码可以访问原文件。
#[test]
fn change_password_rekeys_workspace() {
    let env = TestEnv::new("old-password");
    let link = env.init("rotate");
    let source = env.write_file("secret.txt", "secret");

    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success();

    env.command()
        .args(["passwd", &env.path(&link), "old-password", "new-password"])
        .assert()
        .success()
        .stdout(predicate::str::contains("密码修改成功"));

    env.command_with_password("old-password")
        .args(["free", &env.path(&link)])
        .assert()
        .failure();

    env.command_with_password("new-password")
        .args(["free", &env.path(&link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("secret.txt"));
}

/// 验证打包再解包后可通过新链接读取原文件。
#[test]
fn pack_and_unpack_create_reusable_links() {
    let env = TestEnv::new("test-password");
    let link = env.init("package");
    let source = env.write_file("payload.txt", "payload");

    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success();

    env.command()
        .args(["pack", &env.path(&link)])
        .assert()
        .success();

    // pack 的默认输出名由容器名派生，解包时再显式指定一个新名称。
    let package = env.work.path().join("package.vault.veil");
    assert!(package.exists());

    env.command()
        .args([
            "unpack",
            &env.path(&package),
            "-n",
            "restored",
            "-p",
            "test-password",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("已解包容器"));

    let restored_link = env.link_path("restored");
    assert!(restored_link.exists());

    env.command()
        .args(["free", &env.path(&restored_link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("payload.txt"));
}

/// 验证不存在的容器会以失败状态退出并给出明确提示。
#[test]
fn nonexistent_container_fails_cleanly() {
    let env = TestEnv::new("test-password");

    env.command()
        .args(["free", "missing.veil-link"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("找不到"));
}
