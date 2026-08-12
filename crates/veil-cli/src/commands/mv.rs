use anyhow::Result;
use colored::Colorize;
use veil_core::container::Container;

/// 移动或重命名容器内的文件。
///
/// 只修改索引中的虚拟路径，不移动或重写加密数据。如果扩展名改变，会重新识别 MIME 类型。
///
/// # 参数
/// - `container_path`: 容器文件路径
/// - `from`: 源虚拟路径（容器内）
/// - `to`: 目标虚拟路径（容器内）
/// - `password`: 容器密码（`None` 则交互式输入）
///
/// # 返回
/// - `Ok(())`: 文件已重命名并更新索引
/// - `Err(anyhow::Error)`: 失败（密码错误、源文件不存在、目标已存在或写入失败）
///
/// # 示例
/// ```bash
/// # 位置参数方式
/// veil mv photos.veil old.jpg archive/old.jpg mypass
///
/// # 选项方式（-i 和 -o 都是容器内路径）
/// veil mv photos.veil -i old.jpg -o archive/old.jpg -p mypass
/// ```
pub fn run(container_path: &str, from: &str, to: &str, password: Option<String>) -> Result<()> {
    let password = super::prompt_password(crate::i18n::t("prompt.container_password"), password)?;

    println!("{}", crate::i18n::t("opening_container").cyan());
    let mut container = Container::open(container_path, password)?;

    println!("{}", crate::i18n::t2("mv.moving", "from", from, "to", to).blue());
    container.rename_file(from, to)?;

    println!("{}", crate::i18n::t2("mv.moved", "from", from, "to", to).green());
    Ok(())
}
