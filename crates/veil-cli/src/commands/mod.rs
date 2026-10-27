//! Veil 子命令实现及命令层公共辅助函数。
//!
//! 本模块把全局配置解析、密码输入、链接路径生成和提示展示组合到各子命令中。
//! 命令实现负责用户交互与命令适配，容器操作逐步迁移到 `veil-service`。

pub mod add;
pub mod config;
pub mod ex;
pub mod exists;
pub mod free;
pub mod info;
pub mod init;
pub mod link;
pub mod list;
pub mod mv;
pub mod pack;
pub mod passwd;
pub mod rm;
pub mod shell;
pub mod unpack;

use std::path::Path;
use veil_core::error::VeilError;
use veil_core::metadata::{FileEntry, MetaData};
use veil_service::commands::ContainerHandle;
use veil_service::veil_ops::{ContainerPathKind, MovedPathKind, RemovedPathKind};
use veil_service::{
    ContainerOperation, ContainerOperationResult, ServiceError, ServiceRequest, ServiceResponse,
    VeilService,
};

use crate::error::Result;

pub(crate) fn request(request: ServiceRequest<'_>) -> Result<ServiceResponse> {
    VeilService::default()
        .execute(request)
        .map_err(|error| match error {
            ServiceError::Core(error) => error.into(),
            ServiceError::Workflow(error) => map_workflow_error(error),
            ServiceError::Import(error) => match error {
                veil_service::import::ImportError::SourceNotFound(path) => crate::cli_error!(AddSourceNotFound, "path" => path.display()),
                veil_service::import::ImportError::NotFile(path) => crate::cli_error!(AddNotFile, "path" => path.display()),
                veil_service::import::ImportError::InvalidDestination(path) => crate::cli_error!(AddInvalidDestination, "path" => path),
                veil_service::import::ImportError::DirectoryRead { path, error } => crate::cli_error!(AddDirectoryReadFailed, "path" => path.display(), "error" => error),
            },
            other => VeilError::WorkError(other.to_string()).into(),
        })
}

