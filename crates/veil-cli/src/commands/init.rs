use anyhow::Result;
use colored::Colorize;
use veil_core::container::Container;

/// CLI 版本（从 Cargo.toml 读取）
const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 创建新的加密容器。
///
/// 创建一个空的 `.veil` 容器文件，使用用户提供的密码加密私钥。
/// 容器创建后可以使用 `add` 命令添加文件。
///
/// # 参数
/// - `container_path`: 容器文件路径（如 "photos.veil"）
/// - `password`: 用户密码（`Option<String>`，`None` 则交互式输入）
///
/// # 返回
/// - `Ok(())`: 容器创建成功
/// - `Err(anyhow::Error)`: 创建失败（文件已存在、加密失败或写入失败）
///
/// # 示例
/// ```bash
/// # 位置参数方式
/// veil init photos.veil mypassword
///
/// # 选项方式
/// veil init photos.veil -p mypassword
///
/// # 交互式
/// veil init photos.veil
/// ```
pub fn run(container_path: &str, password: Option<String>) -> Result<()> {
    // 检查文件是否已存在
    if std::path::Path::new(container_path).exists() {
        anyhow::bail!("容器文件已存在: {}", container_path);
    }

    println!("{}", "创建新容器...".cyan());
    let password = super::prompt_new_password(password)?;

    Container::create(container_path, password, CLI_VERSION)?;

    println!("{} 容器创建成功: {}", "✓".green(), container_path);
    Ok(())
}
