//! Veil 子命令实现及命令层公共辅助函数。
//!
//! 本模块通过 service 查询本机登记，并组合密码输入、链接和提示展示。
//! 所属模块：CLI交互适配；命令实现负责参数、终端提示与结果展示，容器业务委托 `veil-service`。
//! 依赖：服务请求/响应、SecretString、rpassword、终端编码和本地化。目标是统一入口错误与交互规则。
//!
//! 核心清单：`resolve_veil` 把名称/ID/链接/目录解析为容器句柄；`container_client`
//! 包装容器操作；`prompt_password`/`prompt_new_password` 提供三级密码输入
//! （参数 → 环境变量 → 交互）；`request` 统一转发服务请求并映射错误。

/// `veil add`：加密导入本地文件或目录（目录递归保留相对结构）。
pub mod add;
/// `veil config`：查看配置或持久化提示级别。
pub mod config;
/// `veil ex`：解密导出文件或文件夹到本地。
pub mod ex;
/// `veil exists`：判断容器内文件或目录是否存在。
pub mod exists;
/// `veil free`：以树状视图展示容器内容与统计。
pub mod free;
/// `veil info`：展示容器身份与内容统计。
pub mod info;
/// `veil init`：创建新容器、veil_dir、配置记录与链接文件。
pub mod init;
/// `veil link`：为容器重建 `.veil-link` 链接文件。
pub mod link;
/// `veil list`：列出文件名称、大小与加密时间。
pub mod list;
/// `veil logs`：导出本地诊断日志。
pub mod logs;
/// `veil mv`：移动或重命名容器内文件。
pub mod mv;
/// `veil pack`：导出 `.veil` 打包文件。
pub mod pack;
/// `veil passwd`：修改容器密码（--full 轮换文件密钥重加密）。
pub mod passwd;
/// `veil rm`：删除容器内文件或目录。
pub mod rm;
/// `veil shell`：交互式命令会话。
pub mod shell;
/// 导入/导出共用的进度显示（内部模块，不对外暴露）。
mod transfer;
/// `veil unpack`：在新设备上还原打包容器。
pub mod unpack;

use veil_core::error::VeilError;
use veil_core::metadata::{FileEntry, MetaData};
use veil_service::commands::ContainerHandle;
use veil_service::veil_ops::{ContainerPathKind, MovedPathKind, RemovedPathKind};
use veil_service::{
    ContainerOperation, ContainerOperationResult, ServiceError, ServiceRequest, ServiceResponse,
    VeilService,
};

use crate::error::Result;

/// 创建临时服务执行一次无持续会话要求的CLI请求，并转换服务错误。
/// 入参：request（ServiceRequest）按值消费，借用字段在同步调用内有效。
/// 返回值：ServiceResponse为对应业务结果。
/// 异常：ServiceError转CommandError并标记already_reported。
/// 注意事项：每次构造默认服务及后台队列，不保留打开会话；标记只抑制CLI重复日志，不代表底层每种错误都实际写过日志。
pub(crate) fn request(request: ServiceRequest<'_>) -> Result<ServiceResponse> {
    VeilService::default()
        .execute(request)
        .map_err(|error| map_service_error(error).already_reported())
}

