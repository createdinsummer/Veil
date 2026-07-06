use anyhow::Result;
use colored::Colorize;
use std::path::Path;
use veil_core::container::Container;

pub fn run(container_path: &str, virtual_path: Option<&str>, output: &str, all: bool, password: Option<String>) -> Result<()> {
    let password = super::prompt_password("请输入容器密码: ", password)?;

    println!("{}", "正在打开容器...".cyan());
    let container = Container::open(container_path, password)?;

    if all {
        // 导出全部内容
        println!("{} 正在导出全部内容到: {}", "→".blue(), output);
        container.extract_all(output)?;
        println!("{} 导出完成", "✓".green());
    } else if let Some(path) = virtual_path {
        // 导出单个文件或目录
        let output_path = Path::new(output);

        // 判断是文件还是目录
        if let Some(_meta) = container.get_file(path) {
            // 是文件
            println!("{} 正在导出文件: {} -> {}", "→".blue(), path, output);
            container.extract_file(path, output_path)?;
            println!("{} 文件已导出: {}", "✓".green(), output);
        } else {
            // 尝试作为目录导出
            println!("{} 正在导出目录: {} -> {}", "→".blue(), path, output);
            container.extract_dir(path, output_path)?;
            println!("{} 目录已导出完成", "✓".green());
        }
    } else {
        anyhow::bail!("请指定要导出的路径（--path）或使用 --all 导出全部");
    }

    Ok(())
}
