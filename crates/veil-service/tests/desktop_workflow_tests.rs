//! 桌面端服务入口的空间、文件及预览流程。
use std::path::Path;
use veil_service::{ServiceError, ServiceRequest, ServiceResponse, VeilService};

#[test]
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
        ServiceResponse::Preview(bytes) => assert_eq!(bytes, b"hello"),
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
    service
        .execute(ServiceRequest::MoveContainer {
            id: &created.id,
            target_space: &default.id,
            password: "password",
        })
        .unwrap();
    assert!(!Path::new(&container_path).exists());
    let moved = Path::new(&default.path).join(&created.id);
    assert!(moved.join("veil.db").is_file());
    assert!(
        matches!(service.execute(ServiceRequest::RefreshContainer).unwrap(),ServiceResponse::Container(view) if view.path==moved.to_string_lossy())
    );
    let recents = match service.execute(ServiceRequest::ListRecents).unwrap() {
        ServiceResponse::Recents(r) => r,
        _ => panic!("预期最近记录"),
    };
    assert_eq!(recents[0].id, created.id);
    service
        .execute(ServiceRequest::RemoveSessionPath {
            name: "note.txt".into(),
        })
        .unwrap();
    service.execute(ServiceRequest::LockContainer).unwrap();
    assert!(matches!(
        service.execute(ServiceRequest::PreviewSessionFile {
            name: "note.txt".into()
        }),
        Err(ServiceError::NotOpen)
    ));
}
