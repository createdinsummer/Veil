//! `veil add` 子命令：向工作区容器添加本地文件或目录。

use crate::error::Result;
use colored::Colorize;
use std::path::Path;
use veil_core::workspace_ops::{AddFileSpec, WorkspaceManager};
use walkdir::WalkDir;

/// 添加本地文件或目录到容器（工作区架构）。
///
/// 单文件可使用目标路径覆盖容器内名称；目录会递归添加并保持相对目录结构。
///
/// # 参数
/// - `container_name`: 容器名称或链接路径。
/// - `source`: 本地源文件或目录。
/// - `dest`: 可选的容器内目标路径。目录作为目标时表示目标前缀。
/// - `password`: 容器密码（`None` 则交互式输入）。
///
/// # 返回
/// - `Ok(())`: 文件已加密并添加到容器。
/// - `Err(CommandError)`: 参数、源路径、密码、加密或元数据提交失败。
///
/// # 示例
/// ```bash
/// veil add photos vacation.jpg
/// veil add photos vacation.jpg 2026/vacation.jpg
/// veil add photos ~/Pictures
/// ```
pub fn run(
    container_name: &str,
    source: &str,
    dest: Option<&str>,
    password: Option<String>,
) -> Result<()> {
    // 先解析链接或容器名，后续所有操作都针对解析出的工作区路径。
    let resolved = super::resolve_container(container_name)?;
    let workspace_path = resolved.workspace_path;

    // 提前收集所有源路径和目标路径，避免密码输入后才发现路径无效。
    let source_path = Path::new(source);
    if !source_path.exists() {
        crate::cli_bail!(AddSourceNotFound, "path" => source);
    }

    let files = collect_add_specs(source_path, dest)?;
    if files.is_empty() {
        println!("{}", crate::i18n::t("add.empty_dir").yellow());
        return Ok(());
    }

    println!("{}", crate::i18n::t("opening_container").cyan());
    let password_str =
        super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    let manager = WorkspaceManager::new(workspace_path);
    let encrypted_names = manager.add_files(&files, password)?;

    if files.len() == 1 {
        let target = &files[0].target;
        println!(
            "{}",
            crate::i18n::t1("add.file_added", "path", target).green()
        );
        println!(
            "{}",
            crate::i18n::t1("common.encrypted_name", "name", &encrypted_names[0]).bright_black()
        );
    } else {
        println!(
            "{}",
            crate::i18n::t1(
                "add.files_added",
                "count",
                &encrypted_names.len().to_string()
            )
            .green()
        );
    }

    Ok(())
}

/// 将文件或目录展开为工作区添加请求。
fn collect_add_specs(source: &Path, dest: Option<&str>) -> Result<Vec<AddFileSpec>> {
    if source.is_file() {
        let source_name = file_name(source)?;
        let target = match dest {
            Some(dest) if is_directory_target(dest) => {
                join_container_path(dest.trim_end_matches(['/', '\\']), &source_name)?
            }
            Some(dest) => dest.to_string(),
            None => source_name,
        };

        return Ok(vec![AddFileSpec::new(source, target)]);
    }

    if !source.is_dir() {
        crate::cli_bail!(AddNotFile, "path" => source.display());
    }

    let root_name = file_name(source)?;
    let target_root = match dest {
        Some(dest) if !dest.is_empty() => dest.trim_end_matches(['/', '\\']).to_string(),
        Some(_) => crate::cli_bail!(AddInvalidDestination, "path" => ""),
        None => root_name,
    };

    let mut specs = Vec::new();
    for entry in WalkDir::new(source).follow_links(false) {
        let entry = entry.map_err(|error| {
            crate::cli_error!(
                AddDirectoryReadFailed,
                "path" => source.display(),
                "error" => error
            )
        })?;

        if !entry.file_type().is_file() {
            continue;
        }

        let relative = entry.path().strip_prefix(source).map_err(|error| {
            crate::cli_error!(
                AddDirectoryReadFailed,
                "path" => entry.path().display(),
                "error" => error
            )
        })?;
        let relative = path_to_container_path(relative)?;
        let target = if target_root.is_empty() {
            relative
        } else {
            join_container_path(&target_root, &relative)?
        };

        specs.push(AddFileSpec::new(entry.path(), target));
    }

    Ok(specs)
}

/// 返回路径最后一级文件名。
fn file_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| crate::cli_error!(AddInvalidDestination, "path" => path.display()))
}

/// 判断目标参数是否明确表示目录。
fn is_directory_target(path: &str) -> bool {
    path.ends_with('/') || path.ends_with('\\')
}

/// 拼接容器内路径，统一使用 `/`。
fn join_container_path(prefix: &str, suffix: &str) -> Result<String> {
    let prefix = prefix.trim_matches('/');
    let suffix = suffix.trim_matches('/');
    let target = match (prefix.is_empty(), suffix.is_empty()) {
        (true, true) => String::new(),
        (true, false) => suffix.to_string(),
        (false, true) => prefix.to_string(),
        (false, false) => format!("{}/{}", prefix, suffix),
    };

    if target.is_empty() {
        crate::cli_bail!(AddInvalidDestination, "path" => "");
    }

    Ok(target)
}

/// 把本地相对路径转换为统一的容器内路径。
fn path_to_container_path(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        let part = component
            .as_os_str()
            .to_str()
            .ok_or_else(|| crate::cli_error!(AddInvalidDestination, "path" => path.display()))?;
        parts.push(part);
    }

    join_container_path("", &parts.join("/"))
}
