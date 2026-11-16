//! 桌面客户端使用的空间、容器、文件及预览服务流程集成测试。
//! 所属模块：veil-service 桌面工作流；直接验证请求协议，不运行 SwiftUI 窗口。
//! 核心函数：desktop_flow_in_isolated_home 准备隔离进程；desktop_flow_worker 执行服务请求并核对业务结果。
//! 依赖能力：VeilService 请求/响应、临时 HOME、子进程和文件系统。
//! 业务目标：确认桌面端所依赖的业务链路在隔离本机配置下能够衔接完成。
use std::path::Path;
use veil_service::{ServiceError, ServiceRequest, ServiceResponse, VeilService};

#[test]
/// 验证桌面端服务在隔离 HOME 中完成空间、容器和文件的主要工作流。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn desktop_flow_in_isolated_home() {
    let root = tempfile::TempDir::new().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "desktop_flow_worker", "--nocapture"])
        .env("VEIL_APP_DATA_DIR", root.path().join("app"))
        .env("VEIL_DESKTOP_TEST_ROOT", root.path())
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
/// 在独立进程中运行桌面流程测试主体，隔离环境变量与应用状态。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
fn desktop_flow_worker() {
    let Some(root) = std::env::var_os("VEIL_DESKTOP_TEST_ROOT") else {
        return;
    };
    #[cfg(debug_assertions)]
    veil_core::kdf::enable_fast_test_kdf();
    let root = Path::new(&root);
    let service = VeilService::default();
    let selected_directory = root.join("自选空间目录");
    std::fs::create_dir(&selected_directory).unwrap();
    std::fs::write(selected_directory.join("已有文件.txt"), b"keep").unwrap();
    let spaces = match service
        .execute(ServiceRequest::CreateSpace {
            name: "工作文件".into(),
            path: Some(selected_directory.clone()),
        })
        .unwrap()
    {
        ServiceResponse::Spaces(spaces) => spaces,
        _ => panic!("预期返回空间列表"),
    };
    assert_eq!(
        Path::new(&spaces.iter().find(|s| s.name == "工作文件").unwrap().path),
        selected_directory.canonicalize().unwrap()
    );
    assert_eq!(
        std::fs::read(selected_directory.join("已有文件.txt")).unwrap(),
        b"keep"
    );
    assert!(
        service
            .execute(ServiceRequest::CreateSpace {
                name: "工作文件".into(),
                path: None,
            })
            .is_err()
    );
    for path in [
        selected_directory.clone(),
        root.join("自选空间目录/已有文件.txt"),
        "relative".into(),
    ] {
        assert!(
            service
                .execute(ServiceRequest::CreateSpace {
                    name: "无效目录".into(),
                    path: Some(path),
                })
                .is_err()
        );
    }
    let created = match service
        .execute(ServiceRequest::CreateInSpace {
            space: "工作文件".into(),
            name: "个人文件".into(),
            password: "password".into(),
        })
        .unwrap()
    {
        ServiceResponse::Container(view) => view,
        _ => panic!("预期返回容器"),
    };
    let container_path = created.path.clone();
    assert_eq!(
        Path::new(&container_path).parent().unwrap(),
        selected_directory.canonicalize().unwrap()
    );
    let source = root.join("note.txt");
    std::fs::write(&source, b"hello").unwrap();
    let request = || ServiceRequest::ImportSessionFiles {
        sources: vec![source.display().to_string()],
        directory: String::new(),
    };
    let view = match service.execute(request()).unwrap() {
        ServiceResponse::Container(view) => view,
        _ => panic!("预期返回容器"),
    };
    assert_eq!(view.files.len(), 1);
    assert_eq!(view.files[0].category, "document");
    assert_eq!(view.files[0].preview_mime, Some("text/plain"));
    assert_eq!(view.files[0].version, 1);
    std::fs::write(&source, b"changed").unwrap();
    assert!(matches!(
        service.execute(request()),
        Err(ServiceError::ImportFileFailed {
            source: veil_core::error::VeilError::FileAlreadyExists(_),
            ..
        })
    ));
    match service
        .execute(ServiceRequest::PreviewSessionFile {
            name: "note.txt".into(),
        })
        .unwrap()
    {
        ServiceResponse::Preview(bytes) | ServiceResponse::PreviewUnstable(bytes) => {
            assert_eq!(bytes, b"hello")
        }
        _ => panic!("预期返回文件内容"),
    }
    let output = root.join("output.txt");
    std::fs::write(&output, b"keep").unwrap();
    assert!(
        service
            .execute(ServiceRequest::ExportSessionFile {
                name: "note.txt".into(),
                destination: output.display().to_string()
            })
            .is_err()
    );
    assert_eq!(std::fs::read(&output).unwrap(), b"keep");
    std::fs::remove_file(&output).unwrap();
    service
        .execute(ServiceRequest::ExportSessionFile {
            name: "note.txt".into(),
            destination: output.display().to_string(),
        })
        .unwrap();
    assert_eq!(std::fs::read(output).unwrap(), b"hello");
    let listed = match service.execute(ServiceRequest::ListSpaces).unwrap() {
        ServiceResponse::Spaces(spaces) => spaces,
        _ => panic!("预期返回空间列表"),
    };
    let work = listed.iter().find(|s| s.name == "工作文件").unwrap();
    assert_eq!(work.containers.len(), 1);
    assert_eq!(work.containers[0].path, container_path);
    let default = listed.iter().find(|s| s.is_default).unwrap();
    let source_manager = veil_service::veil_ops::VeilManager::for_veil(
        container_path.clone().into(),
        created.id.clone(),
    );
    source_manager
        .create_directory("empty/nested", "password")
        .unwrap();
    let source_db = veil_service::storage::container::ContainerDb::open(
        Path::new(&container_path),
        "password",
        Some(&created.id),
    )
    .unwrap();
    let source_metadata = source_db.metadata().unwrap();
    // 移动必须与已有读取共存，且不能将该租约复制到新库。
    let source_read = veil_service::storage::locks::LockManager::new(&container_path, "password")
        .acquire_container(veil_service::storage::locks::LockMode::Read)
        .unwrap();
    service
        .execute(ServiceRequest::MoveContainer {
            id: &created.id,
            target_space: &default.id,
            password: "password",
        })
        .unwrap();
    assert!(Path::new(&container_path).join("veil.db").is_file());
    let listed = match service.execute(ServiceRequest::ListSpaces).unwrap() {
        ServiceResponse::Spaces(spaces) => spaces,
        _ => panic!("预期空间列表"),
    };
    let copied = &listed
        .iter()
        .find(|s| s.id == default.id)
        .unwrap()
        .containers[0];
    assert_ne!(copied.id, created.id);
    let copied_db = veil_service::storage::container::ContainerDb::open(
        Path::new(&copied.path),
        "password",
        Some(&copied.id),
    )
    .unwrap();
    assert_eq!(copied_db.metadata().unwrap().files.len(), 1);
    assert_eq!(
        serde_json::to_value(&copied_db.metadata().unwrap().directories).unwrap(),
        serde_json::to_value(&source_metadata.directories).unwrap()
    );
    assert_eq!(
        copied_db.keys().unwrap()[0].key_material,
        source_db.keys().unwrap()[0].key_material
    );
    assert_eq!(
        serde_json::to_value(&copied_db.metadata().unwrap().files).unwrap(),
        serde_json::to_value(&source_metadata.files).unwrap()
    );
    let target_write = veil_service::storage::locks::LockManager::new(&copied.path, "password")
        .acquire_container(veil_service::storage::locks::LockMode::Write)
        .unwrap();
    target_write.release().unwrap();
    source_read.release().unwrap();
    assert!(
        matches!(service.execute(ServiceRequest::RefreshContainer).unwrap(),ServiceResponse::Container(view) if view.path==container_path)
    );
    let recents = match service.execute(ServiceRequest::ListRecents).unwrap() {
        ServiceResponse::Recents(r) => r,
        _ => panic!("预期最近记录"),
    };
    assert!(recents.iter().any(|r| r.id == created.id));
    service
        .execute(ServiceRequest::RemoveSessionPath {
            name: "note.txt".into(),
        })
        .unwrap();
    assert_eq!(copied_db.metadata().unwrap().files.len(), 1);
    service.execute(ServiceRequest::LockContainer).unwrap();
    assert!(matches!(
        service.execute(ServiceRequest::PreviewSessionFile {
            name: "note.txt".into()
        }),
        Err(ServiceError::NotOpen)
    ));
}
