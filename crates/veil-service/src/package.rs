//! 加密容器包的流式导出与导入；运行锁在导入暂存区内清理。
//!
//! 本模块实现第 2 版 .veil 包的业务边界：
//! - 导出（VeilPacker）：先取得容器级读租约固定「数据库＋密文」的同一版本，
//!   再把加密数据库与所引用的密文流式写入包，不复制整个容器目录；
//! - 导入（VeilUnpacker）：在临时暂存区解包，验证密码、容器身份、密文集合
//!   与全部分块认证后，由 unpack_registered 把暂存目录发布；导出副本里的运行锁（源设备瞬时状态）在
//!   暂存区清理，源容器的锁不受影响。
use crate::{
    config::AppConfig,
    error::Result,
    metadata::MetaData,
    storage::{
        container::ContainerDb,
        invalid,
        locks::{self, LockMode},
    },
    veil_ops::VeilManager,
};
use std::path::{Path, PathBuf};
use veil_core::{
    database::CONTAINER_DB,
    veil_package::{VeilPackageWriter, VeilUnpacker as RawUnpacker},
};

/// 便携包服务适配器：保存来源路径，提供数据库模型读取及完整引用/密文认证。
/// 不自行持有密码或固定暂存目录；是否使用隔离目录与何时发布，由具体方法和上层恢复流程共同决定。
pub struct VeilUnpacker {
    /// 待解包的 .veil 包文件路径。
    path: PathBuf,
}
impl VeilUnpacker {
    /// 功能：保存待导入便携包的位置，供后续读取、试密码与恢复使用。
    /// 入参：path: impl `AsRef<Path>` 为 .veil 包路径，允许相对路径。
    /// 返回值：VeilUnpacker，只持有路径副本。
    /// 异常：不读文件，不返回文件缺失或格式错误。
    /// 注意事项：每个实际操作才打开包，调用方应保持源稳定；此构造器不分配暂存目录或校验密码。
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().into(),
        }
    }
    /// 功能：提取包内数据库快照建立临时试密码对象，供界面在正式恢复前反馈口令可用性。
    /// 入参：self: &VeilUnpacker，为已保存来源路径的解包器。
    /// 返回值：Ok(VeilPasswordVerifier)，内部为已写入临时文件的加密数据库副本。
    /// 异常：包头/快照段非法、包或临时文件 I/O 失败传播 VeilError。
    /// 注意事项：不展开独立密文，也未在这里验证任何口令；校验器的 false 还可能代表 I/O/格式问题，不能只解释为密码错误。
    pub fn read_password_verifier(&self) -> Result<veil_core::veil_package::VeilPasswordVerifier> {
        RawUnpacker::new(&self.path).read_password_verifier()
    }
    /// 功能：读取包中的原始加密数据库段，为元数据查看或密码验证提供输入。
    /// 入参：self: &VeilUnpacker，使用其包路径。
    /// 返回值：Ok(`Vec<u8>`)，为完整数据库快照加密字节，含义不是 MetaData 模型的 JSON 序列化结果。
    /// 异常：打开/读取失败、头部或段长度非法传播 VeilError。
    /// 注意事项：委托 core 解包器，不解密、不核对后续全部密文；数据库段可能最多分配 512 MiB 内存。
    pub fn read_encrypted_metadata(&self) -> Result<Vec<u8>> {
        RawUnpacker::new(&self.path).read_encrypted_metadata()
    }
    /// 功能：在隔离临时位置用密码打开包内数据库，获取容器身份、文件清单和密钥快照。
    /// 入参：self: &VeilUnpacker；password: &str 为预期解锁此数据库的口令。
    /// 返回值：Ok((MetaData,`Vec<u8>`))，前者为解锁查询出的模型，后者仍是原始加密数据库字节。
    /// 异常：临时目录、快照读取/写入、数据库密码/结构或元数据校验失败传播 VeilError。
    /// 注意事项：返回 MetaData 含原始文件密钥，应限制持有/复制；只读数据库模型，不验证包内所有密文标签。
    /// 临时连接先关闭，再由 PoolScope 撤销池复用，最后删除临时目录，不把解锁数据库文件写到最终容器位置。
    pub fn read_metadata(&self, password: &str) -> Result<(MetaData, Vec<u8>)> {
        // 所有中间物都放进临时目录，绝不把解密后的数据库写进目标位置。
        let temp = tempfile::TempDir::new()?;
        let _connections = veil_core::database::PoolScope::new(temp.path().join(CONTAINER_DB));
        let bytes = self.read_encrypted_metadata()?;
        std::fs::write(temp.path().join(CONTAINER_DB), &bytes)?;
        // 以实际读表验证密码解锁包内数据库副本。
        let db = ContainerDb::open(temp.path(), password, None)?;
        Ok((db.metadata()?, bytes))
    }
    /// 功能：还原便携包并验证其数据库与全部引用密文，产出可供发布的容器元数据。
    /// 入参：self: &VeilUnpacker；directory: &Path 为调用方选择的解包目录，应是隔离暂存位置；password: &str 为数据库口令。
    /// 返回值：Ok(MetaData)，在数据库可打开、引用集合与目录条目一致、所有文件分块认证和精确长度检查通过后返回。
    /// 异常：解包结构、文件/数据库 I/O、密码/字段/引用、缺少文件身份或密钥、认证/长度及进度取消失败传播 VeilError。
    /// 注意事项：本方法直接写传入 directory，不自行创建临时守卫或原子发布；失败可能留下已经解出的文件。
    /// 会删除此副本的 operation_locks 全部运行锁记录，源包与源容器不受这条 DELETE 影响；明文只按分块短暂验证到 sink。
    pub fn unpack_verified(&self, directory: &Path, password: &str) -> Result<MetaData> {
        // 底层解包：写出数据库文件与全部密文。
        RawUnpacker::new(&self.path).unpack(directory)?;
        let db = ContainerDb::open(directory, password, None)?;
        // 运行锁只对源容器及其执行者有效；恢复副本不得继承这些瞬时状态。
        db.conn.execute("DELETE FROM operation_locks", [])?;
        let metadata = db.metadata()?;
        // 数据库引用的密文文件名集合（expected）。
        let expected: std::collections::HashSet<_> = metadata
            .files
            .iter()
            .map(|f| f.encrypted_name.as_str())
            .collect();
        // 磁盘上实际存在的条目集合（actual）。
        let actual = std::fs::read_dir(directory)?
            .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect::<std::io::Result<Vec<_>>>()?;
        // 实际条目数必须等于「引用密文数＋1 个数据库文件」，且每个引用密文都在。
        if actual.len() != expected.len() + 1
            || expected.iter().any(|name| !directory.join(name).is_file())
        {
            return Err(invalid("打包密文与数据库引用不一致"));
        }
        // 全部密文都验证分块认证，但不落盘明文。
        // 先算明文总字节数用于进度显示（checked_add 防溢出）。
        let total = metadata
            .files
            .iter()
            .try_fold(0u64, |total, file| total.checked_add(file.size));
        veil_core::progress::stage("verifying", total);
        // 逐文件验证：身份存在、密钥存在，再把密文整体解密进黑洞（sink）
        // 以触发全部块的认证校验；明文只短暂存在于分块缓冲区中，不保存成磁盘文件或整文件副本。
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

/// 功能：在工作区暂存、验证并改名导入容器，发布完整目录后建立本机登记和链接入口。
/// 入参：package_path: &Path 为源包；veil_dir: &Path 为最终目录，必须等于 work_root/expected_id；expected_id/name/password: &str 为预期身份、新展示名和口令；
/// config: &mut AppConfig 为本机登记门面；work_root: &Path 为已准备的目标空间根；link_path: &Path 为尚不存在的链接输出。
/// 返回值：Ok(usize)，为恢复的文件条数，不含目录数。
/// 异常：目标位置/ID 冲突、暂存解包验证、身份不符、数据库改名、目录发布/同步或登记/链接失败传播 VeilError。
/// 注意事项：发布前失败由 TempDir 尽力清理；成功改名后的登记或链接失败保留已验证容器，不能据 Err 删除它或认为未发布。
/// 不把本机登记和目录重命名纳入一个原子事务；调用方须按创建/空间流程协调并发，目标目录以禁止覆盖重命名作最终保护。
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
    // 目标位置与 ID 可用性先检查，避免解包完成后才发现冲突。
    if veil_dir != work_root.join(expected_id) || veil_dir.exists() || link_path.exists() {
        return Err(invalid("解包目标位置冲突"));
    }
    config.ensure_veil_id_available(expected_id)?;
    // 解包全程在工作根目录下的临时暂存区进行。
    let staging = tempfile::TempDir::new_in(work_root)?;
    let connections = veil_core::database::PoolScope::new(staging.path().join(CONTAINER_DB));
    let metadata = VeilUnpacker::new(package_path).unpack_verified(staging.path(), password)?;
    // 包内身份必须与预期 ID 一致，防止把 A 容器的包登记成 B。
    if metadata.veil_id != expected_id {
        return Err(invalid("容器身份不一致"));
    }
    // 把暂存副本的容器名称改成登记名（也顺带校验数据库可写）。
    VeilManager::new(staging.path().into()).rename_container(name, password)?;
    // 原子发布目录，之后登记失败时保留经过验证的数据，便于重新登记。
    drop(connections);
    crate::fsutil::rename_no_replace(staging.path(), veil_dir)?;
    crate::fsutil::sync_directory(work_root)?;
    // 发布成功后登记本机目录与链接；任一步失败数据已保留，可重新登记。
    config.register_directory(veil_dir, expected_id, name, &metadata.created_at)?;
    config.register_link_at(expected_id, name, veil_dir, link_path)?;
    Ok(metadata.files.len())
}

