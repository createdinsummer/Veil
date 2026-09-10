use anyhow::Result;
use colored::Colorize;
use veil_core::config::{GlobalConfig, ContainerConfig};
use veil_core::container_format::ContainerUnpacker;
use veil_core::metadata::{MetaHeader, MetaData};
use veil_core::workspace::WorkspaceConfig;
use std::path::Path;

/// 解包 .veil 容器文件到工作区
pub fn run_workspace(
    container_path: &str,
    container_name: Option<&str>,
    workspace_name: Option<&str>,
    password: Option<String>,
) -> Result<()> {
    // 检查容器文件是否存在
    if !Path::new(container_path).exists() {
        anyhow::bail!("容器文件不存在: {}", container_path);
    }

    // 确定容器名称
    let name = if let Some(n) = container_name {
        n.to_string()
    } else {
        // 从文件名提取
        Path::new(container_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("无法从文件名提取容器名称"))?
            .to_string()
    };

    // 加载配置
    let mut config = GlobalConfig::load()?;

    // 确保默认工作区配置存在
    if config.workspace.default.is_none() {
        config.workspace.default = Some(WorkspaceConfig::default_workspace()?);
    }

    // 检查容器是否已存在
    if config.containers.contains_key(&name) {
        anyhow::bail!("容器 '{}' 已存在，请使用不同的名称或先删除现有容器", name);
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
                .ok_or_else(|| anyhow::anyhow!("工作区 '{}' 不存在", ws_name))?
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
        anyhow::bail!("目录已存在: {}", container_dir.display());
    }

    println!("{}", "正在解包容器...".cyan());
    let password_str = super::prompt_password("请输入容器密码: ", password)?;

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
    kdf::derive_key(password.as_bytes(), &header.salt, &mut *master_key)
        .map_err(|e| anyhow::anyhow!("密钥派生失败: {:?}", e))?;

    // 解密元数据
    let header_len = MetaHeader::header_len(&encrypted_metadata)?;
    let encrypted_data = &encrypted_metadata[header_len..];

    use chacha20poly1305::{
        aead::{Aead, KeyInit},
        ChaCha20Poly1305,
    };
    use chacha20poly1305::aead::generic_array::GenericArray;

    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(&*master_key));
    let nonce_ga = GenericArray::from_slice(&header.nonce);

    let decrypted = cipher
        .decrypt(nonce_ga, encrypted_data)
        .map_err(|e| anyhow::anyhow!("解密失败（密码错误或数据损坏）: {}", e))?;

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
        workspace: Some(workspace_name.unwrap_or("default").to_string()),
        container_dir: Some(name.clone()),
        workspace_path: None,
        dedicated: false,
        created_at: chrono::Utc::now().to_rfc3339(),
        last_accessed: None,
    };

    config.containers.insert(name.clone(), container_config);
    config.save()?;

    println!("{}", format!("✓ 已解包容器 '{}'", name).green());
    println!("{}", format!("  文件数: {}  工作区: {}", metadata.files.len(), container_dir.display()).bright_black());

    Ok(())
}
