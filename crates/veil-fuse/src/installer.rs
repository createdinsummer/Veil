//! macFUSE 安装引导程序
//!
//! 检测 macFUSE 是否已安装，如果未安装则引导用户完成安装。

use std::process::Command;
use std::path::Path;

/// macFUSE 安装状态
#[derive(Debug, PartialEq)]
pub enum MacFuseStatus {
    /// 已安装且可用
    Installed,
    /// 已安装但需要重启
    InstalledNeedsReboot,
    /// 未安装
    NotInstalled,
}

/// 检测 macFUSE 安装状态
pub fn check_macfuse() -> MacFuseStatus {
    // 检查 macFUSE 文件系统是否存在
    let macfuse_path = Path::new("/Library/Filesystems/macfuse.fs");

    if !macfuse_path.exists() {
        return MacFuseStatus::NotInstalled;
    }

    // 检查内核扩展是否已加载
    let output = Command::new("kextstat")
        .output()
        .ok();

    if let Some(output) = output {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains("osxfuse") || stdout.contains("macfuse") {
            return MacFuseStatus::Installed;
        }
    }

    // 已安装但内核扩展未加载 = 需要重启
    MacFuseStatus::InstalledNeedsReboot
}

/// 引导用户安装 macFUSE
pub fn guide_installation() -> Result<(), Box<dyn std::error::Error>> {
    println!("\n╔══════════════════════════════════════════════════════════╗");
    println!("║  🚀 Veil FUSE 挂载 - 初次使用向导                        ║");
    println!("╚══════════════════════════════════════════════════════════╝\n");

    let status = check_macfuse();

    match status {
        MacFuseStatus::Installed => {
            println!("✅ macFUSE 已安装且可用\n");
            Ok(())
        }
        MacFuseStatus::InstalledNeedsReboot => {
            show_reboot_guide()
        }
        MacFuseStatus::NotInstalled => {
            show_install_guide()
        }
    }
}

/// 显示重启引导
fn show_reboot_guide() -> Result<(), Box<dyn std::error::Error>> {
    println!("⚠️  macFUSE 已安装，但需要重启电脑来加载内核扩展\n");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    println!("📝 重启步骤：\n");
    println!("  1. 保存所有工作");
    println!("  2. 关闭所有应用");
    println!("  3. 在终端执行：");
    println!("     \x1b[33msudo reboot\x1b[0m");
    println!("  4. 电脑重启后，再次运行此命令\n");

    println!("❓ 为什么需要重启？");
    println!("   macFUSE 是内核扩展，需要重启才能加载到系统中。\n");

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    Err("需要重启电脑".into())
}

/// 显示安装引导
fn show_install_guide() -> Result<(), Box<dyn std::error::Error>> {
    println!("📦 macFUSE 未安装\n");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    println!("💡 什么是 macFUSE？\n");
    println!("   macFUSE 允许 Veil 将加密容器挂载为虚拟磁盘。");
    println!("   安装后可以：");
    println!("   • 双击视频秒开（< 1 秒）");
    println!("   • 拖动进度条流畅");
    println!("   • 内存占用恒定（~64KB）\n");

    println!("📥 安装步骤：\n");

    // 检查是否有 Homebrew
    let has_brew = Command::new("which")
        .arg("brew")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if has_brew {
        println!("  \x1b[32m方法 1：使用 Homebrew（推荐）\x1b[0m\n");
        println!("  在终端执行：");
        println!("  \x1b[33mbrew install --cask macfuse\x1b[0m\n");
        println!("  然后重启电脑。\n");

        println!("  方法 2：手动下载安装\n");
        println!("  1. 访问：https://osxfuse.github.io/");
        println!("  2. 下载 macFUSE-4.x.dmg");
        println!("  3. 双击安装");
        println!("  4. 重启电脑\n");
    } else {
        println!("  \x1b[32m推荐方法：手动安装\x1b[0m\n");
        println!("  1. 访问：https://osxfuse.github.io/");
        println!("  2. 下载 macFUSE-4.x.dmg");
        println!("  3. 双击安装");
        println!("  4. 重启电脑\n");

        println!("  替代方法：先安装 Homebrew，再安装 macFUSE\n");
        println!("  1. 安装 Homebrew：");
        println!("     /bin/bash -c \"$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\"");
        println!("  2. 安装 macFUSE：");
        println!("     \x1b[33mbrew install --cask macfuse\x1b[0m");
        println!("  3. 重启电脑\n");
    }

    println!("⚠️  安装注意事项：\n");
    println!("  • 安装过程中需要输入管理员密码");
    println!("  • 首次安装需要在"系统设置"中允许内核扩展");
    println!("  • 必须重启电脑才能生效\n");

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");

    // 询问用户是否现在安装
    if has_brew {
        println!("💬 是否现在安装？\n");
        println!("  输入 'y' 自动安装（需要密码）");
        println!("  输入 'n' 手动安装（稍后运行）\n");

        print!("请选择 [y/n]: ");
        std::io::Write::flush(&mut std::io::stdout())?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;

        if input.trim().eq_ignore_ascii_case("y") {
            println!("\n🚀 开始安装 macFUSE...\n");

            let status = Command::new("brew")
                .args(&["install", "--cask", "macfuse"])
                .status()?;

            if status.success() {
                println!("\n✅ macFUSE 安装成功！\n");
                println!("⚠️  现在需要重启电脑来加载内核扩展。");
                println!("   重启后再次运行此命令即可使用。\n");
                println!("是否现在重启？[y/n]: ");
                std::io::Write::flush(&mut std::io::stdout())?;

                let mut reboot_input = String::new();
                std::io::stdin().read_line(&mut reboot_input)?;

                if reboot_input.trim().eq_ignore_ascii_case("y") {
                    println!("\n正在重启...");
                    Command::new("sudo").arg("reboot").status()?;
                } else {
                    println!("\n请手动重启电脑后再使用。");
                }
            } else {
                println!("\n❌ 安装失败。请尝试手动安装。");
            }
        } else {
            println!("\n请手动安装 macFUSE 后再运行此命令。");
        }
    }

    Err("需要安装 macFUSE".into())
}

/// 显示安装完成后的快速开始指南
pub fn show_quick_start() {
    println!("\n╔══════════════════════════════════════════════════════════╗");
    println!("║  🎉 macFUSE 已就绪！                                     ║");
    println!("╚══════════════════════════════════════════════════════════╝\n");

    println!("🚀 快速开始：\n");
    println!("  1. 挂载容器：");
    println!("     \x1b[33mveil mount-fuse vault.veil /tmp/vault\x1b[0m\n");
    println!("  2. 访问文件：");
    println!("     \x1b[33mopen /tmp/vault\x1b[0m\n");
    println!("  3. 卸载：");
    println!("     按 Ctrl+C 或执行 \x1b[33mumount /tmp/vault\x1b[0m\n");

    println!("✨ 享受秒开视频的快感吧！\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_macfuse() {
        let status = check_macfuse();
        // 只测试函数不报错
        println!("macFUSE 状态: {:?}", status);
    }
}