/// 将服务错误转为CLI稳定错误码及本地化参数。
/// 入参：error（ServiceError）按值消费。
/// 返回值：CommandError：Core/Workflow/Import专门映射，其余转换通用WorkError。
/// 异常：无Result返回，不在此执行或恢复业务。
/// 注意事项：错误正文可能描述已部分完成操作；映射不证明回滚，也不自动标记已记录日志。
pub(crate) fn map_service_error(error: ServiceError) -> crate::error::CommandError {
    match error {
        ServiceError::Core(error) => error.into(),
        ServiceError::Workflow(error) => map_workflow_error(error),
        ServiceError::Import(error) => match error {
            // 用户取消导入按核心取消错误处理。
            veil_service::import::ImportError::Cancelled => VeilError::Cancelled.into(),
            // 源路径不存在 → 3001。
            veil_service::import::ImportError::SourceNotFound(path) => {
                crate::cli_error!(AddSourceNotFound, "path" => path.display())
            }
            // 源不是普通文件 → 3002。
            veil_service::import::ImportError::NotFile(path) => {
                crate::cli_error!(AddNotFile, "path" => path.display())
            }
            // 容器内目标路径非法（如逃逸到根外）→ 3003。
            veil_service::import::ImportError::InvalidDestination(path) => {
                crate::cli_error!(AddInvalidDestination, "path" => path)
            }
            // 源目录遍历失败 → 3004，附带底层错误文本。
            veil_service::import::ImportError::DirectoryRead { path, error } => {
                crate::cli_error!(AddDirectoryReadFailed, "path" => path.display(), "error" => error)
            }
        },
        // 兜底：未知服务错误归入通用业务错误并透传文本。
        other => VeilError::WorkError(other.to_string()).into(),
    }
}

/// 把创建、链接、备份准备及提交错误映射为命令行提示。
/// 入参：error（WorkflowError）为服务工作流错误。
/// 返回值：对应init/link/pack/unpack等CommandError，Core/Io委托通用映射。
/// 异常：无错误返回。
/// 注意事项：不只处理计划阶段，CreateFailed/LinkRegistrationFailed可发生在数据已写入之后；保留路径与原因供排查。
fn map_workflow_error(error: veil_service::workflow::WorkflowError) -> crate::error::CommandError {
    use veil_service::workflow::WorkflowError as E;
    match error {
        // ---- init：工作区选项互相冲突 ----
        E::ConflictingWorkOptions => crate::cli_error!(InitConflictingWorkOptions),
        E::DedicatedRequiresPath => crate::cli_error!(InitDedicatedRequiresWorkPath),
        E::PortableConflictsWork => crate::cli_error!(InitPortableConflictsWork),
        // ---- init：链接与容器命名校验 ----
        E::InvalidLinkName => crate::cli_error!(InitInvalidLinkName),
        E::InvalidName => crate::cli_error!(InitInvalidVeilName),
        E::InvalidLinkExtension(path) => {
            crate::cli_error!(InitInvalidLinkExtension, "path" => path.display())
        }
        E::LinkExists(path) => crate::cli_error!(LinkOutputExists, "path" => path.display()),
        E::ContainerExists(path) => crate::cli_error!(InitVeilExists, "path" => path.display()),
        E::DedicatedNotEmpty(path) => {
            crate::cli_error!(InitDedicatedPathNotEmpty, "path" => path.display())
        }
        // ---- unpack：包文件与目标目录校验 ----
        E::PackageNotFound(path) => crate::cli_error!(UnpackVeilNotFound, "path" => path.display()),
        E::UnpackNameInvalid => crate::cli_error!(UnpackNameExtractFailed),
        E::UnpackMissingId => crate::cli_error!(UnpackMissingVeilId),
        E::UnpackDirectoryExists(path) => {
            crate::cli_error!(UnpackDirectoryExists, "path" => path.display())
        }
        E::UnpackInvalidLinkExtension(path) => {
            crate::cli_error!(UnpackInvalidLinkExtension, "path" => path.display())
        }
        // ---- link：链接生成校验 ----
        E::LinkMissingId => crate::cli_error!(LinkMissingVeilId),
        E::LinkInvalidExtension(path) => {
            crate::cli_error!(LinkInvalidExtension, "path" => path.display())
        }
        E::PackOutputExists(path) => crate::cli_error!(PackOutputExists, "path" => path.display()),
        // ---- 其余：密码、配置与底层错误 ----
        E::PasswordEmpty => crate::cli_error!(PasswordEmpty),
        E::InvalidHintLevel => crate::cli_error!(ConfigInvalidLevel),
        E::CreateFailed { directory, source } => {
            crate::cli_error!(InitWorkCreateFailed, "path" => directory.display(), "error" => source)
        }
        E::LinkRegistrationFailed { path, source } => {
            crate::cli_error!(InitLinkRegistrationFailed, "path" => path.display(), "error" => source)
        }
        E::Core(error) => error.into(),
        E::Io(error) => error.into(),
    }
}

