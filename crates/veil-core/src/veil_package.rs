//! 第 2 版便携包：加密数据库快照与独立密文，拒绝旧元数据格式。
//!
//! # 业务定位
//!
//! `.veil` 便携包用于跨设备迁移：把容器"搬"到另一台设备时，无法直接复制
//! 正在读写的 veil.db（数据库事务与密文引用可能变化、旧密文可能被并发清理），因此本模块
//! 定义第 2 版打包格式：加密数据库的一致快照 + 它所引用的全部独立密文文件。
//! 解包后在新设备独立解锁、验证身份与引用后才发布登记，不依赖源设备的应用库。
//!
//! # 磁盘布局（整数字段使用小端序，名称按 UTF-8 字节保存）
//!
//! ```text
//! ┌ 魔数 8B "VEILPKG\0" ┐
//! ├ version u16（固定 2）├
//! ├ header_size u32（固定 22）├
//! ├ metadata_size u32（数据库快照字节数，4096 对齐，≤512 MiB）├
//! ├ file_count u32（密文条目数，写完后回填）├
//! ├ 数据库快照（整库加密，原样字节）├
//! └ 每条密文：名称长度 u16 + 名称 + 内容长度 u64 + 内容 ┘
//! ```
//!
//! # 安全约定
//!
//! - 调用方应提供已加密且一致的快照与密文；本层原样复制，不自行加密或认证内容。
//! - 密文文件名必须符合 `<lock_id>_<随机UUID>.enc` 新格式；名称、长度、
//!   重复条目与尾部多余数据在解包时严格校验，防伪造/截断包。
//! - 旧格式（第 1 版、旧元数据头）明确拒绝，不提供兼容分支。
//!
//! 核心清单：VeilPackageHeader 解释段边界，VeilPackageWriter 流式追加并发布包，VeilUnpacker 还原结构，
//! VeilPasswordVerifier 持有临时数据库供试开；read_header / validate_name 统一基本格式检查。
//! 依赖：标准 Read/Write/Seek、tempfile 暂存文件、HashSet 去重、database 试开、progress 统计及 fsutil 目录同步。
//! 本层不建立业务租约或登记容器，快照一致性、内容认证及最终发布由 service 负责。
use crate::{
    database,
    error::{Result, VeilError},
};
use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
/// 包文件魔数：`VEILPKG` + 结尾 0，识别"这是 Veil 第 2 版便携包"。
pub const VEIL_PACKAGE_MAGIC: &[u8; 8] = b"VEILPKG\0";
/// 当前包格式版本号；只接受精确的 2，其他版本一律拒绝。
pub const VEIL_PACKAGE_VERSION: u16 = 2;
/// 包内数据库快照段上限 512 MiB；独立文件密文按 64 位长度流式处理，不受此限。
const MAX_DB: u32 = 512 * 1024 * 1024;
/// 功能：统一表达便携包结构或字段不符合项目格式的失败。
/// 入参：s: &str 为内部检查得到的原因说明。
/// 返回值：VeilError::InvalidFormat，拥有一份原因文本。
/// 异常：只构造错误对象，不以 Result 再次返回错误。
/// 注意事项：不记录日志，不自动清理已经写出的文件；原因不应包含密码或密钥原文。
fn invalid(s: &str) -> VeilError {
    VeilError::InvalidFormat(s.into())
}
/// 包文件头：22 字节固定头部，描述后续数据段的尺寸与数量。
///
/// 业务定位：解包器必须先读头才能知道数据库快照多大、有多少条密文；
/// 头部字段同时充当完整性校验点（版本、对齐、上限）。
#[derive(Debug, Clone)]
pub struct VeilPackageHeader {
    /// 包格式版本，固定 2。
    pub version: u16,
    /// 头部自身字节数，固定 22（供未来扩展识别）。
    pub header_size: u32,
    /// 紧随头部之后的加密数据库快照字节数（4096 对齐）。
    pub metadata_size: u32,
    /// 数据库快照之后的密文条目数。
    pub file_count: u32,
}
impl VeilPackageHeader {
    /// 功能：为即将写出的第 2 版便携包建立固定布局的头部数据。
    /// 入参：metadata_size: u32 为数据库快照字节数；file_count: u32 为密文条目数，写入开始时可为 0 占位。
    /// 返回值：VeilPackageHeader，version 为 2，header_size 为 22，其余字段直接保存传入值。
    /// 异常：不返回 Result，不验证快照大小或对齐。
    /// 注意事项：若需验证可被解包器接受，应检查 4096 对齐及 512 MiB 上限；构造本身不证明快照是有效加密数据库。
    pub fn new(metadata_size: u32, file_count: u32) -> Self {
        Self {
            version: 2,
            header_size: 22,
            metadata_size,
            file_count,
        }
    }
    /// 功能：将便携包头编码为磁盘格式，供开始写包和最终回填计数使用。
    /// 入参：self: &VeilPackageHeader 为待写头部。
    /// 返回值：`Vec<u8>`，固定 22 字节，按魔数、u16 版本、三个 u32 字段顺序拼接，整数均为小端序。
    /// 异常：不返回业务错误，不校验当前各字段是否合法。
    /// 注意事项：即使 self.header_size 不等于 22，实际输出仍为 22 字节；序列化成功不等于通过 from_bytes 的格式检查。
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::new();
        // 8 字节魔数在最前，识别包类型。
        b.extend_from_slice(VEIL_PACKAGE_MAGIC);
        // 2 字节版本号。
        b.extend_from_slice(&self.version.to_le_bytes());
        // 三个 4 字节字段：头部大小、快照大小、密文条数。
        for n in [self.header_size, self.metadata_size, self.file_count] {
            b.extend_from_slice(&n.to_le_bytes());
        }
        b
    }
    /// 功能：从包头识别格式和数据库段边界，拒绝旧版及无法按固定页格式解释的声明。
    /// 入参：b: &[u8] 为包含至少 22 字节头部的缓冲，可包含更多后续字节。
    /// 返回值：Ok(VeilPackageHeader)，要求魔数匹配、版本 2、头长 22，快照大小在 4096～512 MiB 且为 4096 的倍数。
    /// 异常：长度不足、魔数/版本/头长/快照大小不合规返回 InvalidFormat；长度检查后才解析固定切片。
    /// 注意事项：只解释前 22 字节，不验证后续数据段、文件数量真实性或数据库密码；尾部验证由解包流程完成。
    pub fn from_bytes(b: &[u8]) -> Result<Self> {
        // 头部不足 22 字节或魔数不符：不是本格式的包。
        if b.len() < 22 || &b[..8] != VEIL_PACKAGE_MAGIC {
            return Err(invalid("仅支持 veil pack 生成的新格式打包文件"));
        }
        // 按固定偏移解析各字段（小端序）。
        let h = Self {
            version: u16::from_le_bytes(b[8..10].try_into().unwrap()),
            header_size: u32::from_le_bytes(b[10..14].try_into().unwrap()),
            metadata_size: u32::from_le_bytes(b[14..18].try_into().unwrap()),
            file_count: u32::from_le_bytes(b[18..22].try_into().unwrap()),
        };
        if h.version != 2
            || h.header_size != 22
            || h.metadata_size < 4096
            || !h.metadata_size.is_multiple_of(4096)
            || h.metadata_size > MAX_DB
        {
            return Err(invalid(
                "仅支持 veil pack 生成的新格式打包文件；格式或数据库大小无效",
            ));
        }
        Ok(h)
    }
}
/// 打包写入器：先写同目录临时文件，全部完成后原子发布到目标路径。
///
/// 业务定位：避免目标位置出现"写到一半"的包；数据库快照在前、密文在后，
/// 全部写完后回填头部文件计数并落盘。
pub struct VeilPackageWriter {
    /// 最终发布的目标包路径。
    output: PathBuf,
    /// 同目录临时文件：写入过程中的暂存载体，成功后原子改名。
    temp: tempfile::NamedTempFile,
    /// 已写入的密文条数（发布前回填到头部）。
    count: u32,
    /// 已写入的密文名称集合，用于查重。
    names: HashSet<String>,
}
impl VeilPackageWriter {
    /// 功能：建立一份尚未公开的备份包，先写占位包头和调用方提供的数据库快照。
    /// 入参：output: impl `AsRef<Path>` 为最终包路径；database: &[u8] 为完整加密数据库快照字节。
    /// 返回值：Ok(VeilPackageWriter)，持有同目录临时文件、零文件计数和空名称集合，最终目标尚未替换。
    /// 异常：快照长度不能用 u32 表达或为空/超过 512 MiB 时返回 InvalidFormat；临时创建和写入失败传播 I/O 错误。
    /// 注意事项：父目录必须已存在；这里只检查非空和上限，不验证 4096 对齐、SQLCipher 内容或快照一致性。
    /// 上层须先取得一致快照并保护被引用密文，不能仅凭写入器构造成功确认包可恢复。
    pub fn new(output: impl AsRef<Path>, database: &[u8]) -> Result<Self> {
        let output = output.as_ref();
        // 目标父目录；空路径段（如相对文件名）退化为当前目录。
        let parent = output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let size = u32::try_from(database.len()).map_err(|_| invalid("数据库快照过大"))?;
        // 快照必须非空且不超过 512 MiB 上限。
        if size == 0 || size > MAX_DB {
            return Err(invalid("数据库快照大小无效"));
        }
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        // 先写占位头（文件数暂为 0），随后紧跟数据库快照字节。
        temp.write_all(&VeilPackageHeader::new(size, 0).to_bytes())?;
        temp.write_all(database)?;
        Ok(Self {
            output: output.into(),
            temp,
            count: 0,
            names: HashSet::new(),
        })
    }
    /// 功能：在包尾追加一份被数据库引用的密文，保存名称、声明长度和原始密文字节。
    /// 入参：self: &mut VeilPackageWriter 为当前写包状态；name: &str 为两个规范 UUID v4 用下划线连接并以 .enc 结尾的名称；
    /// path: &Path 为要复制的源密文路径，源在整个打包过程中应由上层保护不被替换或改写。
    /// 返回值：Ok(()) 表示写完声明长度并递增 count，names 记录此名称以拒绝重复条目。
    /// 异常：名称非法/重复、长度编码或计数溢出为 InvalidFormat；源读取、目标写入或复制失败传播 I/O 错误。
    /// 注意事项：按源打开时的文件大小限长复制，缩短会失败，变大后的多余尾部不会在这里检测；不校验密文认证标签。
    /// 名称先登记、内容随后写入，失败可能留下半条记录并占用 names；遇错应放弃此写入器，不在其上继续追加或发布。
    pub fn add_file(&mut self, name: &str, path: &Path) -> Result<()> {
        // 名称必须符合新格式密文命名，且同一包内不能重复。
        validate_name(name)?;
        if !self.names.insert(name.into()) {
            return Err(invalid("打包密文名称重复"));
        }
        let bytes = name.as_bytes();
        // 名称最长 u16 字节（配合解包端 128 上限校验）。
        let len = u16::try_from(bytes.len()).map_err(|_| invalid("名称过长"))?;
        let mut source = File::open(path)?;
        let size = source.metadata()?.len();
        // 逐段写：2 字节名称长度、名称字节、8 字节内容长度。
        self.temp.write_all(&len.to_le_bytes())?;
        self.temp.write_all(bytes)?;
        self.temp.write_all(&size.to_le_bytes())?;
        // 只复制元数据声明的 size 字节（take 限长），防止源文件并发变大；
        // 复制量不足 size 说明源文件在打包途中被改动。
        if crate::progress::copy(&mut Read::by_ref(&mut source).take(size), &mut self.temp)? != size
        {
            return Err(invalid("源密文长度变化"));
        }
        // 条数加一，防溢出。
        self.count = self
            .count
            .checked_add(1)
            .ok_or_else(|| invalid("文件过多"))?;
        Ok(())
    }
    /// 功能：结束写包并把完整临时包发布为最终备份，允许替换已有目标。
    /// 入参：self: VeilPackageWriter，按值消费写入器，调用方此后不能继续追加。
    /// 返回值：Ok(()) 表示回填文件数、文件同步、替换和父目录同步均成功。
    /// 异常：回填、同步或发布失败通过 VeilError 返回；替换后的目录同步也可能失败。
    /// 注意事项：返回错误不一定代表旧目标仍在，目录同步失败时新包已经生效；需要保护既有备份时使用 finish_without_overwrite。
    pub fn finish(self) -> Result<()> {
        self.publish(true)
    }
    /// 功能：结束写包并发布到尚不存在的最终位置，保护用户已有备份。
    /// 入参：self: VeilPackageWriter，取得完整写入器所有权。
    /// 返回值：Ok(()) 表示文件数回填、同步及禁止覆盖发布完成，并成功请求父目录同步。
    /// 异常：目标已存在、写入/同步或发布失败返回 VeilError；发布后目录同步失败也会返回错误。
    /// 注意事项：目标存在检查在最后发布时进行，可能已经完成整个包的生成；失败后须核对目标状态，不能把 Err 当成完全未发布。
    pub fn finish_without_overwrite(self) -> Result<()> {
        self.publish(false)
    }
    /// 功能：统一收尾写包，回填实际密文条数并按覆盖策略发布临时包。
    /// 入参：self: VeilPackageWriter 为尚未发布的包；overwrite: bool 为 true 时允许替换，false 时拒绝已有目标。
    /// 返回值：Ok(()) 表示文件偏移 18 处计数已更新、包内容已同步、发布和父目录同步成功。
    /// 异常：seek、写入、sync_all、persist 或父目录同步失败都传播为 VeilError。
    /// 注意事项：发布后的同步失败无法通过返回类型区分，目标可能已经是完整新包；调用方应核对权威状态，避免盲目删除或重试覆盖。
    /// 本方法不再次验证条目与数据库引用相符，也不为多个业务写包者申请互斥租约。
    fn publish(mut self, overwrite: bool) -> Result<()> {
        // 头部偏移 18 处是 file_count 字段，seek 回填实际条数。
        self.temp.seek(SeekFrom::Start(18))?;
        self.temp.write_all(&self.count.to_le_bytes())?;
        // 发布前请求同步完整临时文件，再改名；实际断电持久性取决于操作系统、文件系统与设备。
        self.temp.as_file().sync_all()?;
        if overwrite {
            // persist：原子替换目标路径上的旧包。
            self.temp
                .persist(&self.output)
                .map_err(|e| VeilError::Io(e.error))?;
        } else {
            // persist_noclobber：目标已存在则失败，避免覆盖他人刚发布的包。
            self.temp
                .persist_noclobber(&self.output)
                .map_err(|e| VeilError::Io(e.error))?;
        }
        // 发布后请求同步父目录；失败会向外返回，但此时最终目标已经被替换或创建。
        crate::fsutil::sync_directory(
            self.output
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?;
        Ok(())
    }
}
/// 解包前的密码验证器：把包内加密数据库快照落到临时文件上试开。
///
/// 业务定位：解包/导入前先验证用户密码能否解开包内数据库，避免解出内容后
/// 才发现密码错误；验证数据库只是临时副本，析构时清掉相关连接状态。
pub struct VeilPasswordVerifier {
    /// 包内数据库快照的临时副本，试开就发生在它上面。
    database: tempfile::NamedTempFile,
}
impl Drop for VeilPasswordVerifier {
    /// 功能：临时密码校验器释放前撤销其数据库池复用资格。
    /// 入参：self 为持有临时加密数据库文件的 VeilPasswordVerifier。
    /// 返回值：无；随后字段析构负责临时文件生命周期。
    /// 异常：invalidate 不返回错误，路径无法解析时静默跳过。
    /// 注意事项：不强制中断仍在借用的连接，也不等同于擦除磁盘数据。
    fn drop(&mut self) {
        // 通知数据库层该路径即将消失，清掉连接缓存，防止后续误用。
        database::invalidate(self.database.path());
    }
}
impl VeilPasswordVerifier {
    /// 功能：在正式解包前尝试打开包内数据库副本，给用户输入的密码提供初步反馈。
    /// 入参：self: &VeilPasswordVerifier 为持有临时数据库快照的校验器；password: &str 为待尝试的容器口令。
    /// 返回值：数据库统一打开及可读性检查全部成功为 true；任何失败均为 false。
    /// 异常：不保留 Result 失败原因，密码错误、数据库损坏、格式不支持或临时 I/O/连接池问题都可能得到 false。
    /// 注意事项：true 不证明包内全部密文完整，也不完成业务表与引用核对；不能把每个 false 都断言成“密码一定错误”。
    pub fn matches_password(&self, password: &str) -> bool {
        database::open(self.database.path(), Some(password), false).is_ok()
    }
}
/// 包解包器：把 `.veil` 包还原为数据库文件 + 密文文件集合。
///
/// 业务定位：读取并严格校验包结构，把内容写出到目标目录；输出只包含加密
/// 数据，名称、长度、重复条目与尾部均校验；只做结构还原，可信度与发布仍需 service 层完整验证。
pub struct VeilUnpacker {
    /// 待解包的 .veil 文件路径。
    path: PathBuf,
}
impl VeilUnpacker {
    /// 功能：为后续结构读取或解包保存便携包位置。
    /// 入参：path: impl `AsRef<Path>` 为包路径，可为相对路径；构造时复制为 PathBuf。
    /// 返回值：VeilUnpacker，仅持有路径，不缓存包内容或已打开句柄。
    /// 异常：不读文件，不返回文件不存在或格式错误。
    /// 注意事项：每次读取都会重新打开该路径，上层应避免在一次导入期间让包被并发替换。
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().into(),
        }
    }
    /// 功能：只提取便携包中的加密数据库快照，供密码检查或恢复准备使用。
    /// 入参：self: &VeilUnpacker，包含待打开包的路径。
    /// 返回值：Ok(`Vec<u8>`) 为包头声明的完整数据库字节，保持加密状态，最大 512 MiB。
    /// 异常：打开、读取、取文件长度失败传播 I/O 错误；头部不合规或快照超出剩余长度返回 InvalidFormat。
    /// 注意事项：数据库段整块分配到内存；只检查此段边界，不验证密文条目数量、内容、尾部或 SQLCipher 可读性。
    pub fn read_encrypted_metadata(&self) -> Result<Vec<u8>> {
        let mut file = File::open(&self.path)?;
        let header = read_header(&mut file)?;
        // 文件总长减去 22 字节头，就是可供快照+密文使用的剩余空间。
        let remaining = file.metadata()?.len().saturating_sub(22);
        // 声明的快照大小超过剩余空间 = 包被截断。
        if header.metadata_size as u64 > remaining {
            return Err(invalid("包内数据库被截断"));
        }
        let mut bytes = vec![0; header.metadata_size as usize];
        file.read_exact(&mut bytes)?;
        Ok(bytes)
    }
    /// 功能：把包内数据库快照复制到临时文件，建立可重复试密码的校验对象。
    /// 入参：self: &VeilUnpacker，为包含包路径的解包器。
    /// 返回值：Ok(VeilPasswordVerifier)，持有已写入并请求同步的加密数据库临时副本。
    /// 异常：包读取、头部/段长校验、临时创建、写入或同步失败传播 VeilError。
    /// 注意事项：这里尚未尝试密码，快照仍是加密字节；对象析构撤销对应连接池复用并由临时文件守卫尝试删除副本。
    pub fn read_password_verifier(&self) -> Result<VeilPasswordVerifier> {
        let mut database = tempfile::NamedTempFile::new()?;
        // 快照字节原样写入临时文件并落盘，之后用密码实际打开验证。
        database.write_all(&self.read_encrypted_metadata()?)?;
        database.as_file().sync_all()?;
        Ok(VeilPasswordVerifier { database })
    }
    /// 功能：按包记录还原加密数据库和密文集合，为上层在暂存目录验证后发布做准备。
    /// 入参：self: &VeilUnpacker 为包来源；directory: impl `AsRef<Path>` 为解包目录，允许缺失，会创建父级；已有同名输出一律拒绝覆盖。
    /// 返回值：Ok(`Vec<u8>`) 为已还原的加密数据库快照字节；目录中另外写出每条密文及 veil.db。
    /// 异常：头部、名称、重复条目、截断或多余尾部返回 InvalidFormat；目录、读取、复制、同步或新建输出失败传播 I/O 错误。
    /// 注意事项：会逐个创建正式文件，不提供整个目录的原子回滚；失败可能留下已还原文件或半截当前文件，上层应使用隔离暂存目录并清理。
    /// 不解密或认证密文，也不核对数据库引用；此方法读包两次，调用方应保持源包稳定，完成业务验证后才能登记为可用容器。
    pub fn unpack(&self, directory: impl AsRef<Path>) -> Result<Vec<u8>> {
        let directory = directory.as_ref();
        // 先读快照字节（同时校验头部），再重开文件从头解析。
        let bytes = self.read_encrypted_metadata()?;
        let mut file = File::open(&self.path)?;
        let header = read_header(&mut file)?;
        // 进入 unpacking 阶段，总量=整个包大小；已读过头部+快照计入进度。
        crate::progress::stage("unpacking", Some(file.metadata()?.len()));
        crate::progress::advance(u64::from(header.header_size) + u64::from(header.metadata_size));
        // 第一次 read_encrypted_metadata 已把游标推到快照末尾；
        // 重新打开后 seek 跳过头部+快照，对齐到第一条密文记录。
        file.seek(SeekFrom::Current(header.metadata_size as i64))?;
        std::fs::create_dir_all(directory)?;
        // names 集合记录本包已解出的密文名称，用于查重。
        let mut names = HashSet::new();
        // 逐条密文记录：每轮处理一条密文文件的还原。
        for _ in 0..header.file_count {
            // 读 2 字节名称长度。
            let mut len = [0; 2];
            file.read_exact(&mut len)?;
            let len = u16::from_le_bytes(len) as usize;
            // 名称长度必须在 1..=128：非空且防异常占用。
            if len == 0 || len > 128 {
                return Err(invalid("包内文件名称无效"));
            }
            // 读名称字节并转 UTF-8。
            let mut name = vec![0; len];
            file.read_exact(&mut name)?;
            let name = String::from_utf8(name).map_err(|_| invalid("包内名称不是 UTF-8"))?;
            // 名称必须符合新格式密文命名且不重复。
            validate_name(&name)?;
            if !names.insert(name.clone()) {
                return Err(invalid("包内密文名称重复"));
            }
            // 读 8 字节内容长度。
            let mut size = [0; 8];
            file.read_exact(&mut size)?;
            let size = u64::from_le_bytes(size);
            // 头部信息计入进度。
            crate::progress::advance(2 + len as u64 + 8);
            // 声明的长度不能超过文件剩余字节，否则是截断包。
            if size
                > file
                    .metadata()?
                    .len()
                    .saturating_sub(file.stream_position()?)
            {
                return Err(invalid("包内密文被截断"));
            }
            // create_new：目标目录已有同名文件就失败，绝不覆盖。
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(directory.join(&name))?;
            // 限长复制并核对实际复制量，防包内声明长度与实际内容不符。
            if crate::progress::copy(&mut Read::by_ref(&mut file).take(size), &mut output)? != size
            {
                return Err(invalid("包内密文被截断"));
            }
            output.sync_all()?;
        }
        // 所有密文之后不允许有任何多余字节（严格校验尾部）。
        let mut tail = [0; 1];
        if file.read(&mut tail)? != 0 {
            return Err(invalid("打包文件存在多余数据"));
        }
        // 最后把数据库快照还原为容器库文件（CONTAINER_DB=veil.db），同样防覆盖。
        let mut database = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join(database::CONTAINER_DB))?;
        database.write_all(&bytes)?;
        database.sync_all()?;
        // 请求同步已创建文件的目录项；不支持目录同步的平台可能为空操作，不能承诺所有掉电场景。
        crate::fsutil::sync_directory(directory)?;
        Ok(bytes)
    }
}
/// 功能：在当前文件游标处读取并验证一份固定便携包头。
/// 入参：file: &mut File 为已打开的包，调用者应把游标放在文件开头。
/// 返回值：Ok(VeilPackageHeader)，文件游标前进 22 字节。
/// 异常：不足 22 字节或读取失败传播 I/O 错误；字段校验失败由 from_bytes 返回 InvalidFormat。
/// 注意事项：不会自动 seek 到零，也不读取快照或后续条目；只应对本格式包按既定顺序调用。
fn read_header(file: &mut File) -> Result<VeilPackageHeader> {
    let mut bytes = [0; 22];
    file.read_exact(&mut bytes)?;
    VeilPackageHeader::from_bytes(&bytes)
}
/// 功能：限定包中每条输出都是符合容器命名规则的单个密文文件，防止任意路径写入。
/// 入参：name: &str 为条目名称，要求为规范小写 UUID v4、下划线、另一规范 UUID v4，最后接 .enc。
/// 返回值：两段 UUID、唯一分隔和后缀都合法时为 Ok(())。
/// 异常：后缀或任一 UUID 不合规、缺段或多段返回 InvalidFormat。
/// 注意事项：不访问磁盘、不确认第一段真是现存租约 ID，也不检查此名称是否已重复；重复检查由写入器或解包循环另外完成。
fn validate_name(name: &str) -> Result<()> {
    // 必须 .enc 结尾，剥离后缀得到 "uuid_uuid"。
    let base = name
        .strip_suffix(".enc")
        .ok_or_else(|| invalid("仅允许密文条目"))?;
    // 按下划线拆两段：锁 ID UUID 与随机 UUID；三段及以上即非法。
    let mut parts = base.split('_');
    if !parts.next().is_some_and(crate::metadata::is_uuid_v4)
        || !parts.next().is_some_and(crate::metadata::is_uuid_v4)
        || parts.next().is_some()
    {
        return Err(invalid("密文名称不符合新格式"));
    }
    Ok(())
}
