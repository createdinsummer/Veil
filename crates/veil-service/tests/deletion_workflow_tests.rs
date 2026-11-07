//! 空间、容器、文件夹和文件删除的端到端流程。
use std::path::Path;
use veil_service::{ServiceRequest, ServiceResponse, VeilService};

#[test]
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
    service
        .execute(ServiceRequest::DeleteContainer {
            id: removable.id,
            password: "password".into(),
        })
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
