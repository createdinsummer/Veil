//! 容器打包流程：协调锁并调用核心库的包写入能力。

use crate::config::{GlobalConfig, VeilConfig};
use crate::error::VeilError;
use crate::locking::{self, LockOptions};
use crate::metadata::{MetaData, VEIL_INFO_FILE};
use crate::veil_ops::VeilManager;
use std::path::{Path, PathBuf};
use veil_core::veil_package::VeilPackageWriter;
use veil_core::veil_package::VeilUnpacker;

/// 将当前容器的元数据和密文打包为一个文件。
pub struct VeilPacker {
    output_path: PathBuf,
    lock_options: LockOptions,
    overwrite: bool,
}

/// 还原密文并登记容器，失败时清理本次创建的内容。
pub(crate) fn unpack_registered(
    package_path: &Path,
    veil_dir: &Path,
    expected_id: &str,
    name: &str,
    password: &str,
    config: &mut GlobalConfig,
    work_root: &Path,
    link_path: &Path,
) -> Result<usize, VeilError> {
    crate::work::validate_veil_id(expected_id)?;
    if veil_dir != work_root.join(expected_id) {
        return Err(VeilError::WorkError("容器目录与工作区不匹配".into()));
    }
    if name.trim().is_empty()
        || link_path.extension().and_then(|value| value.to_str()) != Some("veil-link")
    {
        return Err(VeilError::WorkError("容器名称或链接路径无效".into()));
    }
    if veil_dir.exists() || link_path.exists() {
        return Err(VeilError::WorkError("容器目录或链接已存在".into()));
    }
    let unpacker = VeilUnpacker::new(package_path);
    let (metadata, encrypted_metadata) = unpacker.read_metadata(password)?;
    if metadata.veil_id != expected_id {
        return Err(VeilError::InvalidFormat(
            "打包文件身份与元数据不一致".into(),
        ));
    }
    config.ensure_veil_id_available(expected_id)?;
    let count = metadata.files.len();
    let work_root_existed = work_root.exists();
    let result = (|| {
        unpacker.unpack_encrypted_files(veil_dir, &metadata)?;
        veil_core::temp::write_private_file_atomic(
            &veil_dir.join(VEIL_INFO_FILE),
            &encrypted_metadata,
        )?;
        if metadata.veil_name != name {
            VeilManager::for_veil(veil_dir.to_path_buf(), metadata.veil_id.clone())
                .update_meta(password, |metadata| metadata.veil_name = name.to_owned())?;
        }
        config.veils.remove(&metadata.veil_id);
        config.register_veil(VeilConfig {
            veil_id: metadata.veil_id.clone(),
            veil_name: name.to_owned(),
            created_at: metadata.created_at.clone(),
            last_accessed: None,
            links: Vec::new(),
        })?;
        config.register_link_at(&metadata.veil_id, name, veil_dir, link_path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(link_path);
        let _ = std::fs::remove_dir_all(veil_dir);
        if !work_root_existed {
            let _ = std::fs::remove_dir(work_root);
        }
    }
    result.map(|()| count)
}

impl VeilPacker {
    /// 绑定目标包路径。
    pub fn new(output_path: impl AsRef<Path>) -> Self {
        Self {
            output_path: output_path.as_ref().to_path_buf(),
            lock_options: LockOptions::default(),
            overwrite: true,
        }
    }

    /// 拒绝覆盖已存在的包。
    pub fn without_overwrite(mut self) -> Self {
        self.overwrite = false;
        self
    }

    /// 配置每个锁的等待超时和取消标志。
    pub fn with_lock_options(mut self, options: LockOptions) -> Self {
        self.lock_options = options;
        self
    }

    /// 验证元数据快照，逐文件持共享锁读取密文并发布包。
    pub fn pack(
        &self,
        veil_dir: impl AsRef<Path>,
        metadata: &MetaData,
        encrypted_metadata: &[u8],
    ) -> Result<(), VeilError> {
        let veil_dir = veil_dir.as_ref();
        let _maintenance = locking::maintenance(veil_dir, true, &self.lock_options)?;
        let config = locking::config(veil_dir, true, &self.lock_options)?;
        if std::fs::read(veil_dir.join(VEIL_INFO_FILE))? != encrypted_metadata {
            return Err(VeilError::WorkError(
                "元数据在打包前发生变化，请重新执行打包".into(),
            ));
        }
        drop(config);
        let mut writer = VeilPackageWriter::new(&self.output_path, encrypted_metadata)?;
        for entry in &metadata.files {
            let _file = locking::file(veil_dir, &entry.file_id, true, &self.lock_options)?;
            writer.add_file(&entry.original_name, &veil_dir.join(&entry.encrypted_name))?;
        }
        if self.overwrite {
            writer.finish()
        } else {
            writer.finish_without_overwrite()
        }
    }
}
