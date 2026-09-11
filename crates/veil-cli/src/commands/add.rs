//! `veil add` 子命令：向工作区容器添加本地文件。

use anyhow::Result;
use colored::Colorize;
use std::path::Path;
use veil_core::workspace_ops::WorkspaceManager;

/// 添加文件到容器（工作区架构）。
///
/// 将本地文件加密后添加到工作区容器中。
///
/// # 参数
/// - `container_name`: 容器名称
/// - `source`: 源文件路径
/// - `dest`: 目标路径；当前版本保留参数接口但不会使用该值。
/// - `password`: 容器密码（`None` 则交互式输入）
///
/// # 返回
/// - `Ok(())`: 文件已加密并添加到容器
/// - `Err(anyhow::Error)`: 失败
///
/// # 示例
/// ```bash
/// veil add photos vacation.jpg
/// veil add photos ~/Pictures/photo.jpg
/// ```
pub fn run(
    container_name: &str,
    source: &str,
    _dest: Option<&str>,
    password: Option<String>,
) -> Result<()> {
    // 先解析链接或容器名，后续所有操作都针对解析出的工作区路径。
    let resolved = super::resolve_container(container_name)?;
    let workspace_path = resolved.workspace_path;

    // 提前校验源路径，避免用户输入密码后才发现文件不存在。
    let file = Path::new(source);
    if !file.exists() {
        anyhow::bail!(
            "{}",
            crate::i18n::t1("add.source_not_found", "path", source)
        );
    }

    if !file.is_file() {
        anyhow::bail!("{}", crate::i18n::t1("add.not_file", "path", source));
    }

    println!("{}", crate::i18n::t("opening_container").cyan());
    let password_str =
        super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    // 只在调用 core 时短暂暴露密码，不把明文密码继续传入后续流程。
    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // add_file 负责生成加密名、写入密文并更新加密元数据。
    let manager = WorkspaceManager::new(workspace_path);
    let encrypted_name = manager.add_file(file, password)?;

    let file_name = file.file_name().unwrap().to_string_lossy();
    println!(
        "{}",
        crate::i18n::t1("add.file_added", "path", &file_name).green()
    );
    println!(
        "{}",
        crate::i18n::t1("common.encrypted_name", "name", &encrypted_name).bright_black()
    );

    Ok(())
}
