//! 一致快照与导入业务；不复制运行中的单个数据库文件。
use crate::{
    config::AppConfig,
    error::Result,
    metadata::MetaData,
    storage::{container::ContainerDb, invalid},
    veil_ops::VeilManager,
};
use std::path::{Path, PathBuf};
use veil_core::{
    database::CONTAINER_DB,
    veil_package::{VeilPackageWriter, VeilUnpacker as RawUnpacker},
};

pub struct VeilUnpacker {
    path: PathBuf,
}
impl VeilUnpacker {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().into(),
        }
    }
    pub fn read_password_verifier(&self) -> Result<veil_core::veil_package::VeilPasswordVerifier> {
        RawUnpacker::new(&self.path).read_password_verifier()
    }
    pub fn read_encrypted_metadata(&self) -> Result<Vec<u8>> {
        RawUnpacker::new(&self.path).read_encrypted_metadata()
    }
    pub fn read_metadata(&self, password: &str) -> Result<(MetaData, Vec<u8>)> {
        let temp = tempfile::TempDir::new()?;
        let bytes = self.read_encrypted_metadata()?;
        std::fs::write(temp.path().join(CONTAINER_DB), &bytes)?;
        let db = ContainerDb::open(temp.path(), password, None)?;
        Ok((db.metadata()?, bytes))
    }
    pub fn unpack_verified(&self, directory: &Path, password: &str) -> Result<MetaData> {
        RawUnpacker::new(&self.path).unpack(directory)?;
        let db = ContainerDb::open(directory, password, None)?;
        let metadata = db.metadata()?;
        let expected: std::collections::HashSet<_> = metadata
            .files
            .iter()
            .map(|f| f.encrypted_name.as_str())
            .collect();
        let actual = std::fs::read_dir(directory)?
            .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect::<std::io::Result<Vec<_>>>()?;
        if actual.len() != expected.len() + 1
            || expected.iter().any(|name| !directory.join(name).is_file())
        {
            return Err(invalid("打包密文与数据库引用不一致"));
        }
        if db
            .conn
            .query_row("SELECT count(*) FROM operation_locks", [], |r| {
                r.get::<_, i64>(0)
            })?
            != 0
        {
            return Err(invalid("导出副本包含运行锁"));
        }
        // 全部密文都验证分块认证，但不落盘明文。
        let total = metadata
            .files
            .iter()
            .try_fold(0u64, |total, file| total.checked_add(file.size));
        veil_core::progress::stage("verifying", total);
        for f in &metadata.files {
            let n = db
                .node(&f.file_id)?
                .ok_or_else(|| invalid("文件身份缺失"))?;
            let key = db
                .keys()?
                .into_iter()
                .find(|k| Some(&k.key_id) == n.key_id.as_ref())
                .ok_or_else(|| invalid("文件密钥缺失"))?;
            veil_core::file_ops::decrypt_file_to_writer(
                &directory.join(&f.encrypted_name),
                &mut std::io::sink(),
                &*key.material()?,
                &f.nonce,
                f.size,
            )?;
        }
        veil_core::progress::finalizing();
        Ok(metadata)
    }
}

pub(crate) fn unpack_registered(
    package_path: &Path,
    veil_dir: &Path,
    expected_id: &str,
    name: &str,
    password: &str,
    config: &mut AppConfig,
    work_root: &Path,
    link_path: &Path,
) -> Result<usize> {
    if veil_dir != work_root.join(expected_id) || veil_dir.exists() || link_path.exists() {
        return Err(invalid("解包目标位置冲突"));
    }
    config.ensure_veil_id_available(expected_id)?;
    let staging = tempfile::TempDir::new_in(work_root)?;
    let metadata = VeilUnpacker::new(package_path).unpack_verified(staging.path(), password)?;
    if metadata.veil_id != expected_id {
        return Err(invalid("容器身份不一致"));
    }
    VeilManager::new(staging.path().into()).rename_container(name, password)?;
    // 原子发布目录，之后登记失败时保留经过验证的数据，便于重新登记。
    crate::fsutil::rename_no_replace(staging.path(), veil_dir)?;
    crate::fsutil::sync_directory(work_root)?;
    config.register_directory(veil_dir, expected_id, name, &metadata.created_at)?;
    config.register_link_at(expected_id, name, veil_dir, link_path)?;
    Ok(metadata.files.len())
}

pub struct VeilPacker {
    output_path: PathBuf,
    overwrite: bool,
    options: veil_core::locking::LockOptions,
}
impl VeilPacker {
    pub fn new(output: impl AsRef<Path>) -> Self {
        Self {
            output_path: output.as_ref().into(),
            overwrite: true,
            options: crate::storage::locks::default_options(),
        }
    }
    pub fn without_overwrite(mut self) -> Self {
        self.overwrite = false;
        self
    }
    pub fn with_lock_options(mut self, options: veil_core::locking::LockOptions) -> Self {
        self.options = options;
        self
    }
    pub fn pack_container(&self, manager: &VeilManager, password: &str) -> Result<usize> {
        let temporary = tempfile::TempDir::new()?;
        let manager =
            VeilManager::new(manager.veil_dir.clone()).with_lock_options(self.options.clone());
        let count = manager.snapshot_to(temporary.path(), password)?;
        let database = std::fs::read(temporary.path().join(CONTAINER_DB))?;
        let metadata = ContainerDb::open(temporary.path(), password, None)?.metadata()?;
        let total = metadata.files.iter().try_fold(0u64, |total, file| {
            total.checked_add(
                std::fs::metadata(temporary.path().join(&file.encrypted_name))
                    .ok()?
                    .len(),
            )
        });
        veil_core::progress::stage("packing", total);
        let mut writer = VeilPackageWriter::new(&self.output_path, &database)?;
        for f in &metadata.files {
            writer.add_file(&f.encrypted_name, &temporary.path().join(&f.encrypted_name))?;
        }
        veil_core::progress::finalizing();
        if self.overwrite {
            writer.finish()?;
        } else {
            writer.finish_without_overwrite()?;
        }
        Ok(count)
    }
}