/// 解析命令的容器选择器，并展示缓存恢复链接的提示。
/// 入参：input（&str）为容器ID、名称、目录或链接路径，具体歧义处理由服务配置规则决定。
/// 返回值：ContainerHandle保存定位身份、目录及恢复提示。
/// 异常：定位失败映射并标already_reported；服务返回非预期变体报协议WorkError。
/// 注意事项：配置加载/链接恢复可写应用库或文件；不验证容器密码。恢复标志代表采用缓存分支，不一定本次新建链接。
pub fn resolve_veil(input: &str) -> Result<ContainerHandle> {
    // PrepareContainer 根据输入类型（链接/唯一名称/veil_id/容器目录）查本机登记。
    let resolved = match VeilService::default()
        .execute(ServiceRequest::PrepareContainer { selector: input })
    {
        Ok(ServiceResponse::ContainerPrepared(resolved)) => resolved,
        Err(error) => return Err(map_service_error(error).already_reported()),
        // 响应类型与请求不匹配属于服务协议错误。
        _ => return Err(VeilError::WorkError("服务响应类型不符合请求".into()).into()),
    };

    // 已恢复的链接需要提示用户。
    // 链接缺失时服务层用配置缓存恢复了完整正文，这里提醒用户缓存已写回。
    if let Some(link_path) = resolved.recovered_link() {
        crate::hints::show_link_recovery_hint(link_path);
    }

    Ok(resolved)
}

/// 命令层适配器：所有容器操作都交给服务请求入口。
///
/// 同时保存"已解析的容器句柄"与"服务入口"：句柄来自 PrepareContainer，
/// 承载稳定 veil_id 与目录信息；后续所有操作都针对该句柄执行，
/// 保证同一条命令始终作用于同一个容器。
pub struct CliContainer {
    /// 已解析的容器句柄：稳定 ID、展示名称、veil_dir 目录与可选链接路径。
    container: ContainerHandle,
    /// 服务入口：向 veil-service 转发容器操作请求。
    service: VeilService,
}

/// 为已定位容器构造命令层操作适配器。
/// 入参：resolved（&ContainerHandle）为定位结果。
/// 返回值：CliContainer，克隆句柄并创建独立默认服务。
/// 异常：无Result返回，默认服务线程创建失败可能panic。
/// 注意事项：不解锁或校验密码；句柄绑定预期ID，但文件系统状态可随后变化。
pub fn container_client(resolved: &ContainerHandle) -> CliContainer {
    CliContainer {
        container: resolved.clone(),
        service: VeilService::default(),
    }
}

