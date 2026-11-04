//! macOS 客户端专用的 C ABI。仅转换调用参数，业务仍由 veil-service 执行。
use serde::Deserialize;
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    ptr, slice,
};
use veil_service::{ServiceRequest, ServiceResponse, VeilService};
use zeroize::{Zeroize, Zeroizing};

/// kind：0 为 JSON，1 为 UTF-8 错误，2 为文件二进制。
/// 所有返回缓冲区均由 veil_buffer_free 释放，调用方不可自行 free。
#[repr(C)]
pub struct VeilBuffer {
    pub data: *mut u8,
    pub length: usize,
    pub kind: u32,
}

impl VeilBuffer {
    fn new(bytes: Vec<u8>, kind: u32) -> Self {
        let mut bytes = bytes.into_boxed_slice();
        let result = Self {
            data: bytes.as_mut_ptr(),
            length: bytes.len(),
            kind,
        };
        std::mem::forget(bytes);
        result
    }
    fn error(message: impl Into<String>) -> Self {
        Self::new(message.into().into_bytes(), 1)
    }
}

/// 不派生 Debug，避免密码进入调试日志。
#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    ListSpaces,
    ListRecents,
    CreateSpace {
        name: String,
    },
    CreateContainer {
        space: String,
        name: String,
        password: String,
    },
    OpenContainer {
        path: String,
        password: String,
    },
    ImportPackage {
        path: String,
        space: String,
        password: String,
    },
    Refresh,
    Lock,
    ImportFiles {
        sources: Vec<String>,
        directory: String,
    },
    PreviewFile {
        name: String,
    },
    ExportFile {
        name: String,
        destination: String,
    },
    ExportPackage {
        destination: String,
    },
    RemovePath {
        name: String,
    },
    GetPreference {
        key: String,
    },
    SetPreference {
        key: String,
        value: serde_json::Value,
    },
}

fn execute(service: &VeilService, request: Request) -> Result<VeilBuffer, String> {
    let request = match request {
        Request::ListSpaces => ServiceRequest::ListSpaces,
        Request::ListRecents => ServiceRequest::ListRecents,
        Request::CreateSpace { name } => ServiceRequest::CreateSpace { name },
        Request::CreateContainer {
            space,
            name,
            password,
        } => ServiceRequest::CreateInSpace {
            space,
            name,
            password,
        },
        Request::OpenContainer { path, password } => {
            ServiceRequest::OpenContainer { path, password }
        }
        Request::ImportPackage {
            path,
            space,
            password,
        } => ServiceRequest::ImportPackageInSpace {
            path,
            space,
            password,
        },
        Request::Refresh => ServiceRequest::RefreshContainer,
        Request::Lock => ServiceRequest::LockContainer,
        Request::ImportFiles { sources, directory } => {
            ServiceRequest::ImportSessionFiles { sources, directory }
        }
        Request::PreviewFile { name } => ServiceRequest::PreviewSessionFile { name },
        Request::ExportFile { name, destination } => {
            ServiceRequest::ExportSessionFile { name, destination }
        }
        Request::ExportPackage { destination } => {
            ServiceRequest::ExportSessionPackage { destination }
        }
        Request::RemovePath { name } => ServiceRequest::RemoveSessionPath { name },
        Request::GetPreference { key } => {
            return serialize(service.execute(ServiceRequest::GetUserSetting { key: &key }));
        }
        Request::SetPreference { key, value } => {
            return serialize(service.execute(ServiceRequest::SetUserSetting {
                key: &key,
                value: &value,
            }));
        }
    };
    serialize(service.execute(request))
}

fn serialize(
    result: Result<ServiceResponse, veil_service::ServiceError>,
) -> Result<VeilBuffer, String> {
    let response = result.map_err(|error| error.to_string())?;
    let data = match response {
        ServiceResponse::Container(value) => serde_json::to_vec(&value),
        ServiceResponse::Spaces(value) => serde_json::to_vec(&value),
        ServiceResponse::Recents(value) => serde_json::to_vec(&value),
        ServiceResponse::Setting(value) => serde_json::to_vec(&value),
        ServiceResponse::Preview(bytes) => return Ok(VeilBuffer::new(bytes, 2)),
        ServiceResponse::Done | ServiceResponse::Locked => Ok(b"null".to_vec()),
        _ => return Err("服务返回了不支持的响应".into()),
    }
    .map_err(|_| "无法编码服务响应".to_owned())?;
    Ok(VeilBuffer::new(data, 0))
}

#[unsafe(no_mangle)]
pub extern "C" fn veil_client_new() -> *mut VeilService {
    Box::into_raw(Box::new(VeilService::default()))
}

/// # Safety
/// client 必须来自 veil_client_new；释放前须等待该客户端的全部调用结束，且只释放一次。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn veil_client_free(client: *mut VeilService) {
    if !client.is_null() {
        drop(unsafe { Box::from_raw(client) });
    }
}

