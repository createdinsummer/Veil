//! mount 子命令 - 挂载容器为网络磁盘

use veil_core::container::Container;
use anyhow::Result;

pub fn run(container_path: &str, password: Option<String>, auto_open: bool) -> Result<()> {
    // 获取密码
    let password = if let Some(pwd) = password {
        pwd
    } else {
        rpassword::prompt_password("输入密码: ")?
    };

    println!("正在打开容器...");
    let start = std::time::Instant::now();
    let container = Container::open(container_path, password)?;
    println!("✓ 容器已打开 ({:.2}s)", start.elapsed().as_secs_f64());

    println!("正在启动 WebDAV 服务器并挂载...");
    println!();

    // 使用 tokio runtime
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        mount_async(container, auto_open).await
    })
}

async fn mount_async(container: Container, auto_open: bool) -> Result<()> {
    use veil_webdav::{MountManager, MountConfig};

    // 初始化日志
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let mut manager = MountManager::new();

    let config = MountConfig {
        port_start: 18080,
        auto_open,
        show_notice: true,
    };

    match manager.mount(container, config).await {
        Ok(info) => {
            println!("✅ 挂载成功！");
            println!("   挂载点: {}", info.mount_point.display());
            println!("   端口: {}", info.port);
            println!();
            println!("按 Ctrl+C 退出（自动卸载）");

            // 等待 Ctrl+C
            tokio::signal::ctrl_c().await?;

            println!();
            println!("正在卸载...");
            manager.unmount(&info.container_path).await?;
            println!("✅ 已卸载");

            Ok(())
        }
        Err(e) => {
            Err(anyhow::anyhow!("挂载失败: {}", e))
        }
    }
}