impl CliContainer {
    /// 读取容器元数据和磁盘/MIME展示统计。
    /// 入参：password（&str）为容器密码。
    /// 返回值：ContainerInfo，含敏感元数据、逻辑字节总量及MIME计数映射。
    /// 异常：底层密码/身份/租约/数据库/文件错误映射CommandError；对应操作结果变体不符会unreachable! panic。
    /// 注意事项：磁盘统计不等于分配块；MIME按扩展名猜测，不认证全部文件正文。
    fn info(&self, password: &str) -> Result<veil_service::operations::ContainerInfo> {
        match self.execute(ContainerOperation::ReadInfo { password })? {
            ContainerOperationResult::Info(value) => Ok(value),
            // 服务层保证请求与响应一一对应，其余分支属于协议缺陷。
            _ => unreachable!(),
        }
    }
    /// 将适配器的一次容器操作转发给已定位目标。
    /// 入参：operation（ContainerOperation）为按值消费的请求；self保存ContainerHandle与服务。
    /// 返回值：ContainerOperationResult为对应操作结果。
    /// 异常：服务错误映射并标already_reported，非Operation响应报协议WorkError。
    /// 注意事项：同步调用，可能读写磁盘/等锁；不会由此自动创建解锁Session。
    fn execute(&self, operation: ContainerOperation<'_>) -> Result<ContainerOperationResult> {
        match self.service.execute(ServiceRequest::OperatePrepared {
            container: &self.container,
            operation,
        }) {
            Ok(ServiceResponse::Operation(result)) => Ok(result),
            Err(error) => Err(map_service_error(error).already_reported()),
            // 响应类型与请求不匹配属于服务协议错误。
            _ => Err(VeilError::WorkError("服务响应类型不符合请求".into()).into()),
        }
    }
    /// 读取容器身份、目录和文件元数据。
    /// 入参：password（&str）为容器密码。
    /// 返回值：MetaData，含文件/目录和解密数据密钥。
    /// 异常：底层密码/身份/租约/数据库/文件错误映射CommandError；对应操作结果变体不符会unreachable! panic。
    /// 注意事项：返回含敏感密钥；不是全部文件正文备份。
    fn read_meta(&self, password: &str) -> Result<MetaData> {
        match self.execute(ContainerOperation::ReadMetadata { password })? {
            ContainerOperationResult::Metadata(v) => Ok(v),
            _ => unreachable!(),
        }
    }
    /// 取得全部容器文件展示条目。
    /// 入参：password（&str）为容器密码。
    /// 返回值：`Vec<FileEntry>`，每项一个内部文件路径及其大小/版本/密文信息，空容器为空。
    /// 异常：底层密码/身份/租约/数据库/文件错误映射CommandError；对应操作结果变体不符会unreachable! panic。
    /// 注意事项：非分页，不读取全部密文正文。
    fn list_files(&self, password: &str) -> Result<Vec<FileEntry>> {
        match self.execute(ContainerOperation::ListFiles { password })? {
            ContainerOperationResult::Files(v) => Ok(v),
            _ => unreachable!(),
        }
    }
    /// 查询容器逻辑路径当前类型。
    /// 入参：path（&str）为内部路径，根可用空/点/斜杠；password为密码。
    /// 返回值：ContainerPathKind::File/Directory/Missing，带规范化路径。
    /// 异常：底层密码/身份/租约/数据库/文件错误映射CommandError；对应操作结果变体不符会unreachable! panic。
    /// 注意事项：目录为显式数据库节点，不是按文件名推断的隐式目录；结果是瞬时观察。
    fn path_kind(&self, path: &str, password: &str) -> Result<ContainerPathKind> {
        match self.execute(ContainerOperation::PathKind { path, password })? {
            ContainerOperationResult::PathKind(v) => Ok(v),
            _ => unreachable!(),
        }
    }
    /// 删除文件或目录子树的引用，并尽力同步回收密文。
    /// 入参：path为内部路径；password为密码。
    /// 返回值：RemovedPathKind，区分删除文件或目录。
    /// 异常：底层密码/身份/租约/数据库/文件错误映射CommandError；对应操作结果变体不符会unreachable! panic。
    /// 注意事项：物理删除失败可能静默保留密文，释放租约失败时逻辑删除可能已完成。
    fn remove_path(&self, path: &str, password: &str) -> Result<RemovedPathKind> {
        match self.execute(ContainerOperation::RemovePath { path, password })? {
            ContainerOperationResult::Removed(v) => Ok(v),
            _ => unreachable!(),
        }
    }
    /// 移动或改名容器树节点，返回实际落点。
    /// 入参：from/to（&str）为内部源/目标；password为密码。
    /// 返回值：(MovedPathKind,String)，目标为现有目录时可能追加源名称。
    /// 异常：底层密码/身份/租约/数据库/文件错误映射CommandError；对应操作结果变体不符会unreachable! panic。
    /// 注意事项：不移动磁盘密文、不改变内容版本；最终唯一约束错误未必统一映射同名业务码。
    fn move_path(&self, from: &str, to: &str, password: &str) -> Result<(MovedPathKind, String)> {
        match self.execute(ContainerOperation::MovePath { from, to, password })? {
            ContainerOperationResult::Moved(k, p) => Ok((k, p)),
            _ => unreachable!(),
        }
    }
    /// 执行普通数据库改密或完整内容密钥轮换。
    /// 入参：old/new（&str）为旧/新密码；full（bool）为true完整轮换、false仅数据库rekey。
    /// 返回值：完成为Ok(())。
    /// 异常：底层密码/身份/租约/数据库/文件错误映射CommandError；对应操作结果变体不符会unreachable! panic。
    /// 注意事项：完整轮换逐文件提交，可在失败前已完成部分迁移或已改数据库密码；重试需使用实际有效密码。
    fn change_password(&self, old: &str, new: &str, full: bool) -> Result<()> {
        match self.execute(ContainerOperation::ChangePassword { old, new, full })? {
            ContainerOperationResult::Done => Ok(()),
            _ => unreachable!(),
        }
    }
}

