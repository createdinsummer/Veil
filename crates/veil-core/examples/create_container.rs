use age::secrecy::SecretString;
use veil_core::container::Container;

fn main() -> veil_core::error::Result<()> {
    let path = "demo.veil";
    let pass = || SecretString::from("correct horse".to_owned());

    // 创建 → 加两个文件
    let mut container = Container::create(path, pass())?;
    container.add_file("hello.txt", b"Hello, Veil!")?;
    container.add_file("photos/note.md", b"# secret note\nline2")?;
    println!(
        "✅ 加了 {} 个文件，文件大小 {} 字节",
        container.nodes().len(),
        std::fs::metadata(path)?.len()
    );

    // 重新打开 → 列出 → 读回并校验
    let reopened = Container::open(path, pass())?;
    println!("✅ 重新打开，目录：");
    for node in reopened.nodes() {
        println!("   - {} ({} 字节)", node.path, node.size);
    }

    let data = reopened.read_file("hello.txt")?;
    println!("✅ 读回 hello.txt = {:?}", String::from_utf8_lossy(&data));
    assert_eq!(data, b"Hello, Veil!");

    Ok(())
}