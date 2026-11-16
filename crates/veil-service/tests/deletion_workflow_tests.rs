//! 空间、容器、文件夹和文件删除的服务入口集成测试。
//! 所属模块：veil-service 删除工作流；通过 ServiceRequest 验证业务对象删除后的可见状态。
//! 核心函数：deletion_flow_in_isolated_home 创建隔离子进程；deletion_flow_worker 执行完整删除链路及断言。
//! 依赖能力：VeilService 请求/响应、临时 HOME、子进程和文件系统。
//! 业务目标：确认不同层级删除作用于正确对象，环境隔离由父子进程配置完成。
use std::path::Path;
use veil_service::{ServiceRequest, ServiceResponse, VeilService};

#[test]
/// 验证容器和空间删除流程仅影响隔离 HOME 中登记的数据。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn deletion_flow_in_isolated_home() {
    let root = tempfile::TempDir::new().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "deletion_flow_worker", "--nocapture"])
        .env("VEIL_APP_DATA_DIR", root.path().join("app"))
        .env("VEIL_DELETE_TEST_ROOT", root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
/// 在子进程中执行删除流程并报告结果，供父测试检查磁盘和数据库状态。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn deletion_flow_worker() {
    let Some(root) = std::env::var_os("VEIL_DELETE_TEST_ROOT") else {
        return;
    };
    #[cfg(debug_assertions)]
    veil_core::kdf::enable_fast_test_kdf();
    let root = Path::new(&root);
    let parent_path = root.join("parent-space");
    let child_path = parent_path.join("child-space");
    std::fs::create_dir_all(&child_path).unwrap();
    let service = VeilService::default();

    let spaces = match service
        .execute(ServiceRequest::CreateSpace {
            name: "父空间".into(),
            path: Some(parent_path.clone()),
        })
        .unwrap()
    {
        ServiceResponse::Spaces(spaces) => spaces,
        _ => panic!("预期返回空间列表"),
    };
    let parent = spaces.iter().find(|space| space.name == "父空间").unwrap();
    let parent_id = parent.id.clone();
    let spaces = match service
        .execute(ServiceRequest::CreateSpace {
            name: "子空间".into(),
            path: Some(child_path.clone()),
        })
        .unwrap()
    {
        ServiceResponse::Spaces(spaces) => spaces,
        _ => panic!("预期返回空间列表"),
    };
    let child = spaces.iter().find(|space| space.name == "子空间").unwrap();
    let child_id = child.id.clone();

    let removable = match service
        .execute(ServiceRequest::CreateInSpace {
            space: "父空间".into(),
            name: "待删除容器".into(),
            password: "password".into(),
        })
        .unwrap()
    {
        ServiceResponse::Container(container) => container,
        _ => panic!("预期返回容器"),
    };
    let removable_path = Path::new(&removable.path).to_path_buf();
    let reader = VeilService::default();
    reader
        .execute(ServiceRequest::OpenContainer {
            path: removable.path.clone(),
            password: "password".into(),
        })
        .unwrap();
    assert!(
        service
            .execute(ServiceRequest::DeleteContainer {
                id: removable.id.clone()
            })
            .is_err()
    );
    assert!(removable_path.is_dir());
    assert!(matches!(
        reader.execute(ServiceRequest::RefreshContainer).unwrap(),
        ServiceResponse::Container(_)
    ));
    reader.execute(ServiceRequest::LockContainer).unwrap();
    service
        .execute(ServiceRequest::DeleteContainer { id: removable.id })
        .unwrap();
    assert!(!removable_path.exists());

    let active = match service
        .execute(ServiceRequest::CreateInSpace {
            space: "子空间".into(),
            name: "保留到空间删除".into(),
            password: "password".into(),
        })
        .unwrap()
    {
        ServiceResponse::Container(container) => container,
        _ => panic!("预期返回容器"),
    };
    let single = root.join("single.txt");
    let tree = root.join("tree");
    std::fs::create_dir(&tree).unwrap();
    std::fs::write(&single, b"single").unwrap();
    std::fs::write(tree.join("leaf.txt"), b"leaf").unwrap();
    service
        .execute(ServiceRequest::ImportSessionFiles {
            sources: vec![single.display().to_string(), tree.display().to_string()],
            directory: String::new(),
        })
        .unwrap();
    let view = match service
        .execute(ServiceRequest::RemoveSessionPath {
            name: "single.txt".into(),
        })
        .unwrap()
    {
        ServiceResponse::Container(view) => view,
        _ => panic!("预期返回容器"),
    };
    assert_eq!(view.files.len(), 1);
    let view = match service
        .execute(ServiceRequest::RemoveSessionPath {
            name: "tree".into(),
        })
        .unwrap()
    {
        ServiceResponse::Container(view) => view,
        _ => panic!("预期返回容器"),
    };
    assert!(view.files.is_empty());

    // 同空间另一个容器仍打开时，也能删除未使用的容器。
    let disposable = match reader
        .execute(ServiceRequest::CreateInSpace {
            space: "子空间".into(),
            name: "可独立删除".into(),
            password: "password".into(),
        })
        .unwrap()
    {
        ServiceResponse::Container(view) => view,
        _ => panic!("预期容器"),
    };
    reader
        .execute(ServiceRequest::DeleteContainer { id: disposable.id })
        .unwrap();
    assert!(
        matches!(service.execute(ServiceRequest::RefreshContainer).unwrap(), ServiceResponse::Container(view) if view.path == active.path)
    );

    // 删除父空间时，当前打开的子空间会先释放共享锁；子空间记录也随目录一起清除。
    let spaces = match service
        .execute(ServiceRequest::DeleteSpace { id: parent_id })
        .unwrap()
    {
        ServiceResponse::Spaces(spaces) => spaces,
        _ => panic!("预期返回空间列表"),
    };
    assert!(!parent_path.exists());
    assert!(!spaces.iter().any(|space| space.id == child_id));
    assert!(!Path::new(&active.path).exists());

    let default = spaces.iter().find(|space| space.is_default).unwrap();
    let old_default_id = default.id.clone();
    let old_default_path = Path::new(&default.path).to_path_buf();
    let spaces = match service
        .execute(ServiceRequest::DeleteSpace {
            id: old_default_id.clone(),
        })
        .unwrap()
    {
        ServiceResponse::Spaces(spaces) => spaces,
        _ => panic!("预期返回空间列表"),
    };
    let recreated = spaces.iter().find(|space| space.is_default).unwrap();
    assert_ne!(recreated.id, old_default_id);
    assert!(!old_default_path.exists());
    assert!(Path::new(&recreated.path).is_dir());
}
