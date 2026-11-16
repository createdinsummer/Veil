//! 模拟两个设备之间传递加密容器包的移动端服务流程测试。
//! 所属模块：veil-service 移动端备份/还原；不运行实际 iOS 界面或设备传输。
//! 核心函数：encrypted_package_moves_between_devices 组织隔离环境；package_worker 按指定设备阶段执行服务请求。
//! 依赖能力：VeilService 请求/响应、临时目录、子进程与加密包文件。
//! 业务目标：确认备份产物可在另一份本机配置中还原，并通过服务接口访问内容。
use veil_service::{ServiceRequest, ServiceResponse, VeilService};

#[test]
/// 功能：模拟两个设备顺序导出和导入同一份加密备份，验证跨设备恢复链路。
/// 入参：无；自行创建临时根目录，并为两个子进程分别指定独立应用目录。
/// 返回值：无；export 与 import 子进程都成功退出表示场景通过。
/// 异常：临时目录或进程创建失败、任一子进程失败均使测试 panic。
/// 注意事项：循环先导出后导入；两轮共享包文件，但应用本机登记状态相互隔离。
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
/// 在隔离子进程中执行移动端包导入 / 导出场景，避免测试进程状态互相影响。
/// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
/// 返回值：无；正常执行至末尾表示本场景通过。
/// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
/// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
        let progress = service.transfer_progress();
        assert_eq!(
            (progress.operation, progress.phase),
            ("export_package", "completed")
        );
        assert!(
            progress
                .total_bytes
                .is_some_and(|total| total > 0 && total == progress.processed_bytes)
        );
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
        let progress = service.transfer_progress();
        assert_eq!(
            (progress.operation, progress.phase),
            ("import_package", "completed")
        );
        assert_eq!(
            (progress.processed_bytes, progress.total_bytes),
            (content.len() as u64, Some(content.len() as u64))
        );
        assert_eq!(restored.name, "随身文件");
        assert_eq!(restored.files.len(), 1);
        assert!(std::path::Path::new(&restored.path).starts_with(&spaces[0].path));
        match service
            .execute(ServiceRequest::PreviewSessionFile {
                name: "说明.txt".into(),
            })
            .unwrap()
        {
            ServiceResponse::Preview(bytes) | ServiceResponse::PreviewUnstable(bytes) => {
                assert_eq!(bytes, content)
            }
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
