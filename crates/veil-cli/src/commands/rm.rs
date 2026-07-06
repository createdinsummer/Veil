use anyhow::Result;
use colored::Colorize;
use veil_core::container::Container;

/// 从容器删除文件。
///
/// 按虚拟路径删除容器内的文件，并更新索引。被删除文件的加密数据仍留在容器中
/// 成为死空间（后续可通过 compaction 回收）。
///
/// # 参数
/// - `container_path`: 容器文件路径
/// - `virtual_path`: 要删除的虚拟路径（容器内路径）
/// - `password`: 容器密码（`None` 则交互式输入）
///
/// # 返回
/// - `Ok(())`: 文件已从索引中移除
/// - `Err(anyhow::Error)`: 失败（密码错误、文件不存在或写入失败）
///
/// # 示例
/// ```bash
/// # 位置参数方式
/// veil rm photos.veil archive/old.jpg mypass
///
/// # 选项方式
/// veil rm photos.veil archive/old.jpg -p mypass
/// ```
pub fn run(container_path: &str, virtual_path: &str, password: Option<String>) -> Result<()> {
    let password = super::prompt_password("请输入容器密码: ", password)?;

    println!("{}", "正在打开容器...".cyan());
    let mut container = Container::open(container_path, password)?;

    println!("{} 正在删除: {}", "→".blue(), virtual_path);
    container.remove_file(virtual_path)?;

    println!("{} 已删除: {}", "✓".green(), virtual_path);
    Ok(())
}
