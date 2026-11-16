//! macOS 客户端专用的 C ABI。仅转换调用参数，业务仍由 veil-service 执行。
//!
//! 文件作用：本文件是 veil-macos-bridge 的全部实现——把 veil-service 的能力
//! 包装成 C 风格导出函数（C ABI=跨语言函数调用接口），供 macOS/iOS SwiftUI
//! 客户端（apps/veil-macos/Sources/VeilMac/ServiceClient.swift）经 FFI
//! （FFI=外部函数接口，一种语言调用另一种语言编译出的函数）调用。
//! 所属业务模块：Veil 的原生桥接层（桌面端与 Rust 服务的连接件）。
//! 核心清单：VeilBuffer（跨边界传递的缓冲区）、Request（按 operation 标签
//! 反序列化的请求枚举）、execute（请求翻译与执行）、serialize（响应转缓冲
//! 区），以及导出的 C 函数 veil_configure_app_data_dir / veil_client_new /
//! veil_client_call / veil_client_progress / veil_client_cancel_transfer /
//! veil_client_free / veil_buffer_free / veil_log_* 系列。
//! 依赖能力：veil-service 的 VeilService（业务执行）、serde/serde_json
//! （请求响应编解码）、zeroize（敏感数据清零）。
//! 约定（与 Swift 侧对应）：
//! - 内存管理“谁分配谁释放”：本库返回的每个 VeilBuffer 都由 Swift 调用
//!   veil_buffer_free 归还（Rust 侧 Box 重建后释放并清零），调用方不可自行 free；
//! - 错误码约定用 VeilBuffer.kind：0=JSON、1=UTF-8 错误文本、2=文件二进制、
//!   3=不稳定的文件二进制、4=不稳定的导出结果；
//! - 请求、进度和日志等入口用catch_unwind处理可展开panic；配置、创建及释放入口无同样包装。
//!   捕获不验证裸指针，不能恢复未定义行为或进程中止；各函数须遵守下方Safety约束。
//! - 客户端句柄（*mut VeilService）由 veil_client_new 分配、veil_client_free
//!   释放，期间允许并发调用。
//! 业务目标：让 SwiftUI 界面只面对“JSON 进、缓冲出”的稳定接口，业务规则、
//! 容器管理、导入导出冲突判定等全部留在 veil-service 中。
// 导入说明：serde 负责请求枚举与响应的序列化反序列化；panic 工具用于在
// FFI 边界捕获 Rust panic；指针/切片工具用于 C 指针与 Rust 切片的互转；
// VeilService 是业务入口；zeroize 保证密码等敏感字节释放前被清零。
use serde::Deserialize;
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    ptr, slice,
};
use veil_service::session::{ServiceRequest, ServiceResponse, VeilService};
use zeroize::{Zeroize, Zeroizing};

/// kind：0 为 JSON，1 为 UTF-8 错误，2 为文件二进制，3 为不稳定的文件二进制，4 为不稳定的导出结果。
/// 所有返回缓冲区均由 veil_buffer_free 释放，调用方不可自行 free。
/// 业务定位：Rust 与 Swift 之间唯一的数据交换容器——一段带类型的字节缓冲。
/// 数据说明：data 指向本库在 Rust 堆上分配的缓冲区；length 为其字节数；
/// kind 标识内容形态（见上方 0~4 的含义），Swift 按 kind 决定如何解析。
#[repr(C)]
pub struct VeilBuffer {
    /// 缓冲区首地址；由本库分配，Swift 只读，最终必须交回 veil_buffer_free。
    pub data: *mut u8,
    /// 缓冲区字节数。
    pub length: usize,
    /// 内容类型编码：0=JSON，1=错误文本，2/3=文件二进制，4=不稳定导出结果。
    pub kind: u32,
}

impl VeilBuffer {
    /// 把Rust字节载荷所有权交给跨语言调用方，等待显式归还。
    /// 入参：bytes（`Vec<u8>`）为JSON/错误/文件字节；kind（u32）为约定类型0至4，函数不校验。
    /// 返回值：VeilBuffer，data/length对应Box切片，空载荷长度为0仍须按原值归还。
    /// 异常：无Result；内存分配失败可能中止进程。
    /// 注意事项：转Box切片后forget，调用方不释放会泄漏；不得用C free或改长度，只能一次veil_buffer_free。
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
    /// 生成供Swift显示的UTF-8失败载荷。
    /// 入参：message（impl `Into<String>`）为错误文案。
    /// 返回值：kind=1的VeilBuffer。
    /// 异常：无Result，分配/转换自身失败不保证可恢复。
    /// 注意事项：不自动脱敏文案；调用方仍须归还缓冲区，错误载荷也适用相同所有权协议。
    fn error(message: impl Into<String>) -> Self {
        Self::new(message.into().into_bytes(), 1)
    }
}

