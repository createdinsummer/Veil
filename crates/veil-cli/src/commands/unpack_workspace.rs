use anyhow::Result;
use colored::Colorize;
use std::path::{Path, PathBuf};
use veil_core::config::{ContainerConfig, GlobalConfig};
use veil_core::container_format::ContainerUnpacker;
use veil_core::metadata::{MetaData, MetaHeader};
use veil_core::workspace::WorkspaceConfig;

/// 解包 .veil 容器文件到工作区
pub fn run_workspace(
    container_path: &str,
    container_name: Option<&str>,
    workspace_name: Option<&str>,
    link_output: Option<&str>,
    password: Option<String>,
) -> Result<()> {
    // 检查容器文件是否存在
    if !Path::new(container_path).exists() {
        anyhow::bail!(
            "{}",
            crate::i18n::t1("unpack.container_not_found", "path", container_path)
        );
    }

    // 确定容器名称
    let name = if let Some(n) = container_name {
        n.to_string()
    } else {
        // 从文件名提取
        let stem = Path::new(container_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("{}", crate::i18n::t("unpack.name_extract_failed")))?;
        stem.strip_suffix(".vault").unwrap_or(stem).to_string()
    };
    let link_path = link_output
        .map(PathBuf::from)
        .unwrap_or_else(|| super::default_link_path(&name));

    if link_path.exists() {
        anyhow::bail!(
            "{}",
            crate::i18n::t1(
                "link.output_exists",
                "path",
                &link_path.display().to_string()
            )
        );
    }

    // 加载配置
    let mut config = GlobalConfig::load()?;

    // 确保默认工作区配置存在
    if config.workspace.default.is_none() {
        config.workspace.default = Some(WorkspaceConfig::default_workspace()?);
    }

    // 确定工作区路径
    let workspace_root = if let Some(ws_name) = workspace_name {
        if ws_name == "default" {
            config.workspace.default.as_ref().unwrap().path.clone()
        } else {
            config
                .workspace
                .custom
                .get(ws_name)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "{}",
                        crate::i18n::t1("unpack.workspace_not_found", "name", ws_name)
                    )
                })?
                .path
                .clone()
        }
    } else {
        // 使用默认工作区
        config.workspace.default.as_ref().unwrap().path.clone()
    };

    let container_dir = workspace_root.join(&name);

    // 检查目录是否已存在
    if container_dir.exists() {
        anyhow::bail!(
            "{}",
            crate::i18n::t1(
                "unpack.directory_exists",
                "path",
                &container_dir.display().to_string()
            )
        );
    }

    println!("{}", crate::i18n::t("unpack.in_progress").cyan());
    let password_str =
        super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    use age::secrecy::ExposeSecret;
    let password = password_str.expose_secret();

    // 解包
    let unpacker = ContainerUnpacker::new(container_path);
    let encrypted_metadata = unpacker.unpack(&container_dir)?;

    // 解密元数据以获取文件映射
    let header = MetaHeader::from_bytes(&encrypted_metadata)?;

    // 派生密钥
    use veil_core::kdf;
    use zeroize::Zeroizing;
    let mut master_key = Zeroizing::new([0u8; 32]);
    kdf::derive_key(password.as_bytes(), &header.salt, &mut *master_key).map_err(|e| {
        anyhow::anyhow!(
            "{}",
            crate::i18n::t1("unpack.kdf_failed", "error", &format!("{:?}", e))
        )
    })?;

    // 解密元数据
    let header_len = MetaHeader::header_len(&encrypted_metadata)?;
    let encrypted_data = &encrypted_metadata[header_len..];

    use chacha20poly1305::aead::generic_array::GenericArray;
    use chacha20poly1305::{
        aead::{Aead, KeyInit},
        ChaCha20Poly1305,
    };

    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(&*master_key));
    let nonce_ga = GenericArray::from_slice(&header.nonce);

    let decrypted = cipher.decrypt(nonce_ga, encrypted_data).map_err(|e| {
        anyhow::anyhow!(
            "{}",
            crate::i18n::t1("unpack.decrypt_failed", "error", &e.to_string())
        )
    })?;

    let metadata = MetaData::from_json(&decrypted)?;

    // 现在需要重命名解包的文件到加密名称
    // 解包器使用原始文件名保存，但工作区需要加密文件名
    for file_entry in &metadata.files {
        let original_path = container_dir.join(&file_entry.original_name);
        let encrypted_path = container_dir.join(&file_entry.encrypted_name);

        if original_path.exists() {
            std::fs::rename(&original_path, &encrypted_path)?;
        }
    }

    // 写入元数据文件
    let meta_path = container_dir.join(".veil-meta");
    std::fs::write(&meta_path, &encrypted_metadata)?;

    // 更新配置
    let container_config = ContainerConfig {
        veil_id: metadata.veil_id.clone(),
        container_name: name.clone(),
        workspace: Some(workspace_name.unwrap_or("default").to_string()),
        container_dir: Some(name.clone()),
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

    // 显示解包解释提示
    crate::hints::show_unpack_explain_hint(&name, &link_path, metadata.files.len());

    Ok(())
}
