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

/// 验证 info 输出稳定容器 ID、实际容器大小、稳定排序和 MIME 分布。
#[test]
fn info_reports_identity_size_and_mime_distribution() {
    let env = TestEnv::new("test-password");
    let link = env.init("detailed-info");
    let text = env.write_file("z.txt", "text");
    let image = env.write_file("image.jpg", "jpeg");
    let nested = env.write_file("nested/file.md", "markdown");

    for (source, target) in [
        (&text, "z.txt"),
        (&image, "image.jpg"),
        (&nested, "nested/file.md"),
    ] {
        env.command()
            .args([
                "add",
                &env.path(&link),
                &env.path(source),
                target,
                "test-password",
            ])
            .assert()
            .success();
    }

    let assertion = env
        .command()
        .args(["info", &env.path(&link), "test-password"])
        .assert()
        .success()
        .stdout(predicate::str::contains("容器 ID: veil-"))
        .stdout(predicate::str::contains("容器大小:"))
        .stdout(predicate::str::contains("text/plain: 1"))
        .stdout(predicate::str::contains("image/jpeg: 1"))
        .stdout(predicate::str::contains("text/markdown: 1"));
    let stdout = String::from_utf8(assertion.get_output().stdout.clone()).unwrap();

    let image_index = stdout.find("1. image.jpg").unwrap();
    let nested_index = stdout.find("2. nested/file.md").unwrap();
    let text_index = stdout.find("3. z.txt").unwrap();
    assert!(image_index < nested_index && nested_index < text_index);
}