/// .veil 包的导出器：在容器级读租约下输出一致快照。
pub struct VeilPacker {
    /// 输出包文件路径。
    output_path: PathBuf,
    /// 是否允许覆盖已存在的输出文件（默认允许）。
    overwrite: bool,
    /// 租约等待选项（超时与取消）。
    options: veil_core::locking::LockOptions,
}
impl VeilPacker {
    /// 功能：为容器备份保存输出位置和默认租约等待策略。
    /// 入参：output: impl `AsRef<Path>` 为目标包路径，可为相对路径。
    /// 返回值：VeilPacker，默认 overwrite=true，等锁选项采用 service 默认 30 秒期限。
    /// 异常：不创建文件、不申请租约、不返回错误。
    /// 注意事项：调用方若需保护已有备份，应在实际 pack_container 前使用 without_overwrite；构造成功不代表目录可写或空间足够。
    pub fn new(output: impl AsRef<Path>) -> Self {
        Self {
            output_path: output.as_ref().into(),
            overwrite: true,
            options: crate::storage::locks::default_options(),
        }
    }
    /// 功能：配置导出时保护已有备份文件，避免默认替换行为。
    /// 入参：self: VeilPacker，按值修改当前构造器策略。
    /// 返回值：VeilPacker，overwrite=false，其他路径和锁等待策略不变。
    /// 异常：不访问磁盘，不返回错误。
    /// 注意事项：只决定最后发布方式，不立即检查目标，也不创建占位；冲突可能在全部写包完成后才报告。
    pub fn without_overwrite(mut self) -> Self {
        self.overwrite = false;
        self
    }
    /// 功能：为后续导出申请容器读租约设置等待期限与取消标志。
    /// 入参：self: VeilPacker；options: LockOptions，timeout=None 可无限等，cancellation 为可共享原子取消条件。
    /// 返回值：更新 options 的 VeilPacker。
    /// 异常：只保存配置，不返回错误或申请租约。
    /// 注意事项：不改变租约有效期/续租周期，不代表包流式复制中的任意阻塞 I/O 都可被此选项强制终止。
    pub fn with_lock_options(mut self, options: veil_core::locking::LockOptions) -> Self {
        self.options = options;
        self
    }
    /// 功能：在容器级共享读租约下导出数据库快照和被引用密文，形成一份一致来源的备份包。
    /// 入参：self: &VeilPacker 为输出及覆盖/等待策略；manager: &VeilManager 提供源目录，内部新建管理器并应用本导出器选项；password: &str 为容器数据库口令。
    /// 返回值：Ok(usize)，为包内文件条数；完整临时包发布成功且 core 写入器收尾成功才返回。
    /// 异常：等锁/取消、数据库密码或结构、两次凭证核验、源快照/密文读取、目标写入/覆盖或同步失败传播 VeilError。
    /// 注意事项：前后验证读凭证，持守卫保护遵守同租约协议的写入；数据库字节整块读入内存，独立密文按流复制。
    /// 导出不对全部密文重新解密认证；最终发布后目录同步失败仍可能返回 Err，此时输出已存在。进度取消只在具体检查点生效。
    pub fn pack_container(&self, manager: &VeilManager, password: &str) -> Result<usize> {
        let manager =
            VeilManager::new(manager.veil_dir.clone()).with_lock_options(self.options.clone());
        // 容器级读租约固定数据库与密文文件的同一版本；密文直接流式写入包，
        // 不再复制整个容器到临时目录。
        let guard = manager
            .lock_manager(password)
            .acquire_container(LockMode::Read)?;
        let db = manager.open_db(password)?;
        // 打包开始前校验一次凭证，确保拿到的是真实有效的读租约。
        let credentials = guard.credentials()?;
        locks::validate_many(
            &db.conn,
            &credentials
                .iter()
                .map(|credential| (credential, Some(LockMode::Read)))
                .collect::<Vec<_>>(),
        )?;
        // 读租约保护遵守协议的内容写入，整读当前数据库文件作为快照；
        // 不在这里调用 SQLite backup，不能把此方法理解为允许绕过租约直接复制任意正在改写的库。
        let database = std::fs::read(manager.veil_dir.join(CONTAINER_DB))?;
        let metadata = db.metadata()?;
        // 估算总字节数用于进度条：所有密文文件长度之和。
        let total = metadata.files.iter().try_fold(0u64, |total, file| {
            total.checked_add(
                std::fs::metadata(manager.veil_dir.join(&file.encrypted_name))
                    .ok()?
                    .len(),
            )
        });
        veil_core::progress::stage("packing", total);
        // 先写数据库快照段，再逐个流式追加密文段。
        let mut writer = VeilPackageWriter::new(&self.output_path, &database)?;
        for f in &metadata.files {
            writer.add_file(&f.encrypted_name, &manager.veil_dir.join(&f.encrypted_name))?;
        }
        // 结束前再次校验凭证：期间租约若失效（如被回收），不能产出不一致的包。
        let credentials = guard.credentials()?;
        locks::validate_many(
            &db.conn,
            &credentials
                .iter()
                .map(|credential| (credential, Some(LockMode::Read)))
                .collect::<Vec<_>>(),
        )?;
        veil_core::progress::finalizing();
        // 按配置决定是否允许覆盖输出文件。
        if self.overwrite {
            writer.finish()?;
        } else {
            writer.finish_without_overwrite()?;
        }
        Ok(metadata.files.len())
    }
}