/// 不派生 Debug，避免密码进入调试日志。
/// 业务定位：Swift 发来请求的完整描述——serde 按 `operation` 字段（snake_case）
/// 把 JSON 请求分派到对应变体，未知字段一律拒绝（deny_unknown_fields）。
/// 使用场景：仅由 veil_client_call 解析一次，随后在 execute 中翻译成
/// veil-service 的 ServiceRequest。
#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    /// 列出全部空间（保险箱）及其容器。
    ListSpaces,
    /// 列出最近使用过的容器。
    ListRecents,
    /// 新建空间：name 为空间名，path 为可选的显式存储位置。
    CreateSpace {
        /// 展示名称或新目录单级名称，由对应服务入口校验。
        name: String,
        /// 可选空间本地目录；None由服务创建默认位置。
        path: Option<std::path::PathBuf>,
    },
    /// 删除空间并递归删除其中的容器与文件。
    DeleteSpace {
        /// 要物理删除的空间稳定ID，精确匹配，不按名称解析。
        id: String,
    },
    /// 仅删除当前不可用空间的本机记录；此请求不把磁盘目录送废纸篓，也不物理删除目录。
    DeleteSpaceRecord {
        /// 要移除本机记录的不可用空间ID。
        id: String,
    },
    /// 删除容器及其中的文件。
    DeleteContainer {
        /// 要删除的容器稳定UUID，不需在此请求中提供密码。
        id: String,
    },
    /// 在指定空间新建容器：password 即容器的主密码。
    CreateContainer {
        /// 目标空间ID或名称，由服务选择并验证。
        space: String,
        /// 展示名称或新目录单级名称，由对应服务入口校验。
        name: String,
        /// 数据库或备份包密码，成功打开后会话可能继续持有受保护副本。
        password: String,
    },
    /// 打开现有容器目录并解锁；path不是.veil备份包文件，password为数据库密码。
    OpenContainer {
        /// 本地已存在的容器目录路径，包含veil.db与密文。
        path: String,
        /// 数据库或备份包密码，成功打开后会话可能继续持有受保护副本。
        password: String,
    },
    /// 导入 .veil 打包文件到指定空间：password 为包的密码。
    ImportPackage {
        /// 本地.veil备份包文件路径。
        path: String,
        /// 目标空间ID或名称，由服务选择并验证。
        space: String,
        /// 数据库或备份包密码，成功打开后会话可能继续持有受保护副本。
        password: String,
    },
    /// 重新加载当前容器的目录状态。
    Refresh,
    /// 清除主会话并释放其密码包装/生命周期锁；在途快照仍可能持有密码副本。
    Lock,
    /// 一次性导入一批文件：sources 为源路径列表，directory 为目标目录。
    ImportFiles {
        /// 本地来源路径列表，每项一个文件/目录；扫描处理每项并生成逐文件任务。
        sources: Vec<String>,
        /// 容器内部父目录逻辑路径，空串表示根目录，不是本地磁盘目录。
        directory: String,
    },
    /// 流式导入（长任务版）：边扫描边导入，进度经 veil_client_progress 查询。
    StreamImportFiles {
        /// 本地来源路径列表，每项一个文件/目录；扫描处理每项并生成逐文件任务。
        sources: Vec<String>,
        /// 容器内部父目录逻辑路径，空串表示根目录，不是本地磁盘目录。
        directory: String,
    },
    /// 在当前目录下新建子目录。
    CreateDirectory {
        /// 容器内部父目录逻辑路径，空串表示根目录，不是本地磁盘目录。
        directory: String,
        /// 展示名称或新目录单级名称，由对应服务入口校验。
        name: String,
    },
    /// 查询流式导入期间产生的同名冲突批次。
    PollImportConflicts,
    /// 提交对一批导入冲突的处置决定（覆盖/跳过）。
    ResolveImportConflicts {
        /// poll_import_conflicts返回的批次token，须仍为当前待决批次。
        id: String,
        /// 当前冲突批次的完整决定列表；每项含target与skip/overwrite，服务校验匹配和完整性。
        resolutions: Vec<veil_service::session::SessionImportResolution>,
    },
    /// 读取文件内容用于预览。
    PreviewFile {
        /// 容器内部文件/目录路径，用于预览、单文件导出或删除。
        name: String,
    },
    /// 一次性导出文件到目标路径。
    ExportFile {
        /// 容器内部文件/目录路径，用于预览、单文件导出或删除。
        name: String,
        /// 外部输出位置，单文件为精确文件路径，多项/包依对应业务规则解释。
        destination: String,
    },
    /// 流式导出（长任务版）：names 为要导出的文件列表。
    StreamExportFiles {
        /// 容器内部导出选择列表，每项一个文件或目录，可折叠重复后代选择。
        names: Vec<String>,
        /// 外部输出位置，单文件为精确文件路径，多项/包依对应业务规则解释。
        destination: String,
    },
    /// 查询流式导出期间产生的冲突批次。
    PollExportConflicts,
    /// 提交对一批导出冲突的处置决定。
    ResolveExportConflicts {
        /// poll_export_conflicts返回的批次token，须仍为当前待决批次。
        id: String,
        /// 当前冲突批次的完整决定列表；每项含target与skip/overwrite，服务校验匹配和完整性。
        resolutions: Vec<veil_service::session::SessionImportResolution>,
    },
    /// 把当前会话容器打包为.veil，非整个空间所有容器。
    ExportPackage {
        /// 外部输出位置，单文件为精确文件路径，多项/包依对应业务规则解释。
        destination: String,
    },
    /// 删除当前目录中的文件或目录。
    RemovePath {
        /// 容器内部文件/目录路径，用于预览、单文件导出或删除。
        name: String,
    },
    /// 读取用户偏好设置。
    GetPreference {
        /// 设置键名，服务按用户偏好或系统展示白名单处理。
        key: String,
    },
    /// 写入用户偏好：value为JSON，具体键、类型和值域由服务校验。
    SetPreference {
        /// 设置键名，服务按用户偏好或系统展示白名单处理。
        key: String,
        /// 对应用户偏好的JSON值，具体类型与范围按key校验。
        value: serde_json::Value,
    },
    /// 设置页展示用的系统配置（只读白名单）。
    GetSystemSetting {
        /// 设置键名，服务按用户偏好或系统展示白名单处理。
        key: String,
    },
    /// 设置页切换默认空间；业务校验仍在 veil-service。
    SetDefaultSpace {
        /// 已登记的目标默认空间ID。
        id: String,
    },
    /// 免密时长；只保存秒数到 app.db，密码仍由应用层保存在内存中。
    SetUnlockCache {
        /// 设置的秒数，允许范围由服务对应偏好键校验。
        seconds: u64,
    },
    /// 设置流式导入的自动提交间隔（秒）。
    SetImportCommitInterval {
        /// 设置的秒数，允许范围由服务对应偏好键校验。
        seconds: u64,
    },
    /// 设置流式导入达到多少文件后自动提交一次。
    SetImportCommitCount {
        /// 每批提交文件数量阈值，服务校验正值及上限。
        count: u64,
    },
}