fn map_workflow_error(error: veil_service::workflow::WorkflowError) -> crate::error::CommandError {
    use veil_service::workflow::WorkflowError as E;
    match error {
        E::ConflictingWorkOptions => crate::cli_error!(InitConflictingWorkOptions),
        E::DedicatedRequiresPath => crate::cli_error!(InitDedicatedRequiresWorkPath),
        E::PortableConflictsWork => crate::cli_error!(InitPortableConflictsWork),
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
        E::PackageNotFound(path) => crate::cli_error!(UnpackVeilNotFound, "path" => path.display()),
        E::UnpackNameInvalid => crate::cli_error!(UnpackNameExtractFailed),
        E::UnpackMissingId => crate::cli_error!(UnpackMissingVeilId),
        E::UnpackDirectoryExists(path) => {
            crate::cli_error!(UnpackDirectoryExists, "path" => path.display())
        }
        E::UnpackInvalidLinkExtension(path) => {
            crate::cli_error!(UnpackInvalidLinkExtension, "path" => path.display())
        }
        E::LinkMissingId => crate::cli_error!(LinkMissingVeilId),
        E::LinkInvalidExtension(path) => {
            crate::cli_error!(LinkInvalidExtension, "path" => path.display())
        }
        E::PackOutputExists(path) => crate::cli_error!(PackOutputExists, "path" => path.display()),
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

/// 所有需要选择已有 Veil 的命令都必须经过这里。
///
/// 普通命令支持显式 `.veil-link` 路径、唯一 Veil 名称和唯一 `veil_id`。`veil_dir`
/// 仅由 `veil link` 用于恢复链接。无论输入形式如何，返回前都会读取
/// `.veil-info` 确认真实 `veil_id`。
///
/// 从配置缓存恢复链接后会展示恢复提示。
///
/// # 错误
/// 全局配置加载或 [`veil_service::config::GlobalConfig::resolve_veil`] 失败时返回错误。
pub fn resolve_veil(input: &str) -> Result<ContainerHandle> {
    let resolved = match VeilService::default()
        .execute(ServiceRequest::PrepareContainer { selector: input })
    {
        Ok(ServiceResponse::ContainerPrepared(resolved)) => resolved,
        Err(ServiceError::Core(error)) => return Err(error.into()),
        Err(error) => return Err(VeilError::WorkError(error.to_string()).into()),
        _ => return Err(VeilError::WorkError("服务响应类型不符合请求".into()).into()),
    };

    // 已恢复的链接需要提示用户。
    if let Some(link_path) = resolved.recovered_link() {
        crate::hints::show_link_recovery_hint(link_path);
    }

    Ok(resolved)
}

/// 命令层适配器：所有容器操作都交给服务请求入口。
pub struct CliContainer {
    container: ContainerHandle,
    service: VeilService,
}

pub fn container_client(resolved: &ContainerHandle) -> CliContainer {
    CliContainer {
        container: resolved.clone(),
        service: VeilService::default(),
    }
}

impl CliContainer {
    fn info(
        &self,
        password: &str,
    ) -> std::result::Result<veil_service::operations::ContainerInfo, VeilError> {
        match self.execute(ContainerOperation::ReadInfo { password })? {
            ContainerOperationResult::Info(value) => Ok(value),
            _ => unreachable!(),
        }
    }
    fn execute(
        &self,
        operation: ContainerOperation<'_>,
    ) -> std::result::Result<ContainerOperationResult, VeilError> {
        match self.service.execute(ServiceRequest::OperatePrepared {
            container: &self.container,
            operation,
        }) {
            Ok(ServiceResponse::Operation(result)) => Ok(result),
            Err(ServiceError::Core(error)) => Err(error),
            Err(error) => Err(VeilError::WorkError(error.to_string())),
            _ => Err(VeilError::WorkError("服务响应类型不符合请求".into())),
        }
    }
    fn read_meta(&self, password: &str) -> std::result::Result<MetaData, VeilError> {
        match self.execute(ContainerOperation::ReadMetadata { password })? {
            ContainerOperationResult::Metadata(v) => Ok(v),
            _ => unreachable!(),
        }
    }
    fn list_files(&self, password: &str) -> std::result::Result<Vec<FileEntry>, VeilError> {
        match self.execute(ContainerOperation::ListFiles { password })? {
            ContainerOperationResult::Files(v) => Ok(v),
            _ => unreachable!(),
        }
    }
    fn path_kind(
        &self,
        path: &str,
        password: &str,
    ) -> std::result::Result<ContainerPathKind, VeilError> {
        match self.execute(ContainerOperation::PathKind { path, password })? {
            ContainerOperationResult::PathKind(v) => Ok(v),
            _ => unreachable!(),
        }
    }
    fn add_file(&self, source: &Path, password: &str) -> std::result::Result<String, VeilError> {
        match self.execute(ContainerOperation::AddFile { source, password })? {
            ContainerOperationResult::AddedFile(v) => Ok(v),
            _ => unreachable!(),
        }
    }
    fn extract_file(
        &self,
        name: &str,
        output: &Path,
        password: &str,
    ) -> std::result::Result<(), VeilError> {
        match self.execute(ContainerOperation::ExtractFile {
            name,
            output,
            password,
        })? {
            ContainerOperationResult::Done => Ok(()),
            _ => unreachable!(),
        }
    }
    fn remove_path(
        &self,
        path: &str,
        password: &str,
    ) -> std::result::Result<RemovedPathKind, VeilError> {
        match self.execute(ContainerOperation::RemovePath { path, password })? {
            ContainerOperationResult::Removed(v) => Ok(v),
            _ => unreachable!(),
        }
    }
    fn move_path(
        &self,
        from: &str,
        to: &str,
        password: &str,
    ) -> std::result::Result<(MovedPathKind, String), VeilError> {
        match self.execute(ContainerOperation::MovePath { from, to, password })? {
            ContainerOperationResult::Moved(k, p) => Ok((k, p)),
            _ => unreachable!(),
        }
    }
    fn change_password(
        &self,
        old: &str,
        new: &str,
        full: bool,
    ) -> std::result::Result<(), VeilError> {
        match self.execute(ContainerOperation::ChangePassword { old, new, full })? {
            ContainerOperationResult::Done => Ok(()),
            _ => unreachable!(),
        }
    }
}

/// 显示提示词并读取一行无回显密码。
///
/// 提示词会先转换为当前终端编码；密码内容始终按 UTF-8 字符串读取。
///
/// # 错误
/// 标准输出写入、刷新或终端密码读取失败时返回 [`std::io::Error`]。
fn prompt_password_adaptive(prompt: &str) -> std::io::Result<String> {
    use std::io::Write;

    // 提示文字需要适配旧式 Windows 控制台，密码内容本身保持 UTF-8。
    let prompt_bytes = crate::output_encoding::encode_for_display(prompt);
    std::io::stdout().write_all(&prompt_bytes)?;
    std::io::stdout().flush()?;

    rpassword::read_password()
}

/// 提示用户输入密码（不回显）。
///
/// 支持三种方式（优先级从高到低）：
/// 1. 命令行参数：直接使用传入的密码
/// 2. 环境变量：从 `VEIL_PASSWORD` 读取（用于脚本/测试）
/// 3. 交互式：从 TTY 读取（不回显）
///
/// # 参数
/// - `prompt`: 提示文本（如 "请输入容器密码: "）
/// - `password_arg`: 命令行参数提供的密码（`None` 则尝试环境变量或交互式）
///
/// # 返回
/// - `Ok(SecretString)`: 成功获取密码
/// - `Err(CommandError)`: 失败（TTY 不可用或读取失败）
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

/// 提示用户输入密码并确认（用于创建容器或修改密码）。
///
/// 支持三种方式（优先级从高到低）：
/// 1. 命令行参数：直接使用传入的密码（跳过确认）
/// 2. 环境变量：从 `VEIL_PASSWORD` 读取（用于脚本/测试，跳过确认）
/// 3. 交互式：从 TTY 读取两次密码并确认（不回显）
///
/// # 参数
/// - `password_arg`: 命令行参数提供的密码（`None` 则尝试环境变量或交互式）
///
/// # 返回
/// - `Ok(SecretString)`: 成功获取密码
/// - `Err(CommandError)`: 失败（密码为空、两次输入不一致、TTY 不可用或读取失败）
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

/// 使用指定环境变量获取新密码，并在交互模式下要求输入两次。
///
/// 修改密码时传入 `VEIL_NEW_PASSWORD`，避免把旧密码环境变量静默复用为新密码。
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

/// 校验交互输入的新密码和确认密码完全一致。
fn validate_password_confirmation(
    password: String,
    confirmation: &str,
) -> Result<secrecy::SecretString> {
    if password != confirmation {
        crate::cli_bail!(PasswordMismatch);
    }
    validate_new_password(secrecy::SecretString::from(password))
}

/// 拒绝可用于新容器的空密码。
///
/// 旧容器仍可使用空密码打开，以保持对历史数据的兼容；只有创建或修改密码时
/// 禁止产生新的无密码容器。
fn validate_new_password(password: secrecy::SecretString) -> Result<secrecy::SecretString> {
    use secrecy::ExposeSecret;

    match request(ServiceRequest::ValidateNewPassword {
        password: password.expose_secret(),
    })? {
        ServiceResponse::PasswordValid => {}
        _ => return Err(VeilError::WorkError("服务响应类型不符合请求".into()).into()),
    }

    Ok(password)
}

#[cfg(test)]
mod tests {
    use super::validate_password_confirmation;

    /// 验证两次交互密码不一致时稳定返回密码错误。
    #[test]
    fn mismatched_password_confirmation_is_rejected() {
        use secrecy::ExposeSecret;

        let error = validate_password_confirmation("first".to_string(), "second").unwrap_err();
        assert_eq!(error.code(), crate::error::ErrorCode::PasswordMismatch);

        let password = validate_password_confirmation("same".to_string(), "same").unwrap();
        assert_eq!(password.expose_secret(), "same");
    }
}