/// 以终端适配编码显示提示，再读取无回显密码。
/// 入参：prompt（&str）为显示提示。
/// 返回值：String为读取到的密码文本，可为空。
/// 异常：stdout写入/刷新或rpassword读取失败返回io::Error。
/// 注意事项：提示字节适配终端编码，密码不做该编码转换；返回普通String尚未受SecretString保护，不在这里验密码或非空。
fn prompt_password_adaptive(prompt: &str) -> std::io::Result<String> {
    use std::io::Write;

    // 提示文字需要适配旧式 Windows 控制台，密码内容本身保持 UTF-8。
    let prompt_bytes = crate::output_encoding::encode_for_display(prompt);
    std::io::stdout().write_all(&prompt_bytes)?;
    std::io::stdout().flush()?;

    // rpassword 从 TTY 直接读取并关闭回显，绕开标准输入缓冲。
    rpassword::read_password()
}

/// 按显式参数、VEIL_PASSWORD、终端的优先级取得已有容器密码。
/// 入参：prompt（&str）为交互提示；password_arg（`Option<String>`）为显式密码，Some优先且允许空串。
/// 返回值：SecretString保存选中来源的文本。
/// 异常：交互I/O失败转CommandError；环境缺失或非Unicode则回退交互，不作为独立错误。
/// 注意事项：不校验非空或密码正确；参数/环境路径跳过TTY，不移除环境变量。SecretString保护所持副本，不清除命令行或环境原来源。
///
/// # 示例
/// ```no_run
/// # fn main() -> veil_cli::error::Result<()> {
/// # use veil_cli::commands::prompt_password;
/// let password = prompt_password("请输入密码: ", Some("mypass".to_string()))?;
/// let password = prompt_password("请输入密码: ", None)?; // 环境变量或交互式
/// # Ok(())
/// # }
/// ```
pub fn prompt_password(
    prompt: &str,
    password_arg: Option<String>,
) -> Result<secrecy::SecretString> {
    // 显式参数优先级最高，适用于调用方已经获得密码的场景。
    if let Some(pwd) = password_arg {
        return Ok(secrecy::SecretString::from(pwd));
    }

    // 环境变量用于脚本和自动化，读取后立即包装为 SecretString。
    if let Ok(password) = std::env::var("VEIL_PASSWORD") {
        return Ok(secrecy::SecretString::from(password));
    }

    // 最后才进入 TTY 读取，rpassword 负责关闭回显。
    let password = prompt_password_adaptive(prompt)?;
    Ok(secrecy::SecretString::from(password))
}

/// 取得新密码并按交互与否执行确认和非空检查。
/// 入参：password_arg（`Option<String>`）为显式新密码；None尝试VEIL_PASSWORD，再进入交互。
/// 返回值：验证后的SecretString。
/// 异常：空密码、确认不一致、I/O/服务校验或协议错误返回CommandError。
/// 注意事项：参数和环境路径跳过二次确认；交互读两次。当前只验证非空，无强度规则。
///
/// # 示例
/// ```no_run
/// # fn main() -> veil_cli::error::Result<()> {
/// # use veil_cli::commands::prompt_new_password;
/// let password = prompt_new_password(Some("mypass".to_string()))?;
/// let password = prompt_new_password(None)?; // 环境变量或交互式确认
/// # Ok(())
/// # }
/// ```
pub fn prompt_new_password(password_arg: Option<String>) -> Result<secrecy::SecretString> {
    prompt_new_password_with_env(password_arg, "VEIL_PASSWORD")
}