/// 把原生JSON请求翻译成公共服务请求并编码响应。
/// 入参：service（&VeilService）为客户端共享服务；request（Request）为已反序列化的按值请求。
/// 返回值：成功返回VeilBuffer；服务或编码失败为Err(String)。
/// 异常：不在本函数捕获panic，由veil_client_call外层处理可展开panic。
/// 注意事项：同步长任务会等待；设置分支借用本地字符串/JSON立即执行后返回。创建/打开会话可保留密码，函数结束不代表敏感副本全部销毁。
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
        // 以下设置类请求在 service 中没有同形请求，直接内联执行后提前返回。
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
        // 免密时长保存为 JSON 数值型的用户设置项 unlock_cache_seconds。
        Request::SetUnlockCache { seconds } => {
            let value = serde_json::json!(seconds);
            return serialize(service.execute(ServiceRequest::SetUserSetting {
                key: "unlock_cache_seconds",
                value: &value,
            }));
        }
        // 流式导入的自动提交间隔保存为 import_commit_interval_seconds。
        Request::SetImportCommitInterval { seconds } => {
            let value = serde_json::json!(seconds);
            return serialize(service.execute(ServiceRequest::SetUserSetting {
                key: "import_commit_interval_seconds",
                value: &value,
            }));
        }
        // 流式导入的自动提交文件数保存为 import_commit_count。
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

