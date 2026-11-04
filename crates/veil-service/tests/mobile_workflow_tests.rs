//! 模拟两个设备，验证移动端使用的加密容器备份与还原流程。
use veil_service::{ServiceRequest, ServiceResponse, VeilService};

#[test]
fn encrypted_package_moves_between_devices() {
    let root = tempfile::TempDir::new().unwrap();
    for phase in ["export", "import"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "package_worker", "--nocapture"])
            .env("VEIL_APP_DATA_DIR", root.path().join(phase))
            .env("VEIL_MOBILE_TEST_ROOT", root.path())
            .env("VEIL_MOBILE_TEST_PHASE", phase)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{phase}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn package_worker() {
    let Some(root) = std::env::var_os("VEIL_MOBILE_TEST_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let package = root.join("随身文件.veil").to_string_lossy().into_owned();
    let service = VeilService::default();
    let spaces = match service.execute(ServiceRequest::ListSpaces).unwrap() {
        ServiceResponse::Spaces(spaces) => spaces,
        _ => panic!("预期空间列表"),
    };
    let space = spaces
        .iter()
        .find(|space| space.is_default)
        .unwrap()
        .id
        .clone();
    let content = "桌面与 iOS 使用同一密文格式。\n\0完整内容".as_bytes();
    if std::env::var("VEIL_MOBILE_TEST_PHASE").unwrap() == "export" {
        service
            .execute(ServiceRequest::CreateInSpace {
                space,
                name: "随身文件".into(),
                password: "test-password".into(),
            })
            .unwrap();
        let source = root.join("说明.txt");
        std::fs::write(&source, content).unwrap();
        service
            .execute(ServiceRequest::ImportSessionFiles {
                sources: vec![source.to_string_lossy().into_owned()],
                directory: String::new(),
            })
            .unwrap();
        service
            .execute(ServiceRequest::ExportSessionPackage {
                destination: package.clone(),
            })
            .unwrap();
        // 拒绝覆盖已有备份；锁定后不能继续导出。
        assert!(
            service
                .execute(ServiceRequest::ExportSessionPackage {
                    destination: package
                })
                .is_err()
        );
        service.execute(ServiceRequest::LockContainer).unwrap();
        assert!(
            service
                .execute(ServiceRequest::ExportSessionPackage {
                    destination: root.join("locked.veil").to_string_lossy().into_owned(),
                })
                .is_err()
        );
    } else {
        assert!(
            service
                .execute(ServiceRequest::ImportPackageInSpace {
                    path: package.clone(),
                    space: space.clone(),
                    password: "wrong-password".into(),
                })
                .is_err()
        );
        let restored = match service
            .execute(ServiceRequest::ImportPackageInSpace {
                path: package.clone(),
                space: space.clone(),
                password: "test-password".into(),
            })
            .unwrap()
        {
            ServiceResponse::Container(view) => view,
            _ => panic!("预期还原后的容器"),
        };
        assert_eq!(restored.name, "随身文件");
        assert_eq!(restored.files.len(), 1);
        assert!(std::path::Path::new(&restored.path).starts_with(&spaces[0].path));
        match service
            .execute(ServiceRequest::PreviewSessionFile {
                name: "说明.txt".into(),
            })
            .unwrap()
        {
            ServiceResponse::Preview(bytes) => assert_eq!(bytes, content),
            _ => panic!("预期文件内容"),
        }
        // 相同身份的容器不能重复导入，也不应破坏已打开的容器。
        assert!(
            service
                .execute(ServiceRequest::ImportPackageInSpace {
                    path: package,
                    space,
                    password: "test-password".into(),
                })
                .is_err()
        );
        service.execute(ServiceRequest::RefreshContainer).unwrap();
    }
}
