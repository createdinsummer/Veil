use tauri::State;
use veil_service::{ContainerView, ServiceError, ServiceRequest, ServiceResponse, VeilService};

fn container_response(
    result: Result<ServiceResponse, ServiceError>,
) -> Result<ContainerView, String> {
    match result.map_err(|error| error.to_string())? {
        ServiceResponse::Container(view) => Ok(view),
        _ => Err(ServiceError::UnexpectedResponse.to_string()),
    }
}

#[tauri::command]
async fn open_container(
    path: String,
    password: String,
    state: State<'_, VeilService>,
) -> Result<ContainerView, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        container_response(state.execute(ServiceRequest::OpenContainer { path, password }))
    })
    .await
    .map_err(|error| format!("操作未完成：{error}"))?
}

#[tauri::command]
async fn create_container(
    parent: String,
    name: String,
    password: String,
    state: State<'_, VeilService>,
) -> Result<ContainerView, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        container_response(state.execute(ServiceRequest::CreateContainer {
            parent,
            name,
            password,
        }))
    })
    .await
    .map_err(|error| format!("操作未完成：{error}"))?
}

#[tauri::command]
async fn refresh_container(state: State<'_, VeilService>) -> Result<ContainerView, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        container_response(state.execute(ServiceRequest::RefreshContainer))
    })
    .await
    .map_err(|error| format!("操作未完成：{error}"))?
}

#[tauri::command]
fn lock_container(state: State<'_, VeilService>) -> Result<(), String> {
    match state
        .execute(ServiceRequest::LockContainer)
        .map_err(|error| error.to_string())?
    {
        ServiceResponse::Locked => Ok(()),
        _ => Err(ServiceError::UnexpectedResponse.to_string()),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(VeilService::default())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            open_container,
            create_container,
            refresh_container,
            lock_container
        ])
        .run(tauri::generate_context!())
        .expect("无法启动 Veil 应用");
}