/// 将桥接支持的服务结果编码为约定载荷类型。
/// 入参：result（`Result<ServiceResponse,ServiceError>`）为服务结果，取得所有权。
/// 返回值：JSON支持项kind0；预览kind2/3；不稳定导出kind4空载荷；Done/Locked为JSON null；服务失败为Err文本。
/// 异常：编码失败或不支持响应记固定诊断并Err(String)。
/// 注意事项：不覆盖所有ServiceResponse变体；只保留错误Display文本而不传Rust链结构，但正文仍可能含业务详情。返回内存须显式归还。
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
        // 文件预览：稳定/不稳定两种 kind，Swift 用 kind==3 标记“结果可能过期”。
        ServiceResponse::Preview(bytes) => return Ok(VeilBuffer::new(bytes, 2)),
        ServiceResponse::PreviewUnstable(bytes) => return Ok(VeilBuffer::new(bytes, 3)),
        ServiceResponse::ExportUnstable => return Ok(VeilBuffer::new(Vec::new(), 4)),
        // 无返回内容的成功响应统一编码为 JSON null。
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

/// 为后续本机应用库配置原生客户端沙盒目录。
/// 入参：path（*const c_char）为NUL结尾UTF-8路径，null直接拒绝。
/// 返回值：配置成功true；null、非UTF-8或服务配置失败false。
/// 异常：没有catch_unwind；无效非空指针属于未定义行为，不能靠false检测。
/// 注意事项：须在客户端初始化/应用库使用前完成；仅本次借用字符串，服务保存路径值，不保留C指针。
///
/// # Safety
/// 非空path必须指向本次调用全程可读、正确NUL终止的有效分配，不能与释放或写入并发。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn veil_configure_app_data_dir(path: *const std::ffi::c_char) -> bool {
    if path.is_null() {
        veil_core::logging::error("bridge", "configure_directory", "INVALID_ARGUMENTS");
        return false;
    }
    // C 字符串 → &str 的转换：先按 NUL 结尾解析为 CStr，再校验 UTF-8。
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

/// 创建可供Swift长期持有的服务句柄并初始化日志。
/// 入参：无。
/// 返回值：*mut VeilService，Box分配后转裸指针。
/// 异常：无catch_unwind；线程创建等panic不能作为正常错误载荷返回，分配失败可中止进程。
/// 注意事项：允许有效生命周期内并发共享调用，但必须等全部调用结束才free；不是打开容器，不保留调用者密码。
#[unsafe(no_mangle)]
pub extern "C" fn veil_client_new() -> *mut VeilService {
    veil_core::logging::initialize();
    Box::into_raw(Box::new(VeilService::default()))
}

/// 接收原生客户端错误上下文并交统一日志入口。
/// 入参：category（u32）为类别；source（*const c_char）为来源，可null；line（u32）为行号；native_code（i64）为原生码；message为描述，可null。
/// 返回值：无。
/// 异常：可展开panic由catch_unwind忽略；非UTF-8替换为占位，非法地址不在可恢复范围。
/// 注意事项：日志按核心规则处理，但任意来源/正文不能视作天然安全；不保证记录已落盘，也不反馈失败。
///
/// # Safety
/// 所有非空字符串指针须在调用全程有效、可读且NUL终止；不能并发修改或释放。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn veil_log_client_error(
    category: u32,
    source: *const std::ffi::c_char,
    line: u32,
    native_code: i64,
    message: *const std::ffi::c_char,
) {
    // 日志函数也可能出错，用 catch_unwind 兜底：记录失败不影响客户端运行。
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // 空指针与非法 UTF-8 都退化为占位文案，保证日志调用总能完成。
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

/// 在原生生命周期边界尽力等待日志队列刷新。
/// 入参：无。
/// 返回值：无，flush错误及捕获的panic忽略。
/// 异常：可展开panic被捕获，进程中止/非法内存等不由此恢复。
/// 注意事项：同步调用仍可能等待刷新超时；flush不是磁盘fsync，也不保证所有历史记录持久化。
#[unsafe(no_mangle)]
pub extern "C" fn veil_logs_flush() {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let _ = veil_core::logging::flush();
    }));
}

/// 为原生诊断导出提供滚动日志JSON字节。
/// 入参：无。
/// 返回值：成功kind0 JSON，导出失败/捕获panic为kind1通用错误。
/// 异常：内层可展开panic转错误载荷，创建错误缓冲区本身的分配失败仍不可保证恢复。
/// 注意事项：可能等待队列刷新并读取日志文件；返回缓冲必须veil_buffer_free，导出不重新全面净化历史记录。
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

