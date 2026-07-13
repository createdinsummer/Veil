//! FUSE 挂载示例 - 真正的流式解密
//!
//! 用法：
//!   cargo run --release -p veil-fuse --example mount_fuse dome.veil
//!
//! 然后在另一个终端：
//!   ls /tmp/veil
//!   cat /tmp/veil/your-file.mp4 | head -c 1000000 > /dev/null

use veil_core::container::Container;
use veil_fuse::{MountOptions, VeilFS};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("用法: {} <container.veil>", args[0]);
        std::process::exit(1);
    }

    let container_path = &args[1];
    let password = rpassword::prompt_password("输入密码: ")?;

    // 初始化日志
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    println!("🔓 打开容器...");
    let container = Container::open(container_path, password)?;
    println!("✅ 容器已打开\n");

    // 创建文件系统
    let fs = VeilFS::new(container);

    // 配置挂载选项
    let options = MountOptions {
        mount_point: std::path::PathBuf::from("/tmp/veil"),
        read_only: true,
        allow_other: false,
        allow_root: false,
        volname: Some("Veil 加密容器".to_string()),
    };

    println!("📁 挂载到: {}", options.mount_point.display());
    println!("🎬 打开 Finder，访问挂载点");
    println!("💡 双击视频文件，体验秒开！\n");
    println!("按 Ctrl+C 卸载\n");

    // 挂载（阻塞，直到收到 umount 或 Ctrl+C）
    fs.mount(options)?;

    println!("✅ 已卸载");
    Ok(())
}
