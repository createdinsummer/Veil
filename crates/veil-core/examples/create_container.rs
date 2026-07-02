use age::secrecy::SecretString;
use veil_core::container::Container;

fn main() -> veil_core::error::Result<()> {
    let path = "demo.veil";
    // SecretString 不能 clone，用个小闭包每次现造一个
    let pass = || SecretString::from("correct horse".to_owned());

    // 创建
    Container::create(path, pass())?;
    let size = std::fs::metadata(path)?.len();
    println!("✅ 创建容器 {path}，大小 {size} 字节");

    // 用正确密码重新打开
    let container = Container::open(path, pass())?;
    println!("✅ 重新打开成功，目录树条目数 = {}", container.nodes().len());

    // 用错误密码打开 → 必须失败
    match Container::open(path, SecretString::from("wrong".to_owned())) {
        Ok(_) => println!("❌ 不该发生：错误密码竟然打开了！"),
        Err(e) => println!("✅ 错误密码被正确拒绝：{e}"),
    }
    Ok(())
}