/// 从指定环境变量或显式输入取得新密码，交互时要求确认。
/// 入参：password_arg（`Option<String>`）为最高优先级显式文本；env_name（&str）为环境键，改密传VEIL_NEW_PASSWORD。
/// 返回值：非空且在交互时确认相同的SecretString。
/// 异常：空值、两次不一致、终端I/O或服务校验错误传播。
/// 注意事项：环境非Unicode视为不可用并回退TTY；交互确认文本仍是普通String，不因SecretString包装主密码就自动清除所有临时副本。
pub fn prompt_new_password_with_env(
    password_arg: Option<String>,
    env_name: &str,
) -> Result<secrecy::SecretString> {
    // 显式参数跳过确认，调用方已经负责校验来源。
    if let Some(pwd) = password_arg {
        return validate_new_password(secrecy::SecretString::from(pwd));
    }

    // 指定环境变量同样跳过二次输入，便于非交互调用。
    if let Ok(password) = std::env::var(env_name) {
        return validate_new_password(secrecy::SecretString::from(password));
    }

    // 交互模式必须输入两次，不一致时不会向命令层返回任何密码。
    let password = prompt_password_adaptive(crate::i18n::t("prompt.new_password"))?;
    let confirm = prompt_password_adaptive(crate::i18n::t("prompt.confirm_password"))?;

    validate_password_confirmation(password, &confirm)
}

/// 拒绝交互两次新密码不一致，匹配后再做非空校验。
/// 入参：password（String）为首次密码；confirmation（&str）为确认文本。
/// 返回值：匹配且非空时返回SecretString。
/// 异常：不相同为PasswordMismatch；非空服务校验/I/O/协议错误传播。
/// 注意事项：比较为普通字符串相等，不是密码学验证；不清理借用confirmation的存储，由调用方负责。
fn validate_password_confirmation(
    password: String,
    confirmation: &str,
) -> Result<secrecy::SecretString> {
    // 任何不一致都拒绝，绝不让"手滑输错"的密码进入容器。
    if password != confirmation {
        crate::cli_bail!(PasswordMismatch);
    }
    validate_new_password(secrecy::SecretString::from(password))
}

/// 通过公共服务规则拒绝空的新密码。
/// 入参：password（SecretString）为待校验文本，借用expose_secret仅用于本次请求。
/// 返回值：验证通过后返回原SecretString。
/// 异常：空密码或非预期响应、服务错误转CommandError。
/// 注意事项：目前只检查is_empty，空白字符串仍可通过；不验证强度，不尝试解锁旧容器。
fn validate_new_password(password: secrecy::SecretString) -> Result<secrecy::SecretString> {
    use secrecy::ExposeSecret;

    // 当前公共服务只校验非空，不校验密码强度；命令层转发其结果。
    match request(ServiceRequest::ValidateNewPassword {
        password: password.expose_secret(),
    })? {
        ServiceResponse::PasswordValid => {}
        // 响应类型与请求不匹配属于服务协议错误。
        _ => return Err(VeilError::WorkError("服务响应类型不符合请求".into()).into()),
    }

    Ok(password)
}

#[cfg(test)]
mod tests {
    use super::validate_password_confirmation;

    /// 验证两次交互密码不一致时稳定返回密码错误。
    #[test]
    /// 入参：无；由 Rust 测试运行器调用，场景数据在测试体内准备。
    /// 返回值：无，断言通过表示不一致被拒绝且一致输入保留原文。
    /// 异常：准备和断言失败通过panic使测试失败。
    /// 注意事项：一致输入会调用非空服务校验；测试不验证终端无回显行为。
    fn mismatched_password_confirmation_is_rejected() {
        use secrecy::ExposeSecret;

        let error = validate_password_confirmation("first".to_string(), "second").unwrap_err();
        assert_eq!(error.code(), crate::error::ErrorCode::PasswordMismatch);

        let password = validate_password_confirmation("same".to_string(), "same").unwrap();
        assert_eq!(password.expose_secret(), "same");
    }
}
