//! macOS 客户端专用的 C ABI。仅转换调用参数，业务仍由 veil-service 执行。
use serde::Deserialize;
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    ptr, slice,
};
use veil_service::session::{ServiceRequest, ServiceResponse, VeilService};
use zeroize::{Zeroize, Zeroizing};

/// kind：0 为 JSON，1 为 UTF-8 错误，2 为文件二进制，3 为不稳定的文件二进制，4 为不稳定的导出结果。
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
        path: Option<std::path::PathBuf>,
    },
    DeleteSpace {
        id: String,
    },
    DeleteSpaceRecord {
        id: String,
    },
    DeleteContainer {
        id: String,
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
    StreamImportFiles {
        sources: Vec<String>,
        directory: String,
    },
    CreateDirectory {
        directory: String,
        name: String,
    },
    PollImportConflicts,
    ResolveImportConflicts {
        id: String,
        resolutions: Vec<veil_service::session::SessionImportResolution>,
    },
    PreviewFile {
        name: String,
    },
    ExportFile {
        name: String,
        destination: String,
    },
    StreamExportFiles {
        names: Vec<String>,
        destination: String,
    },
    PollExportConflicts,
    ResolveExportConflicts {
        id: String,
        resolutions: Vec<veil_service::session::SessionImportResolution>,
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
    /// 设置页展示用的系统配置（只读白名单）。
    GetSystemSetting {
        key: String,
    },
    /// 设置页切换默认空间；业务校验仍在 veil-service。
    SetDefaultSpace {
        id: String,
    },
    /// 免密时长；只保存秒数到 app.db，密码仍由应用层保存在内存中。
    SetUnlockCache {
        seconds: u64,
    },
    SetImportCommitInterval {
        seconds: u64,
    },
    SetImportCommitCount {
        count: u64,
    },
}

fn execute(service: &VeilService, request: Request) -> Result<VeilBuffer, String> {
    let request = match request {
        Request::ListSpaces => ServiceRequest::ListSpaces,
        Request::ListRecents => ServiceRequest::ListRecents,
        Request::CreateSpace { name, path } => ServiceRequest::CreateSpace { name, path },
        Request::DeleteSpace { id } => ServiceRequest::DeleteSpace { id },
        Request::DeleteSpaceRecord { id } => ServiceRequest::DeleteSpaceRecord { id },
        Request::DeleteContainer { id } => ServiceRequest::DeleteContainer { id },
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
        Request::StreamImportFiles { sources, directory } => {
            ServiceRequest::StreamSessionImport { sources, directory }
        }
        Request::CreateDirectory { directory, name } => {
            ServiceRequest::CreateSessionDirectory { directory, name }
        }
        Request::PollImportConflicts => ServiceRequest::PollSessionImportConflicts,
        Request::ResolveImportConflicts { id, resolutions } => {
            ServiceRequest::ResolveSessionImportConflicts { id, resolutions }
        }
        Request::PreviewFile { name } => ServiceRequest::PreviewSessionFile { name },
        Request::ExportFile { name, destination } => {
            ServiceRequest::ExportSessionFile { name, destination }
        }
        Request::StreamExportFiles { names, destination } => {
            ServiceRequest::StreamSessionExport { names, destination }
        }
        Request::PollExportConflicts => ServiceRequest::PollSessionExportConflicts,
        Request::ResolveExportConflicts { id, resolutions } => {
            ServiceRequest::ResolveSessionExportConflicts { id, resolutions }
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
        Request::GetSystemSetting { key } => {
            return serialize(service.execute(ServiceRequest::GetSystemSetting { key: &key }));
        }
        Request::SetDefaultSpace { id } => {
            return serialize(service.execute(ServiceRequest::SetDefaultSpace { id: &id }));
        }
        Request::SetUnlockCache { seconds } => {
            let value = serde_json::json!(seconds);
            return serialize(service.execute(ServiceRequest::SetUserSetting {
                key: "unlock_cache_seconds",
                value: &value,
            }));
        }
        Request::SetImportCommitInterval { seconds } => {
            let value = serde_json::json!(seconds);
            return serialize(service.execute(ServiceRequest::SetUserSetting {
                key: "import_commit_interval_seconds",
                value: &value,
            }));
        }
        Request::SetImportCommitCount { count } => {
            let value = serde_json::json!(count);
            return serialize(service.execute(ServiceRequest::SetUserSetting {
                key: "import_commit_count",
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
        ServiceResponse::ImportConflicts(value) => serde_json::to_vec(&value),
        ServiceResponse::Spaces(value) => serde_json::to_vec(&value),
        ServiceResponse::Recents(value) => serde_json::to_vec(&value),
        ServiceResponse::Setting(value) => serde_json::to_vec(&value),
        ServiceResponse::Preview(bytes) => return Ok(VeilBuffer::new(bytes, 2)),
        ServiceResponse::PreviewUnstable(bytes) => return Ok(VeilBuffer::new(bytes, 3)),
        ServiceResponse::ExportUnstable => return Ok(VeilBuffer::new(Vec::new(), 4)),
        ServiceResponse::Done | ServiceResponse::Locked => Ok(b"null".to_vec()),
        _ => {
            veil_core::logging::error("bridge", "serialize", "UNSUPPORTED_RESPONSE");
            return Err("服务返回了不支持的响应".into());
        }
    }
    .map_err(|_| {
        veil_core::logging::error("bridge", "serialize", "ENCODING_ERROR");
        "无法编码服务响应".to_owned()
    })?;
    Ok(VeilBuffer::new(data, 0))
}

/// 在创建客户端前配置移动端应用私有目录。
///
/// # Safety
/// path 必须指向有效的、以 NUL 结尾的 UTF-8 字符串。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn veil_configure_app_data_dir(path: *const std::ffi::c_char) -> bool {
    if path.is_null() {
        veil_core::logging::error("bridge", "configure_directory", "INVALID_ARGUMENTS");
        return false;
    }
    let Ok(path) = unsafe { std::ffi::CStr::from_ptr(path) }.to_str() else {
        veil_core::logging::error("bridge", "configure_directory", "INVALID_UTF8");
        return false;
    };
    match veil_service::storage::app::configure_app_data_dir(path.into()) {
        Ok(()) => true,
        Err(error) => {
            veil_core::logging::error_chain("bridge", "configure_directory", &error);
            false
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn veil_client_new() -> *mut VeilService {
    veil_core::logging::initialize();
    Box::into_raw(Box::new(VeilService::default()))
}

/// # Safety
/// source 和 message 必须为有效的 NUL 结尾 UTF-8 字符串；message 仅由 core 脱敏后持久化。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn veil_log_client_error(
    category: u32,
    source: *const std::ffi::c_char,
    line: u32,
    native_code: i64,
    message: *const std::ffi::c_char,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let source = if source.is_null() {
            "client"
        } else {
            unsafe { std::ffi::CStr::from_ptr(source) }
                .to_str()
                .unwrap_or("client")
        };
        let message = if message.is_null() {
            "客户端异常"
        } else {
            unsafe { std::ffi::CStr::from_ptr(message) }
                .to_str()
                .unwrap_or("客户端异常")
        };
        veil_core::logging::client_error(category, source, line, native_code, message);
    }));
}

/// 生命周期边界的尽力刷新；日志失败不能影响客户端退出或进入后台。
#[unsafe(no_mangle)]
pub extern "C" fn veil_logs_flush() {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _ = veil_core::logging::flush();
    }));
}

#[unsafe(no_mangle)]
pub extern "C" fn veil_logs_export() -> VeilBuffer {
    catch_unwind(AssertUnwindSafe(|| match veil_core::logging::export() {
        Ok(bytes) => VeilBuffer::new(bytes, 0),
        Err(error) => {
            veil_core::logging::error_chain("bridge", "export_logs", &error);
            VeilBuffer::error("无法读取错误日志")
        }
    }))
    .unwrap_or_else(|_| VeilBuffer::error("无法导出错误日志"))
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
/// client 在本调用期间必须有效；允许与正在执行的调用并发。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn veil_client_progress(client: *const VeilService) -> VeilBuffer {
    if client.is_null() {
        veil_core::logging::error("bridge", "progress", "INVALID_CLIENT");
        return VeilBuffer::error("客户端无效");
    }
    catch_unwind(AssertUnwindSafe(|| {
        serde_json::to_vec(&unsafe { &*client }.transfer_progress())
            .map(|bytes| VeilBuffer::new(bytes, 0))
            .unwrap_or_else(|_| {
                veil_core::logging::error("bridge", "progress", "ENCODING_ERROR");
                VeilBuffer::error("无法编码进度")
            })
    }))
    .unwrap_or_else(|_| VeilBuffer::error("无法读取进度"))
}

/// # Safety
/// client 在本调用期间必须有效；允许与正在执行的调用并发。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn veil_client_cancel_transfer(client: *const VeilService) -> bool {
    if client.is_null() {
        veil_core::logging::error("bridge", "cancel_transfer", "INVALID_CLIENT");
        return false;
    }
    catch_unwind(AssertUnwindSafe(|| unsafe { &*client }.cancel_transfer())).unwrap_or(false)
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
        veil_core::logging::error("bridge", "call", "INVALID_ARGUMENTS");
        return VeilBuffer::error("请求参数无效");
    }
    catch_unwind(AssertUnwindSafe(|| {
        // 请求副本用完即清零，不保留密码和原始请求正文。
        let input = Zeroizing::new(unsafe { slice::from_raw_parts(data, length) }.to_vec());
        let request = serde_json::from_slice::<Request>(&input).map_err(|error| {
            // 只记录解析类别与行列，不回显请求正文。
            veil_core::logging::error_with_code(
                "bridge",
                "decode_request",
                "INVALID_REQUEST",
                &error,
            );
            "请求格式无效".to_owned()
        })?;
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
        let malformed = br#"{"operation":"unknown","password":"secret-do-not-log"}"#;
        let buffer = unsafe { veil_client_call(client, malformed.as_ptr(), malformed.len()) };
        assert_eq!(buffer.kind, 1);
        unsafe {
            veil_buffer_free(buffer);
        }
        // 解码失败只记录解析位置，不回显请求正文。
        let log = String::from_utf8(veil_core::logging::export().unwrap()).unwrap();
        assert!(!log.contains("secret-do-not-log"));
        let records: Vec<veil_core::logging::Record> = serde_json::from_str(&log).unwrap();
        let decode = records
            .iter()
            .find(|record| record.operation == "decode_request")
            .unwrap();
        assert_eq!(decode.code, "INVALID_REQUEST");
        assert!(
            decode.message.contains("JSON 解析失败"),
            "{}",
            decode.message
        );
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
        // 设置页读取的系统配置：设备 ID 可读，未白名单的键必须拒绝。
        let (kind, bytes) = unsafe {
            call(
                client,
                serde_json::json!({"operation":"get_system_setting","key":"device_id"}),
            )
        };
        assert_eq!(kind, 0, "{}", String::from_utf8_lossy(&bytes));
        let device: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(veil_core::metadata::is_uuid_v4(device.as_str().unwrap()));
        assert_eq!(
            unsafe {
                call(
                    client,
                    serde_json::json!({"operation":"get_system_setting","key":"unknown_key"}),
                )
            }
            .0,
            1
        );
        // 设置页可把默认空间切换到现有空间。
        assert_eq!(
            unsafe {
                call(
                    client,
                    serde_json::json!({"operation":"set_default_space","id":space}),
                )
            }
            .0,
            0
        );
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
        let service = unsafe { &*client }.clone();
        let worker_source = source.to_string_lossy().into_owned();
        let importing = std::thread::spawn(move || {
            service.execute(ServiceRequest::StreamSessionImport {
                sources: vec![worker_source],
                directory: String::new(),
            })
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let inspection: serde_json::Value = loop {
            let (kind, bytes) = unsafe {
                call(
                    client,
                    serde_json::json!({"operation":"poll_import_conflicts"}),
                )
            };
            assert_eq!(kind, 0);
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            if !value.is_null() {
                break value;
            }
            if std::time::Instant::now() > deadline {
                unsafe {
                    veil_client_cancel_transfer(client);
                }
                panic!("未收到冲突批次");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(inspection["conflicts"][0]["sameContent"], true);
        let (kind, bytes) = unsafe {
            call(
                client,
                serde_json::json!({"operation":"resolve_import_conflicts","id":inspection["id"],"resolutions":[{"target":"说明.txt","action":"skip"}]}),
            )
        };
        assert_eq!(kind, 0, "{}", String::from_utf8_lossy(&bytes));
        assert!(importing.join().unwrap().is_ok());
        let (kind, bytes) = unsafe {
            call(
                client,
                serde_json::json!({"operation":"preview_file","name":"说明.txt"}),
            )
        };
        assert!(matches!(kind, 2 | 3));
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
        let exported_logs = veil_logs_export();
        assert_eq!(exported_logs.kind, 0);
        let log_bytes = unsafe { slice::from_raw_parts(exported_logs.data, exported_logs.length) };
        let records: serde_json::Value = serde_json::from_slice(log_bytes).unwrap();
        let text = String::from_utf8_lossy(log_bytes);
        assert!(!text.contains("test-password"));
        assert!(!text.contains("secret-do-not-log"));
        assert!(!text.contains("说明.txt"));
        assert!(!text.contains(Path::new(&root).to_str().unwrap()));
        let records = records.as_array().unwrap();
        assert!(records.iter().any(|r| r["code"] == "INVALID_REQUEST"));
        assert!(records.iter().any(|r| {
            r["operation"] == "ExportSessionFile"
                && r["code"] == "IO_ALREADY_EXISTS"
                && r["context"]["phase"].is_string()
        }));
        assert!(!records.iter().any(|r| r["code"] == "NOT_OPEN"));
        assert!(records.iter().any(|r| r["operation"] == "OpenContainer"));
        unsafe {
            veil_buffer_free(exported_logs);
        }
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
        let (kind, bytes) = unsafe {
            call(
                client,
                serde_json::json!({"operation":"delete_container","id":container["id"]}),
            )
        };
        assert_eq!(kind, 0, "{}", String::from_utf8_lossy(&bytes));
        let spaces: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(spaces.as_array().unwrap().iter().all(|space| {
            space["containers"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["id"] != container["id"])
        }));
        assert!(!Path::new(container["path"].as_str().unwrap()).exists());
        assert_eq!(
            unsafe { call(client, serde_json::json!({"operation":"refresh"})) }.0,
            1
        );
        let (kind, bytes) =
            unsafe { call(client, serde_json::json!({"operation":"list_recents"})) };
        assert_eq!(kind, 0);
        let recents: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            recents
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["id"] != container["id"])
        );
        unsafe {
            veil_client_free(client);
        }
    }
}
