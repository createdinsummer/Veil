//! # keys —— 非对称密钥对的生成，与密文私钥的加密/解密
//!
//! 本模块负责 Veil 加密模型里「密钥」这一层（对应 spec §4）：
//!
//! - 容器随机生成一对 **x25519 非对称密钥** `key_pair`（含私钥、可用 `.to_public()`
//!   导出公钥 `pub_key`）：公钥加密、`key_pair` 解密；
//! - 用户只记一个**密码**，密码经 scrypt 派生后把 `key_pair` 的私钥**加密**成
//!   `cip_pri_key`（密文私钥），存进 Header（[`encrypt_pri_key`]）；
//! - 打开容器时用密码把 `cip_pri_key` **解密**，取回 `key_pair`（[`decrypt_pri_key`]）。
//!
//! 设计要点：scrypt（昂贵 KDF）**每个容器一生只在加密/解密密文私钥时各跑一次**，
//! 之后开每个文件都是廉价的非对称解密。私钥与密码全程不打印、不落盘。

use age::secrecy::{ExposeSecret, SecretString};
use std::io::{Read, Write};

use crate::error::{Result, VeilError};

/// 用「用户密码 `passphrase`」把 `key_pair` 的私钥加密成密文私钥 `cip_pri_key`
/// （即写进 Header 的那串字节）。
///
/// # 参数
/// - `key_pair`:   容器的非对称密钥（x25519，含私钥、可导出公钥）。用 `&` 只借用、
///                 不夺走所有权，调用方之后还能继续用它（比如导出公钥去加密文件）。
/// - `passphrase`: 用户输入的密码，用来加密 `key_pair` 的私钥。
///                 用 `SecretString`（非 `String`）承载，避免被误打进日志；
///                 这里按值传入，因为下面要把它移交给 age 的加密器。
///
/// # 返回
/// - `Ok(Vec<u8>)`:    密文私钥 `cip_pri_key`（可直接写进容器 Header）。
/// - `Err(VeilError)`: 加密过程中的 I/O 或 age 错误（经 `?` 自动转换）。
///
/// 注意：scrypt（昂贵 KDF）只在这里发生，整个容器一生只跑这一次这类操作。
pub fn encrypt_pri_key(key_pair: &age::x25519::Identity, passphrase: SecretString) -> Result<Vec<u8>> {
    // 取私钥的字符串形式 "AGE-SECRET-KEY-..."；仍用 SecretString 包着，切勿打印
    let pri_key_str: SecretString = key_pair.to_string();

    // 用密码构造加密器：with_user_passphrase 内部即用 scrypt 把密码派生成加密密钥
    // passphrase 的所有权在此被 age 拿走（之后本函数不能再用它）
    let encryptor = age::Encryptor::with_user_passphrase(passphrase);

    // 准备一个内存缓冲区当输出目标（Vec<u8> 实现了 Write）
    let mut out = Vec::new();

    // wrap_output(w)：把 w 包成「加密写入器」——往里写明文，它自动加密后写进 w(=out)
    // &mut out：把 out 可变借给加密器，加密器只管写，所有权仍在 out
    // 末尾的 ? ：若返回 io::Error，经 #[from] 自动变成 VeilError::Io 并提前返回
    let mut writer = encryptor.wrap_output(&mut out)?;

    // 把私钥明文喂进加密流。expose_secret() 显式取出 &str，再 as_bytes() 转 &[u8]
    writer.write_all(pri_key_str.expose_secret().as_bytes())?;

    // finish() 必须调用：写出最后一段密文 + 认证标签；漏掉则密文残缺、日后必解密失败
    writer.finish()?;

    // 加密完成，把密文私钥交还调用方
    Ok(out)
}

/// 用「用户密码 `passphrase`」解密密文私钥 `cip_pri_key`，还原出非对称密钥 `key_pair`。
///
/// # 参数
/// - `cip_pri_key`: 之前 `encrypt_pri_key` 产出的密文私钥字节。
///                  用 `&[u8]` 切片只读借用，不关心调用方是 Vec 还是数组，通用且零拷贝。
/// - `passphrase`:  用户输入的密码，用于解密 `cip_pri_key`。
///
/// # 返回
/// - `Ok(Identity)`:   密码正确时还原出的非对称密钥 `key_pair`。
/// - `Err(VeilError)`: 密码错误或数据被篡改时，age 返回 DecryptError（不会吐垃圾）。
pub fn decrypt_pri_key(cip_pri_key: &[u8], passphrase: SecretString) -> Result<age::x25519::Identity> {
    // 密码派生的解密方（age::scrypt::Identity）：与加密时用的 scrypt 密码配对
    // 注意它不是我们的 key_pair，只是「用密码解密」这一步的解密器；passphrase 在此交给它
    let scrypt_identity = age::scrypt::Identity::new(passphrase);

    // Decryptor::new 读取 age 头部。&[u8] 自身实现 Read，可直接当输入源
    // ? ：密文头损坏等 → DecryptError → 经 #[from] 变 VeilError::Decrypt
    let decryptor = age::Decryptor::new(cip_pri_key)?;

    // decrypt 传入「一串候选解密方」（age::Identity trait，即能解密的私钥/密码等）；
    // 这里只有一个，用 iter::once 包成迭代器
    // as &dyn age::Identity：把具体类型转成 trait object 引用（age 要 dyn）
    // 返回可读的 StreamReader；密码错也在这一步报错
    let mut reader = decryptor.decrypt(std::iter::once(&scrypt_identity as &dyn age::Identity))?;

    // 把解密出的明文（私钥字符串）读进 pri_key_str
    let mut pri_key_str = String::new();
    reader.read_to_string(&mut pri_key_str)?;

    // 把字符串解析回 x25519 私钥。
    // parse 的错误类型是 &str，不是 VeilError，所以用 map_err 手动转成 VeilError::Format
    pri_key_str
        .parse::<age::x25519::Identity>()
        .map_err(|e| VeilError::Format(format!("私钥解析失败: {e}")))
}