/// 归还由本桥接分配的服务句柄及其资源。
/// 入参：client（*mut VeilService）为原句柄，null无操作。
/// 返回值：无。
/// 异常：没有catch_unwind；非法/重复释放为未定义行为，服务析构可能同步等待后台线程。
/// 注意事项：释放不等于对正在执行的请求安全取消；Swift须先保证所有引用与调用结束。
///
/// # Safety
/// 非空client只能来自veil_client_new且尚未释放；全部并发调用结束后仅调用一次，不得与任何读取/调用竞争。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn veil_client_free(client: *mut VeilService) {
    if !client.is_null() {
        drop(unsafe { Box::from_raw(client) });
    }
}

/// 读取共享传输进度供Swift轮询，允许与长请求并发。
/// 入参：client（*const VeilService）为有效共享句柄，null可检测。
/// 返回值：成功kind0 Snapshot JSON，空闲也返回状态对象；null/编码失败/捕获panic为kind1。
/// 异常：可展开panic转通用错误；悬空非空指针不被验证。
/// 注意事项：快照仅共享进度槽，不是所有并发操作总览；读取仍可能等待内部Mutex，缓冲须归还。
///
/// # Safety
/// client非空时须来自本库且调用全程存活，不能与veil_client_free竞争。
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

/// 向共享传输进度请求协作取消。
/// 入参：client为有效服务只读指针，null直接false。
/// 返回值：true为首次接受取消；false也可能无可取消任务、已请求取消、null或捕获panic。
/// 异常：可展开panic折为false，非法非空指针不保证恢复。
/// 注意事项：不强制终止线程、不撤销已提交文件；不应把false统一解释为句柄损坏。
///
/// # Safety
/// 非空client在调用期间须有效且未释放；可与业务调用并发，不可与free竞争。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn veil_client_cancel_transfer(client: *const VeilService) -> bool {
    if client.is_null() {
        veil_core::logging::error("bridge", "cancel_transfer", "INVALID_CLIENT");
        return false;
    }
    catch_unwind(AssertUnwindSafe(|| unsafe { &*client }.cancel_transfer())).unwrap_or(false)
}

/// 解析Swift的JSON请求并同步执行，再返回有类型的桥接结果。
/// 入参：client（*mut VeilService）为服务；data（*const u8）为只读JSON字节；length（usize）须1至1MiB。
/// 返回值：成功kind0/2/3/4；参数、解析、服务或捕获panic失败为kind1文本。
/// 异常：null/长度检查可返回错误，合法指针契约外的内存错误不能被catch_unwind兜底；仅可展开panic被转通用错误。
/// 注意事项：先复制原始字节到Zeroizing并在释放时清零；serde解析出的String、Session及调用方输入另有生命周期，不能承诺全清。长任务阻塞当前调用，进度/冲突需并发轮询。
///
/// # Safety
/// client须来自本库且存活至返回；data须指向至少length字节稳定可读内存，复制期间不能改写/释放；不能与client_free竞争。
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
        // 此原始JSON字节副本在作用域释放时零化；解析后的字段和调用方输入不是同一份内存。
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

/// 把本桥接返回的原始载荷重建成Box切片，零化后释放。
/// 入参：buffer（VeilBuffer）按值传回，data/length须保持本库返回原值；null data无操作。
/// 返回值：无。
/// 异常：无catch_unwind；伪造长度/地址或重复释放属于未定义行为。
/// 注意事项：适用于call/progress/logs_export等所有本库缓冲；仅清此载荷字节，不清Swift已经复制出的Data，也不删除预览文件。
///
/// # Safety
/// 非空data必须是尚未释放的本库Box切片首地址且length精确匹配；无其他线程继续读此内存，仅释放一次。
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

    /// 在测试中模拟C请求并复制结果后归还Rust载荷。
    /// 入参：client为有效测试句柄；value（serde_json::Value）为请求对象。
    /// 返回值：(u32,`Vec<u8>`)为kind和独立响应副本。
    /// 异常：序列化unwrap及无效指针可使测试失败或违反安全契约。
    /// 注意事项：输出副本不随veil_buffer_free零化，测试方负责其生命周期。
    ///
    /// # Safety
    /// client须为有效未释放句柄，且不能与free竞争。
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

    /// 验证非法请求只返回 kind=1 的通用错误，且错误文本不会回显请求中的
    /// 密码等敏感字段。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    /// 异常：测试准备或断言失败会 panic；场景中预期的错误由测试体捕获或核对，不作为生产错误返回。
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

    /// 端到端测试的驱动器：以子进程方式运行 native_worker（独立的 app 数据
    /// 目录），校验子进程退出码并把 stdout/stderr 拼进断言信息。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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

    /// 原生链路端到端测试（子进程运行）：覆盖 建空间→建容器→导入冲突→
    /// 预览→导出→锁定→解锁→删容器 的完整业务流，并抽查日志脱敏。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
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
