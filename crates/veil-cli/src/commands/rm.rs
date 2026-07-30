use anyhow::Result;
use colored::Colorize;
use veil_core::container::Container;

/// 从容器删除文件。
///
/// 按虚拟路径删除容器内的文件，并更新索引。被删除文件的加密数据仍留在容器中
/// 成为死空间（后续可通过 compaction 回收）。
///
/// 支持通配符模式（`*` 和 `**`）批量删除。
///
/// # 参数
/// - `container_path`: 容器文件路径
/// - `virtual_path`: 要删除的虚拟路径（容器内路径，可包含通配符）
/// - `password`: 容器密码（`None` 则交互式输入）
///
/// # 返回
/// - `Ok(())`: 文件已从索引中移除
/// - `Err(anyhow::Error)`: 失败（密码错误、文件不存在或写入失败）
///
/// # 示例
/// ```bash
/// # 删除单个文件
/// veil rm photos.veil archive/old.jpg mypass
///
/// # 删除匹配的文件（通配符）
/// veil rm photos.veil "temp/*.tmp" mypass
/// veil rm photos.veil "**/*.log" mypass
/// ```
pub fn run(container_path: &str, virtual_path: &str, password: Option<String>) -> Result<()> {
    let password = super::prompt_password("请输入容器密码: ", password)?;

    println!("{}", "正在打开容器...".cyan());
    let mut container = Container::open(container_path, password)?;

    // 检查是否包含通配符
    if virtual_path.contains('*') {
        // 通配符模式匹配
        println!("{} 正在查找匹配的文件: {}", "→".blue(), virtual_path);
        let deleted = container.remove_matched(virtual_path)?;

        if deleted.is_empty() {
            println!("{} 未找到匹配的文件", "⚠".yellow());
        } else {
            println!("{} 已删除 {} 个文件:", "✓".green(), deleted.len());
            for path in deleted {
                println!("  - {}", path);
            }
        }
    } else {
        // 精确路径匹配（原有逻辑）
        println!("{} 正在删除: {}", "→".blue(), virtual_path);
        container.remove_file(virtual_path)?;
        println!("{} 已删除: {}", "✓".green(), virtual_path);
    }

    Ok(())
}