/// 验证 list 使用容器展示名称并按虚拟路径稳定排序。
#[test]
fn list_uses_container_name_and_stable_order() {
    let env = TestEnv::new("test-password");
    let link = env.init("ordered");
    let z = env.write_file("z.txt", "z");
    let a = env.write_file("a.txt", "a");
    let nested = env.write_file("nested/file.txt", "nested");

    for (source, target) in [(&z, "z.txt"), (&a, "a.txt"), (&nested, "nested/file.txt")] {
        env.command()
            .args([
                "add",
                &env.path(&link),
                &env.path(source),
                target,
                "test-password",
            ])
            .assert()
            .success();
    }

    let assertion = env
        .command()
        .args(["list", &env.path(&link), "test-password"])
        .assert()
        .success()
        .stdout(predicate::str::contains("容器 'ordered' 包含 3 个文件"))
        .stdout(predicate::str::contains("ordered.veil-link").not());
    let stdout = String::from_utf8(assertion.get_output().stdout.clone()).unwrap();

    let a_index = stdout.find("1. a.txt").unwrap();
    let nested_index = stdout.find("2. nested/file.txt").unwrap();
    let z_index = stdout.find("3. z.txt").unwrap();
    assert!(a_index < nested_index && nested_index < z_index);

    let empty_link = env.init("empty-list");
    env.command()
        .args(["list", &env.path(&empty_link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("容器 'empty-list' 包含 0 个文件"))
        .stdout(predicate::str::contains("(空)"));
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

/// 验证 rm 可以递归删除目录并规范化 `./` 路径。
#[test]
fn remove_recursively_deletes_directory() {
    let env = TestEnv::new("test-password");
    let link = env.init("remove-directory");
    env.write_file("tree/top.txt", "top");
    env.write_file("tree/sub/deep.txt", "deep");
    env.write_file("keep.txt", "keep");
    let tree = env.work.path().join("tree");

    env.command()
        .args([
            "add",
            &env.path(&link),
            &env.path(&tree),
            "-p",
            "test-password",
        ])
        .assert()
        .success();
    let keep = env.work.path().join("keep.txt");
    env.command()
        .args([
            "add",
            &env.path(&link),
            &env.path(&keep),
            "-p",
            "test-password",
        ])
        .assert()
        .success();

    env.command()
        .args(["rm", &env.path(&link), "./tree", "-p", "test-password"])
        .assert()
        .success()
        .stdout(predicate::str::contains("已删除目录: ./tree"));

    env.command()
        .args(["list", &env.path(&link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("tree").not())
        .stdout(predicate::str::contains("keep.txt"));

    env.command()
        .args(["exists", &env.path(&link), "tree"])
        .assert()
        .failure()
        .code(1);
}

/// 验证 free 按真实父子层级渲染标准树状连接线。
#[test]
fn free_renders_hierarchical_tree() {
    let env = TestEnv::new("test-password");
    let link = env.init("tree-view");
    let a = env.write_file("a.txt", "a");
    let keep = env.write_file("keep.txt", "kk");
    let b = env.write_file("b.txt", "bbb");
    let unicode = env.write_file("unicode.txt", "four");
    env.write_file("tree/top.txt", "top");
    env.write_file("tree/sub/deep.txt", "deep!");
    let tree = env.work.path().join("tree");

    env.command()
        .args([
            "add",
            &env.path(&link),
            &env.path(&a),
            "-p",
            "test-password",
        ])
        .assert()
        .success();
    env.command()
        .args([
            "add",
            &env.path(&link),
            &env.path(&keep),
            "-p",
            "test-password",
        ])
        .assert()
        .success();
    env.command()
        .args([
            "add",
            &env.path(&link),
            &env.path(&b),
            "nested/b.txt",
            "test-password",
        ])
        .assert()
        .success();
    env.command()
        .args([
            "add",
            &env.path(&link),
            &env.path(&unicode),
            "spaces/中文 文件.txt",
            "test-password",
        ])
        .assert()
        .success();
    env.command()
        .args([
            "add",
            &env.path(&link),
            &env.path(&tree),
            "-p",
            "test-password",
        ])
        .assert()
        .success();

    env.command()
        .args(["free", &env.path(&link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("├── a.txt (1 B)"))
        .stdout(predicate::str::contains("├── keep.txt (2 B)"))
        .stdout(predicate::str::contains("├── nested/"))
        .stdout(predicate::str::contains("│   └── b.txt (3 B)"))
        .stdout(predicate::str::contains("├── spaces/"))
        .stdout(predicate::str::contains("│   └── 中文 文件.txt (4 B)"))
        .stdout(predicate::str::contains("└── tree/"))
        .stdout(predicate::str::contains("    ├── sub/"))
        .stdout(predicate::str::contains("    │   └── deep.txt (5 B)"))
        .stdout(predicate::str::contains("    └── top.txt (3 B)"));
}

/// 验证 mv 支持文件移动、目录重命名、冲突拒绝和循环移动保护。
#[test]
fn move_files_and_directories() {
    let env = TestEnv::new("test-password");
    let link = env.init("move");
    let source = env.write_file("source.txt", "source");
    let keep = env.write_file("keep.txt", "keep");
    env.write_file("tree/top.txt", "top");
    let tree = env.work.path().join("tree");

    env.command()
        .args([
            "add",
            &env.path(&link),
            &env.path(&source),
            "-p",
            "test-password",
        ])
        .assert()
        .success();
    env.command()
        .args([
            "add",
            &env.path(&link),
            &env.path(&keep),
            "-p",
            "test-password",
        ])
        .assert()
        .success();
    env.command()
        .args([
            "add",
            &env.path(&link),
            &env.path(&tree),
            "-p",
            "test-password",
        ])
        .assert()
        .success();

    // 移动文件到已有隐式目录，最终路径应保留文件名。
    env.command()
        .args([
            "mv",
            &env.path(&link),
            "source.txt",
            "tree",
            "-p",
            "test-password",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("tree/source.txt"));
    env.command()
        .args(["exists", &env.path(&link), "tree/source.txt"])
        .assert()
        .success()
        .stdout(predicate::str::contains("存在: 文件"));

    // 目录整体移动时，子文件前缀必须同步更新。
    env.command()
        .args([
            "mv",
            &env.path(&link),
            "./tree",
            "archive",
            "-p",
            "test-password",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("archive"));
    env.command()
        .args(["exists", &env.path(&link), "archive/source.txt"])
        .assert()
        .success()
        .stdout(predicate::str::contains("存在: 文件"));
    env.command()
        .args(["exists", &env.path(&link), "tree"])
        .assert()
        .failure()
        .code(1);

    // 目标文件已存在时拒绝覆盖。
    env.command()
        .args([
            "mv",
            &env.path(&link),
            "keep.txt",
            "archive/source.txt",
            "-p",
            "test-password",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("[9015]"));

    // 目录不能移动到自身子目录。
    env.command()
        .args([
            "mv",
            &env.path(&link),
            "archive",
            "archive/sub",
            "-p",
            "test-password",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("不能把目录移动到自身或子目录"));
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

/// 验证 passwd 选项形式、错误旧密码和空新密码。
#[test]
fn passwd_options_and_failures_are_safe() {
    let env = TestEnv::new("old-password");
    let link = env.init("passwd-options");
    let source = env.write_file("secret.txt", "secret");

    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success();

    env.command()
        .args([
            "passwd",
            &env.path(&link),
            "-p",
            "wrong-password",
            "-n",
            "new-password",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("[9012]"));

    env.command()
        .args(["list", &env.path(&link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("secret.txt"));

    env.command()
        .args(["passwd", &env.path(&link), "-n", ""])
        .assert()
        .failure()
        .stderr(predicate::str::contains("[2002]"));

    env.command()
        .args([
            "passwd",
            &env.path(&link),
            "-p",
            "old-password",
            "-n",
            "new-password",
        ])
        .assert()
        .success();

    env.command_with_password("old-password")
        .args(["list", &env.path(&link)])
        .assert()
        .failure();

    env.command_with_password("new-password")
        .args(["list", &env.path(&link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("secret.txt"));
}

/// 验证新密码环境变量与旧密码环境变量相互独立。
#[test]
fn passwd_uses_separate_new_password_environment_variable() {
    let env = TestEnv::new("old-password");
    let link = env.init("passwd-env");

    env.command()
        .env("VEIL_NEW_PASSWORD", "new-password")
        .args(["passwd", &env.path(&link)])
        .assert()
        .success();

    env.command_with_password("old-password")
        .args(["list", &env.path(&link)])
        .assert()
        .failure();

    env.command_with_password("new-password")
        .args(["list", &env.path(&link)])
        .assert()
        .success();
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

/// 验证错误密码和损坏包不会留下工作区或链接。
#[test]
fn unpack_failures_roll_back_artifacts() {
    let env = TestEnv::new("test-password");
    let link = env.init("package-source");
    let source = env.write_file("payload.txt", "payload");

    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success();
    env.command()
        .args(["pack", &env.path(&link)])
        .assert()
        .success();

    let package = env.work.path().join("package-source.vault.veil");
    let before_count = default_container_count(&env);

    let wrong_link = env.work.path().join("wrong.veil-link");
    env.command_with_password("wrong-password")
        .args([
            "unpack",
            &env.path(&package),
            "-n",
            "wrong-password-unpack",
            "--link",
            &env.path(&wrong_link),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("[9012]"));
    assert!(!wrong_link.exists());
    assert_eq!(default_container_count(&env), before_count);

    let mut damaged = std::fs::read(&package).unwrap();
    damaged.pop();
    std::fs::write(&package, damaged).unwrap();

    let damaged_link = env.work.path().join("damaged.veil-link");
    env.command()
        .args([
            "unpack",
            &env.path(&package),
            "-n",
            "damaged-unpack",
            "--link",
            &env.path(&damaged_link),
        ])
        .assert()
        .failure();
    assert!(!damaged_link.exists());
    assert_eq!(default_container_count(&env), before_count);
}

/// 验证目标路径支持覆盖容器内名称。
#[test]
fn add_honors_destination_path() {
    let env = TestEnv::new("test-password");
    let link = env.init("destination");
    let source = env.write_file("source.txt", "destination content");
    let option_source = env.write_file("option-source.txt", "option destination");

    env.command()
        .args([
            "add",
            &env.path(&link),
            &env.path(&source),
            "nested/renamed.txt",
            "test-password",
        ])
        .assert()
        .success();

    env.command()
        .args(["list", &env.path(&link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("nested/renamed.txt"))
        .stdout(predicate::str::contains("source.txt").not());

    let output = env.work.path().join("renamed-output.txt");
    env.command()
        .args([
            "ex",
            &env.path(&link),
            "nested/renamed.txt",
            &env.path(&output),
        ])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(output).unwrap(),
        "destination content"
    );

    env.command()
        .args([
            "add",
            &env.path(&link),
            "-i",
            &env.path(&option_source),
            "-o",
            "option/renamed.txt",
            "-p",
            "test-password",
        ])
        .assert()
        .success();

    env.command()
        .args(["list", &env.path(&link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("option/renamed.txt"));
}

/// 验证目录递归添加并保持相对路径。
#[test]
fn add_recursively_imports_directories() {
    let env = TestEnv::new("test-password");
    let link = env.init("directory");
    env.write_file("tree/top.txt", "top");
    env.write_file("tree/sub/deep.txt", "deep");
    let source = env.work.path().join("tree");

    env.command()
        .args([
            "add",
            &env.path(&link),
            &env.path(&source),
            "-p",
            "test-password",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("已添加 2 个文件"));

    env.command()
        .args(["list", &env.path(&link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("tree/top.txt"))
        .stdout(predicate::str::contains("tree/sub/deep.txt"));

    let output = env.work.path().join("deep-output.txt");
    env.command()
        .args([
            "ex",
            &env.path(&link),
            "tree/sub/deep.txt",
            &env.path(&output),
        ])
        .assert()
        .success();
    assert_eq!(std::fs::read_to_string(output).unwrap(), "deep");
}

/// 验证重复添加同名文件会替换旧条目而不是累积重复项。
#[test]
fn add_replaces_existing_file() {
    let env = TestEnv::new("test-password");
    let link = env.init("replace");
    let source = env.write_file("repeat.txt", "first");

    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success();

    std::fs::write(&source, "second").unwrap();
    env.command()
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .success();

    let assertion = env
        .command()
        .args(["list", &env.path(&link)])
        .assert()
        .success();
    let stdout = String::from_utf8(assertion.get_output().stdout.clone()).unwrap();
    assert_eq!(stdout.matches("repeat.txt").count(), 1);

    let output = env.work.path().join("repeat-output.txt");
    env.command()
        .args(["ex", &env.path(&link), "repeat.txt", &env.path(&output)])
        .assert()
        .success();
    assert_eq!(std::fs::read_to_string(output).unwrap(), "second");
}

/// 验证错误密码不会留下新的密文文件。
#[test]
fn add_with_wrong_password_does_not_write_ciphertext() {
    let env = TestEnv::new("test-password");
    let link = env.init("wrong-password-add");
    let source = env.write_file("secret.txt", "secret");
    let before = encrypted_file_count(&env);

    env.command_with_password("wrong-password")
        .args(["add", &env.path(&link), &env.path(&source)])
        .assert()
        .failure()
        .stderr(predicate::str::contains("[9012]"));

    assert_eq!(encrypted_file_count(&env), before);

    env.command()
        .args(["list", &env.path(&link)])
        .assert()
        .success()
        .stdout(predicate::str::contains("(空)"));
}

/// 验证 exists 能区分文件、隐式目录和不存在路径，并返回脚本可用的退出码。
#[test]
fn exists_reports_file_directory_and_missing_path() {
    let env = TestEnv::new("test-password");
    let link = env.init("exists");
    let source = env.write_file("tree/nested/file.txt", "content");

    env.command()
        .args([
            "add",
            &env.path(&link),
            &env.path(&source),
            "tree/nested/file.txt",
            "test-password",
        ])
        .assert()
        .success();

    env.command()
        .args(["exists", &env.path(&link), "tree/nested/file.txt"])
        .assert()
        .success()
        .stdout(predicate::str::contains("存在: 文件: tree/nested/file.txt"));

    env.command()
        .args(["exists", &env.path(&link), "tree/nested"])
        .assert()
        .success()
        .stdout(predicate::str::contains("存在: 目录: tree/nested"));

    env.command()
        .args(["exists", &env.path(&link), "missing.txt"])
        .assert()
        .failure()
        .code(1)
        .stdout(predicate::str::contains("不存在: missing.txt"));

    env.command()
        .args(["exists", &env.path(&link)])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("[1010]"));
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

/// 统计默认工作区中单个容器的密文文件数量。
fn encrypted_file_count(env: &TestEnv) -> usize {
    let workspace_root = env.home.path().join(".veil/workspaces/default");
    std::fs::read_dir(workspace_root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .map(|path| {
            std::fs::read_dir(path)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .and_then(|extension| extension.to_str())
                        == Some("enc")
                })
                .count()
        })
        .sum()
}

/// 统计默认工作区中的容器目录数量。
fn default_container_count(env: &TestEnv) -> usize {
    let workspace_root = env.home.path().join(".veil/workspaces/default");
    let Ok(entries) = std::fs::read_dir(workspace_root) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .count()
}