/// # Safety
/// client 必须有效；data 必须指向至少 length 字节的可读内存，且在调用期间保持有效。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn veil_client_call(
    client: *mut VeilService,
    data: *const u8,
    length: usize,
) -> VeilBuffer {
    if client.is_null() || data.is_null() || length == 0 || length > 1024 * 1024 {
        return VeilBuffer::error("请求参数无效");
    }
    catch_unwind(AssertUnwindSafe(|| {
        // 请求副本用完即清零，不保留密码和原始请求正文。
        let input = Zeroizing::new(unsafe { slice::from_raw_parts(data, length) }.to_vec());
        let request =
            serde_json::from_slice::<Request>(&input).map_err(|_| "请求格式无效".to_owned())?;
        execute(unsafe { &*client }, request)
    }))
    .unwrap_or_else(|_| Err("本次操作未完成，请重新打开容器".into()))
    .unwrap_or_else(VeilBuffer::error)
}

/// # Safety
/// buffer 必须是 veil_client_call 返回的原始缓冲区，且只释放一次。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn veil_buffer_free(buffer: VeilBuffer) {
    if !buffer.data.is_null() {
        let mut bytes =
            unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(buffer.data, buffer.length)) };
        bytes.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    unsafe fn call(client: *mut VeilService, value: serde_json::Value) -> (u32, Vec<u8>) {
        let input = serde_json::to_vec(&value).unwrap();
        let buffer = unsafe { veil_client_call(client, input.as_ptr(), input.len()) };
        let output = unsafe { slice::from_raw_parts(buffer.data, buffer.length) }.to_vec();
        let kind = buffer.kind;
        unsafe {
            veil_buffer_free(buffer);
        }
        (kind, output)
    }

    #[test]
    fn rejects_malformed_calls_without_echoing_input() {
        let client = veil_client_new();
        let input = br#"{"operation":"unknown","password":"secret-do-not-log"}"#;
        let buffer = unsafe { veil_client_call(client, input.as_ptr(), input.len()) };
        assert_eq!(buffer.kind, 1);
        let error = unsafe { slice::from_raw_parts(buffer.data, buffer.length) };
        assert_eq!(error, "请求格式无效".as_bytes());
        unsafe {
            veil_buffer_free(buffer);
            veil_client_free(client);
        }
    }

    #[test]
    fn native_workflow() {
        let root = tempfile::TempDir::new().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::native_worker", "--nocapture"])
            .env("VEIL_APP_DATA_DIR", root.path().join("app"))
            .env("VEIL_NATIVE_TEST_ROOT", root.path())
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
    fn native_worker() {
        let Some(root) = std::env::var_os("VEIL_NATIVE_TEST_ROOT") else {
            return;
        };
        #[cfg(debug_assertions)]
        veil_core::kdf::enable_fast_test_kdf();
        let client = veil_client_new();
        let (kind, bytes) = unsafe { call(client, serde_json::json!({"operation":"list_spaces"})) };
        assert_eq!(kind, 0);
        let spaces: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let space = spaces
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["isDefault"] == true)
            .unwrap()["id"]
            .as_str()
            .unwrap();
        let (kind, bytes) = unsafe {
            call(
                client,
                serde_json::json!({"operation":"create_container","space":space,"name":"原生客户端测试","password":"test-password"}),
            )
        };
        assert_eq!(kind, 0, "{}", String::from_utf8_lossy(&bytes));
        let container: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            Path::new(container["path"].as_str().unwrap())
                .join("veil.db")
                .is_file()
        );
        let source = Path::new(&root).join("说明.txt");
        std::fs::write(&source, "SwiftUI 与 Rust 共用真实服务。\n").unwrap();
        let (kind, _) = unsafe {
            call(
                client,
                serde_json::json!({"operation":"import_files","sources":[source],"directory":""}),
            )
        };
        assert_eq!(kind, 0);
        let (kind, bytes) = unsafe {
            call(
                client,
                serde_json::json!({"operation":"preview_file","name":"说明.txt"}),
            )
        };
        assert_eq!(kind, 2);
        assert_eq!(bytes, std::fs::read(&source).unwrap());
        let exported = Path::new(&root).join("导出.txt");
        assert_eq!(unsafe { call(client, serde_json::json!({"operation":"export_file","name":"说明.txt","destination":exported})) }.0, 0);
        assert_eq!(std::fs::read(&exported).unwrap(), bytes);
        assert_eq!(unsafe { call(client, serde_json::json!({"operation":"export_file","name":"说明.txt","destination":exported})) }.0, 1);
        assert_eq!(
            unsafe { call(client, serde_json::json!({"operation":"lock"})) }.0,
            0
        );
        assert_eq!(
            unsafe {
                call(
                    client,
                    serde_json::json!({"operation":"preview_file","name":"说明.txt"}),
                )
            }
            .0,
            1
        );
        assert_eq!(unsafe { call(client, serde_json::json!({"operation":"open_container","path":container["path"],"password":"wrong"})) }.0, 1);
        assert_eq!(unsafe { call(client, serde_json::json!({"operation":"open_container","path":container["path"],"password":"test-password"})) }.0, 0);
        assert_eq!(
            unsafe {
                call(
                    client,
                    serde_json::json!({"operation":"remove_path","name":"说明.txt"}),
                )
            }
            .0,
            0
        );
        unsafe {
            veil_client_free(client);
        }
    }
}
