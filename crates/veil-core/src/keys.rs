//! # keys —— 容器身份的生成与密码锁定/解锁
//!
//! 本模块负责 Veil 加密模型里「钥匙」这一层（对应 spec §4）：
//!
//! - 容器有一把随机生成的 **x25519 身份**（含私钥），真正用来加解密文件内容；
//! - 用户只记一个**密码**，密码经 scrypt 派生后把这把身份**锁**起来，
//!   得到 Header 里的 `locked_identity`（[`lock_identity`]）；
//! - 打开容器时用密码把它**解**开，取回身份（[`unlock_identity`]）。
//!
//! 设计要点：scrypt（昂贵 KDF）**每个容器一生只在锁定/解锁时各跑一次**，
//! 之后开每个文件都是廉价的对称解密。私钥与密码全程不打印、不落盘。

use age::secrecy::{ExposeSecret, SecretString};
use std::io::{Read, Write};

use crate::error::{Result, VeilError};
/// 用「用户密码:passphrase」把容器身份的私钥(id)加密成一串字节（即 Header 里的 locked_identity）。
/// # 参数
/// - `id`:        容器的身份（含 x25519 私钥）。用 `&` 只借用、不夺走所有权，
///                调用方之后还能继续用这个 id（比如拿它的公钥去加密文件）。
/// - `passphrase`:用户输入的密码。
///                用passphrase加密id
///                用 `SecretString`（非 `String`）承载，避免被误打进日志；
///                这里按值传入，因为下面要把它移交给 age 的加密器。
///
/// # 返回
/// - `Ok(Vec<u8>)`:加密后的字节（可直接写进容器 Header）。
/// - `Err(VeilError)`:加密过程中的 I/O 或 age 错误（经 `?` 自动转换）。
///
/// 注意:scrypt（昂贵 KDF）只在这里发生，整个容器一生只跑这一次这类操作。
pub fn lock_identity(id: &age::x25519::Identity, passphrase: SecretString) -> Result<Vec<u8>> {
    // 取私钥的字符串形式 "AGE-SECRET-KEY-..."；仍用 SecretString 包着，切勿打印
    let sk :SecretString = id.to_string();

    // 用密码构造加密器：with_user_passphrase 内部即用 scrypt 把密码派生成加密密钥
    // passphrase 的所有权在此被 age 拿走（之后本函数不能再用它）
    let encryptor = age::Encryptor::with_user_passphrase(passphrase);

    // 准备一个内存缓冲区当输出目标（Vec<u8> 实现了 Write）
    let mut out = Vec::new();

    // wrap_output(w)：把 w 包成「加密写入器」——往里写明文，它自动加密后写进 w(=out)
    // &mut out：把 out 可变借给加密器，加密器只管写，所有权仍在 out
    // 末尾的 ? ：若返回 io::Error，经 #[from] 自动变成 VeilError::Io 并提前返回
    let mut  writer = encryptor.wrap_output(&mut out)?;

    // 把私钥明文喂进加密流。expose_secret() 显式取出 &str，再 as_bytes() 转 &[u8]
    writer.write_all(sk.expose_secret().as_bytes())?;

    // finish() 必须调用：写出最后一段密文 + 认证标签；漏掉则密文残缺、日后必解密失败
    writer.finish()?;

    // 加密完成，把缓冲区所有权交还调用方
    Ok(out)
}

/// 用「用户密码:passphrase」解开 locked_identity，还原出能解密容器内容的身份私钥。
/// # 参数
/// - `locked_identity`:    之前 `lock_identity` 产出的密文字节。
///                用 `&[u8]` 切片只读借用，
///                不关心调用方是 Vec 还是数组，通用且零拷贝。
/// - `passphrase`:用户输入的密码，用于解密locked_identity
///
///
/// # 返回
/// - `Ok(Identity)`:密码正确时还原出的 x25519 私钥身份。
/// - `Err(VeilError)`:密码错误或数据被篡改时，age 返回 DecryptError（不会吐垃圾）。
pub fn unlock_identity(locked_identity: &[u8], passphrase: SecretString) -> Result<age::x25519::Identity> {
    // 解密侧需要与「加密侧密码」配对的 scrypt 身份；passphrase 所有权在此交给它
    let identity = age::scrypt::Identity::new(passphrase);

    // Decryptor::new 读取 age 头部。&[u8] 自身实现 Read，可直接当输入源
    // ? ：密文头损坏等 → DecryptError → 经 #[from] 变 VeilError::Decrypt
    let decryptor = age::Decryptor::new(locked_identity)?;

    // decrypt 传入「一串候选身份」；这里只有一个，用 iter::once 包成迭代器
    // &identity as &dyn age::Identity：把具体类型转成 trait object 引用（age 要 dyn）
    // 返回可读的 StreamReader；密码错也在这一步报错
    let mut reader = decryptor.decrypt(std::iter::once(&identity as &dyn age::Identity))?;

    // 把解密出的明文（私钥字符串）读进 sk
    let mut sk = String::new();
    reader.read_to_string(&mut sk)?;

    // 把字符串解析回 x25519::Identity。
    // parse 的错误类型是 &str，不是 VeilError，所以用 map_err 手动转成 VeilError::Format
    sk.parse::<age::x25519::Identity>()
        .map_err(|e| VeilError::Format(format!("私钥解析失败: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_lock_unlock_roundtrip() {
        // 生成一个新身份，并记下它的公钥字符串（公钥非敏感），供事后比对
        let id = age::x25519::Identity::generate();
        // 记下公钥，用于后续验证
        let pubkey = format!("{}", id.to_public());
        println!("① 随机生成身份，公钥 = {pubkey}"); // 只打印公钥；私钥绝不打印

        // SecretString 不能 clone，需要用两次密码就各建一个
        let locked_identity =
            lock_identity(&id, SecretString::from("correct horse".to_owned())).unwrap();
        println!(
            "② 密码锁定 → locked_identity: {} 字节，前 16 字节 = {:02x?}",
            locked_identity.len(),
            &locked_identity[..16]
        );

        // 正确密码 → 还原出的身份，其公钥应与原来完全一致（证明往返无损）
        let restored =
            unlock_identity(&locked_identity, SecretString::from("correct horse".to_owned()))
                .unwrap();
        println!("③ 正确密码解锁成功，公钥 = {}", restored.to_public());
        assert_eq!(format!("{}", restored.to_public()), pubkey);

        // 错误密码 → 必须失败；只打印 Err（绝不打印 Ok 里的私钥）
        let wrong = unlock_identity(&locked_identity, SecretString::from("wrong".to_owned()));
        match &wrong {
            Ok(_) => println!("④ ❌ 不该发生：错误密码竟然解开了！"),
            Err(e) => println!("④ 错误密码被正确拒绝：{e}"),
        }
        assert!(wrong.is_err());
    }
}
