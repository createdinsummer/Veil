use age::secrecy::SecretString;
use veil_core::container::Container;

fn main() -> veil_core::error::Result<()> {
    let path = "demo.veil";
    let pass = || SecretString::from("correct horse".to_owned());

    // 创建 → 加几个文件
    let mut container = Container::create(path, pass())?;
    container.add_file("hello.txt", b"Hello, Veil!")?;
    container.add_file("photos/2024/a.jpg", b"fake jpg bytes")?;
    container.add_file("photos/2024/b.jpg", b"another jpg")?;
    container.add_file("photos/note.md", b"# secret note\nline2")?;
    println!(
        "✅ 加了 {} 个文件，文件大小 {} 字节",
        container.nodes().len(),
        std::fs::metadata(path)?.len()
    );

    // 重新打开 → 树状展示目录结构
    let mut reopened = Container::open(path, pass())?;
    println!("✅ 重新打开，目录结构：");
    print!("{}", reopened.tree_view());

    // 删除一个文件 → 再看目录结构
    reopened.remove_file("photos/2024/a.jpg")?;
    println!("✅ 删除 photos/2024/a.jpg 后：");
    print!("{}", reopened.tree_view());

    let data = reopened.read_file("hello.txt")?;
    println!("✅ 读回 hello.txt = {:?}", String::from_utf8_lossy(&data));
    assert_eq!(data, b"Hello, Veil!");

    // 按路径解密单个文件到指定位置
    reopened.extract_file("photos/note.md", "out/just-note.md")?;
    println!("✅ 已解密 photos/note.md → out/just-note.md");

    // 导出整个容器到 out/ 目录
    let out_dir = "out";
    reopened.extract_all(out_dir)?;
    println!("✅ 已导出到 {out_dir}/ ：");
    for node in reopened.nodes() {
        let p = std::path::Path::new(out_dir).join(&node.path);
        println!("   - {}", p.display());
    }

    Ok(())
}