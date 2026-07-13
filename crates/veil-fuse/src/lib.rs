//! Veil FUSE 文件系统
//!
//! 将加密容器挂载为虚拟文件系统，支持流式解密。
//!
//! ## 特性
//!
//! - 🚀 真正的按需解密（只解密请求的部分）
//! - 📺 视频秒开（< 1 秒）
//! - 💾 内存占用恒定（~64KB）
//! - 🎯 随机访问（支持拖动进度条）
//!
//! ## 使用示例
//!
//! ```no_run
//! use veil_fuse::{VeilFS, MountOptions};
//! use veil_core::container::Container;
//!
//! let container = Container::open("vault.veil", "password")?;
//! let fs = VeilFS::new(container);
//! let options = MountOptions::default();
//! fs.mount(options)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod filesystem;
mod error;
pub mod installer;

pub use filesystem::VeilFS;
pub use error::{Error, Result};

/// 挂载配置
#[derive(Debug, Clone)]
pub struct MountOptions {
    /// 挂载点路径
    pub mount_point: std::path::PathBuf,

    /// 是否只读
    pub read_only: bool,

    /// 是否允许其他用户访问
    pub allow_other: bool,

    /// 是否允许 root 访问
    pub allow_root: bool,

    /// 卷名称
    pub volname: Option<String>,
}

impl Default for MountOptions {
    fn default() -> Self {
        Self {
            mount_point: std::path::PathBuf::from("/tmp/veil"),
            read_only: true,
            allow_other: false,
            allow_root: false,
            volname: Some("Veil".to_string()),
        }
    }
}