/// 用公钥 `pub_key` 把一段明文加密成 age 密文字节。
/// 每个文件的 blob、以及整棵目录树 Index，都用它加密。
///
/// # 参数
/// - `pub_key`:   容器公钥（由 `key_pair.to_public()` 得来）
/// - `plaintext`: 待加密的明文字节
/// # 返回
/// - `Ok(Vec<u8>)`: 完整的 age 密文
pub fn encrypt_bytes(pub_key: &age::x25519::Recipient, plaintext: &[u8]) -> Result<Vec<u8>> {
    // with_recipients 要「一串实现了 age::Recipient 的东西」；我们只有一个公钥
    // as &dyn age::Recipient：把具体类型转成 trait object 引用
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(pub_key as &dyn age::Recipient))?;

    let mut out = Vec::new();
    let mut writer = encryptor.wrap_output(&mut out)?;
    writer.write_all(plaintext)?;
    writer.finish()?; // 必须调用，否则密文残缺
    Ok(out)
}

/// 用非对称密钥 `key_pair` 的私钥把 age 密文解密回明文字节。
///
/// # 参数
/// - `key_pair`:   容器非对称密钥（解密密文私钥后拿到，用其私钥解密）
/// - `ciphertext`: 之前 `encrypt_bytes` 产出的密文
/// # 返回
/// - `Ok(Vec<u8>)`: 还原出的明文；密文被篡改 / 密钥不对 → `Err`
pub fn decrypt_bytes(key_pair: &age::x25519::Identity, ciphertext: &[u8]) -> Result<Vec<u8>> {
    let decryptor = age::Decryptor::new(ciphertext)?;
    let mut reader = decryptor.decrypt(std::iter::once(key_pair as &dyn age::Identity))?;

    let mut out = Vec::new();
    reader.read_to_end(&mut out)?; // 读全部字节（不是字符串，所以用 read_to_end）
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pri_key_encrypt_decrypt_roundtrip() {
        // 生成一对非对称密钥，并记下公钥字符串（公钥非敏感），供事后比对
        let key_pair = age::x25519::Identity::generate();
        let pub_key = format!("{}", key_pair.to_public());
        println!("① 生成非对称密钥对，公钥 = {pub_key}"); // 只打印公钥；私钥绝不打印

        // SecretString 不能 clone，需要用两次密码就各建一个
        let cip_pri_key =
            encrypt_pri_key(&key_pair, SecretString::from("correct horse".to_owned())).unwrap();
        println!(
            "② 密码加密私钥 → cip_pri_key: {} 字节，前 16 字节 = {:02x?}",
            cip_pri_key.len(),
            &cip_pri_key[..16]
        );

        // 正确密码 → 还原出的私钥，其公钥应与原来完全一致（证明往返无损）
        let restored =
            decrypt_pri_key(&cip_pri_key, SecretString::from("correct horse".to_owned())).unwrap();
        println!("③ 正确密码解密成功，公钥 = {}", restored.to_public());
        assert_eq!(format!("{}", restored.to_public()), pub_key);

        // 错误密码 → 必须失败；只打印 Err（绝不打印 Ok 里的私钥）
        let wrong = decrypt_pri_key(&cip_pri_key, SecretString::from("wrong".to_owned()));
        match &wrong {
            Ok(_) => println!("④ ❌ 不该发生：错误密码竟然解开了！"),
            Err(e) => println!("④ 错误密码被正确拒绝：{e}"),
        }
        assert!(wrong.is_err());
    }

    #[test]
    fn encrypt_decrypt_bytes_roundtrip() {
        let key_pair = age::x25519::Identity::generate();
        let pub_key = key_pair.to_public();

        let plaintext = b"hello veil \xff\x00\x01"; // 含非文本字节，证明按二进制处理
        let ciphertext = encrypt_bytes(&pub_key, plaintext).unwrap();
        println!("明文 {} 字节 → 密文 {} 字节", plaintext.len(), ciphertext.len());

        // 正确密钥能原样解回
        let back = decrypt_bytes(&key_pair, &ciphertext).unwrap();
        assert_eq!(back, plaintext);

        // 换一对密钥（等于"没有正确私钥"）→ 必须失败
        let other_key_pair = age::x25519::Identity::generate();
        assert!(decrypt_bytes(&other_key_pair, &ciphertext).is_err());
    }
}
