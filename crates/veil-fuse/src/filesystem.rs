//! FUSE 文件系统实现

use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry,
    Request, FUSE_ROOT_ID,
};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use veil_core::container::Container;
use veil_core::index::{self, FileMeta, Node};

use crate::{Error, MountOptions, Result};

const TTL: Duration = Duration::from_secs(1);
const BLOCK_SIZE: u32 = 512;

/// Veil FUSE 文件系统
pub struct VeilFS {
    container: Arc<Mutex<Container>>,
    inode_map: HashMap<u64, String>,       // inode -> 虚拟路径
    path_map: HashMap<String, u64>,        // 虚拟路径 -> inode
    next_inode: u64,
}

impl VeilFS {
    /// 创建新的文件系统
    pub fn new(container: Container) -> Self {
        let mut fs = Self {
            container: Arc::new(Mutex::new(container)),
            inode_map: HashMap::new(),
            path_map: HashMap::new(),
            next_inode: 2, // 1 是根目录，从 2 开始
        };

        // 构建 inode 映射
        fs.build_inode_map();
        fs
    }

    /// 挂载文件系统
    pub fn mount(self, options: MountOptions) -> Result<()> {
        let mount_point = &options.mount_point;

        // 创建挂载点
        if !mount_point.exists() {
            std::fs::create_dir_all(mount_point)?;
        }

        // 构建挂载选项
        let mut fuse_options = vec![
            fuser::MountOption::FSName("veil".to_string()),
            fuser::MountOption::RO,
        ];

        if let Some(volname) = options.volname {
            fuse_options.push(fuser::MountOption::VolName(volname));
        }

        if options.allow_other {
            fuse_options.push(fuser::MountOption::AllowOther);
        }

        if options.allow_root {
            fuse_options.push(fuser::MountOption::AllowRoot);
        }

        tracing::info!("挂载 FUSE 文件系统到: {}", mount_point.display());

        // 挂载（阻塞）
        fuser::mount2(self, mount_point, &fuse_options)?;

        Ok(())
    }

    /// 构建 inode 映射表
    fn build_inode_map(&mut self) {
        let container = self.container.lock().unwrap();
        let files = index::list_files(container.root());

        for (path, _meta) in files {
            let inode = self.next_inode;
            self.inode_map.insert(inode, path.clone());
            self.path_map.insert(path, inode);
            self.next_inode += 1;
        }

        tracing::debug!("构建 inode 映射: {} 个文件", self.inode_map.len());
    }

    /// 根据 inode 获取虚拟路径
    fn get_path(&self, ino: u64) -> Option<&String> {
        if ino == FUSE_ROOT_ID {
            None // 根目录
        } else {
            self.inode_map.get(&ino)
        }
    }

    /// 根据虚拟路径获取 inode
    fn get_inode(&self, path: &str) -> Option<u64> {
        self.path_map.get(path).copied()
    }

    /// 获取文件属性
    fn get_file_attr(&self, ino: u64, path: &str) -> Option<FileAttr> {
        let container = self.container.lock().unwrap();
        let meta = container.get_file(path)?;

        Some(FileAttr {
            ino,
            size: meta.size,
            blocks: (meta.size + BLOCK_SIZE as u64 - 1) / BLOCK_SIZE as u64,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: FileType::RegularFile,
            perm: 0o444, // 只读
            nlink: 1,
            uid: 501,
            gid: 20,
            rdev: 0,
            blksize: BLOCK_SIZE,
            flags: 0,
        })
    }

    /// 获取目录属性
    fn get_dir_attr(&self, ino: u64) -> FileAttr {
        FileAttr {
            ino,
            size: 0,
            blocks: 0,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: FileType::Directory,
            perm: 0o555, // 只读目录
            nlink: 2,
            uid: 501,
            gid: 20,
            rdev: 0,
            blksize: BLOCK_SIZE,
            flags: 0,
        }
    }
}

impl Filesystem for VeilFS {
    /// 查找文件
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        tracing::debug!("lookup: parent={}, name={:?}", parent, name);

        let name_str = match name.to_str() {
            Some(s) => s,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        // 构建完整路径
        let path = if parent == FUSE_ROOT_ID {
            name_str.to_string()
        } else {
            match self.get_path(parent) {
                Some(parent_path) => format!("{}/{}", parent_path, name_str),
                None => {
                    reply.error(libc::ENOENT);
                    return;
                }
            }
        };

        // 查找 inode
        if let Some(ino) = self.get_inode(&path) {
            if let Some(attr) = self.get_file_attr(ino, &path) {
                reply.entry(&TTL, &attr, 0);
                return;
            }
        }

        reply.error(libc::ENOENT);
    }

    /// 获取文件属性
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        tracing::debug!("getattr: ino={}", ino);

        if ino == FUSE_ROOT_ID {
            reply.attr(&TTL, &self.get_dir_attr(ino));
            return;
        }

        if let Some(path) = self.get_path(ino) {
            if let Some(attr) = self.get_file_attr(ino, path) {
                reply.attr(&TTL, &attr);
                return;
            }
        }

        reply.error(libc::ENOENT);
    }

    /// 读取文件数据 - 关键方法！
    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        tracing::debug!(
            "read: ino={}, offset={}, size={} ({:.2} KB)",
            ino,
            offset,
            size,
            size as f64 / 1024.0
        );

        let path = match self.get_path(ino) {
            Some(p) => p.clone(),
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        // 流式解密读取
        let start = std::time::Instant::now();
        let container = self.container.lock().unwrap();

        match container.read_range(&path, offset as u64, size as usize) {
            Ok(data) => {
                let elapsed = start.elapsed();
                tracing::debug!(
                    "✓ read 完成: {} 字节 in {:.3}ms",
                    data.len(),
                    elapsed.as_secs_f64() * 1000.0
                );
                reply.data(&data);
            }
            Err(e) => {
                tracing::error!("read 失败: {}", e);
                reply.error(libc::EIO);
            }
        }
    }

    /// 列出目录
    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        tracing::debug!("readdir: ino={}, offset={}", ino, offset);

        if ino != FUSE_ROOT_ID {
            reply.error(libc::ENOTDIR);
            return;
        }

        let mut entries = vec![
            (FUSE_ROOT_ID, FileType::Directory, ".".to_string()),
            (FUSE_ROOT_ID, FileType::Directory, "..".to_string()),
        ];

        // 添加所有文件（扁平化，忽略目录结构）
        for (ino, path) in &self.inode_map {
            // 只显示根目录下的文件
            if !path.contains('/') {
                entries.push((*ino, FileType::RegularFile, path.clone()));
            }
        }

        // 返回条目
        for (i, (ino, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
            if reply.add(*ino, (i + 1) as i64, *kind, name) {
                break;
            }
        }

        reply.ok();
    }
}
