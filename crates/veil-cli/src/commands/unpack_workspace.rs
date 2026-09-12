//! `veil unpack` 子命令：把 `.veil` 包还原为工作区容器。

use crate::error::Result;
use colored::Colorize;
use std::path::{Path, PathBuf};
use veil_core::config::{ContainerConfig, GlobalConfig};
use veil_core::container_format::ContainerUnpacker;
use veil_core::metadata::{MetaData, MetaHeader};
use veil_core::workspace::{WorkspaceConfig, allocate_container_directory};

/// 读取打包文件，分配工作区目录并重建容器和链接。
///
/// 解包分两阶段：先由 [`ContainerUnpacker`] 落盘入口密文，再用密码解密元数据并把
/// 原始名称重命名为工作区使用的加密名称。
///
/// # 参数
/// - `container_path`：待解包的 `.veil` 文件。
/// - `container_name`：可选的展示名称，省略时从文件名推断。
/// - `workspace_name`：可选的命名工作区，省略时使用默认工作区。
/// - `link_output`：可选的链接输出路径。
/// - `password`：可选的命令行密码。
///
/// # 错误
/// 文件不存在、目录或链接冲突、密码错误、元数据无效，以及任何文件读写失败时返回错误。
pub fn run_workspace(
    container_path: &str,
    container_name: Option<&str>,
    workspace_name: Option<&str>,
    link_output: Option<&str>,
    password: Option<String>,
) -> Result<()> {
    if !Path::new(container_path).exists() {
        crate::cli_bail!(UnpackContainerNotFound, "path" => container_path);
    }

    let name = if let Some(n) = container_name {
        n.to_string()
    } else {
        // 默认从包名推断名称，并去掉打包命令添加的 .vault 后缀。
        let stem = Path::new(container_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| crate::cli_error!(UnpackNameExtractFailed))?;
        stem.strip_suffix(".vault").unwrap_or(stem).to_string()
    };
    let mut config = GlobalConfig::load()?;

    if config.workspace.default.is_none() {
        config.workspace.default = Some(WorkspaceConfig::default_workspace()?);
    }

    let workspace_root = if let Some(ws_name) = workspace_name {
        if ws_name == "default" {
            config.workspace.default.as_ref().unwrap().path.clone()
        } else {
            config
                .workspace
                .custom
                .get(ws_name)
                .ok_or_else(|| {
                    crate::cli_error!(UnpackWorkspaceNotFound, "name" => ws_name)
                })?
                .path
                .clone()
        }
    } else {
        config.workspace.default.as_ref().unwrap().path.clone()
    };

    let unpacker = ContainerUnpacker::new(container_path);
    // 明文头部足以取得 veil_id，可在提示密码前完成目标目录分配和冲突检查。
    let encrypted_metadata = unpacker.read_encrypted_metadata()?;
    let header = MetaHeader::from_bytes(&encrypted_metadata)?;
    let veil_id = header.veil_id.clone();
    if veil_id.is_empty() {
        crate::cli_bail!(UnpackMissingVeilId);
    }

    let creation_time = super::creation_timestamp();
    let link_path = link_output
        .map(PathBuf::from)
        .unwrap_or_else(|| super::default_link_path_for_time(&name, &creation_time));

    if link_path.exists() {
        crate::cli_bail!(LinkOutputExists, "path" => link_path.display());
    }

    let container_dir = allocate_container_directory(&workspace_root, &veil_id, &creation_time);

    if container_dir.exists() {
        crate::cli_bail!(UnpackDirectoryExists, "path" => container_dir.display());
    }

    println!("{}", crate::i18n::t("unpack.in_progress").cyan());
    let password_str =
        super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    let encrypted_metadata = unpacker.unpack(&container_dir)?;

    // 解包后的 .veil-meta 仍带原始 TLV 头，使用同一套 Argon2id 流程解密。
    let header = MetaHeader::from_bytes(&encrypted_metadata)?;

    use veil_core::kdf;
    use zeroize::Zeroizing;
    let mut master_key = Zeroizing::new([0u8; 32]);
    kdf::derive_key(password.as_bytes(), &header.salt, &mut *master_key).map_err(|e| {
        crate::cli_error!(UnpackKdfFailed, "error" => format!("{:?}", e))
    })?;

    // 加密 JSON 紧随明文头部，nonce 来自头部字段。
    let header_len = MetaHeader::header_len(&encrypted_metadata)?;
    let encrypted_data = &encrypted_metadata[header_len..];

    use chacha20poly1305::aead::generic_array::GenericArray;
    use chacha20poly1305::{
        ChaCha20Poly1305,
        aead::{Aead, KeyInit},
    };

    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(&*master_key));
    let nonce_ga = GenericArray::from_slice(&header.nonce);

    let decrypted = cipher.decrypt(nonce_ga, encrypted_data).map_err(|e| {
        crate::cli_error!(UnpackDecryptFailed, "error" => e.to_string())
    })?;

    let metadata = MetaData::from_json(&decrypted)?;

    // 打包条目使用原始名称，工作区内容必须改用元数据记录的加密名称。
    for file_entry in &metadata.files {
        let original_path = container_dir.join(&file_entry.original_name);
        let encrypted_path = container_dir.join(&file_entry.encrypted_name);

        if original_path.exists() {
            std::fs::rename(&original_path, &encrypted_path)?;
        }
    }

    // 恢复 .veil-meta 后才能按工作区模型打开并继续增删文件。
    let meta_path = container_dir.join(".veil-meta");
    std::fs::write(&meta_path, &encrypted_metadata)?;

    let container_config = ContainerConfig {
        veil_id: metadata.veil_id.clone(),
        container_name: name.clone(),
        workspace: Some(workspace_name.unwrap_or("default").to_string()),
        container_dir: container_dir
            .file_name()
            .and_then(|directory| directory.to_str())
            .map(ToOwned::to_owned),
        workspace_path: None,
        dedicated: false,
        created_at: chrono::Utc::now().to_rfc3339(),
        last_accessed: None,
        links: Vec::new(),
    };

    config
        .containers
        .insert(metadata.veil_id.clone(), container_config);
    config.save()?;
    // 链接与容器记录使用同一 veil_id，后续移动工作区不会改变身份。
    config.register_link_at(&metadata.veil_id, &name, &container_dir, &link_path)?;

    println!(
        "{}",
        crate::i18n::t1("unpack.created", "name", &name).green()
    );
    println!(
        "{}",
        crate::i18n::t2(
            "unpack.stats",
            "count",
            &metadata.files.len().to_string(),
            "path",
            &container_dir.display().to_string()
        )
        .bright_black()
    );

    crate::hints::show_unpack_explain_hint(&name, &link_path, metadata.files.len());

    Ok(())
}
