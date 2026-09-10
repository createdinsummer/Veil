//! # keys —— 非对称密钥对的生成，与密文私钥的加密/解密
//!
//! 本模块负责 Veil 加密模型里「密钥」这一层（对应 spec §4）：
//!
//! - 容器随机生成一对 **x25519 非对称密钥** `key_pair`（含私钥、可用 `.to_public()`
//!   导出公钥 `pub_key`）：公钥加密、`key_pair` 解密；
//! - 用户只记一个**密码**，密码经 Argon2id 派生后把 `key_pair` 的私钥**加密**成
//!   `cip_pri_key`（密文私钥），存进 Header（[`encrypt_pri_key`]）；
//! - 打开容器时用密码把 `cip_pri_key` **解密**，取回 `key_pair`（[`decrypt_pri_key`]）。
//!
//! 设计要点：Argon2id（昂贵 KDF）**每个容器一生只在加密/解密密文私钥时各跑一次**，
//! 之后开每个文件都是廉价的非对称解密。私钥与密码全程不打印、不落盘。

use age::secrecy::{ExposeSecret, SecretString};
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    ChaCha20Poly1305, Nonce,
};
use std::io::{Read, Write};
use zeroize::Zeroize;

use crate::error::{Result, VeilError};
use crate::kdf::runtime_params;

/// 用「用户密码 `passphrase`」把 `key_pair` 的私钥加密成密文私钥 `cip_pri_key`
/// （即写进 Header 的那串字节）。
///
/// # 参数
/// - `key_pair`: 容器的非对称密钥（x25519，含私钥、可导出公钥）。用 `&` 只借用、
///   不夺走所有权，调用方之后还能继续用它（比如导出公钥去加密文件）。
/// - `passphrase`: 用户输入的密码，用来加密 `key_pair` 的私钥。
///   用 `SecretString`（非 `String`）承载，避免被误打进日志；
///   这里按值传入，因为下面要把它移交给加密器。
///
/// # 返回
/// - `Ok(Vec<u8>)`:    密文私钥 `cip_pri_key`（可直接写进容器 Header）。
/// - `Err(VeilError)`: 加密过程中的错误。
///
/// 注意：Argon2id（昂贵 KDF）只在这里发生，整个容器一生只跑这一次这类操作。
pub fn encrypt_pri_key(
    key_pair: &age::x25519::Identity,
    passphrase: SecretString,
) -> Result<Vec<u8>> {
    // 1. 提取私钥字符串
    let pri_key_str: SecretString = key_pair.to_string();

    // 2. 生成随机盐值
    let mut salt = [0u8; 16];
    getrandom::getrandom(&mut salt)
        .map_err(|e| VeilError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

    // 3. Argon2id 派生密钥
    let params = runtime_params();
    let argon2_params = Params::new(
        params.memory_kb,
        params.iterations,
        params.parallelism,
        Some(32),
    )
    .map_err(|e| VeilError::Format(format!("Argon2 参数无效: {}", e)))?;

    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon2_params);

    let mut derived_key = [0u8; 32];
    argon2
        .hash_password_into(
            passphrase.expose_secret().as_bytes(),
            &salt,
            &mut derived_key,
        )
        .map_err(|e| VeilError::Format(format!("Argon2 派生失败: {}", e)))?;

    // 4. 使用派生密钥加密私钥（ChaCha20-Poly1305）
    let cipher = ChaCha20Poly1305::new(&derived_key.into());
    let nonce_array = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let nonce = Nonce::from(nonce_array);

    let ciphertext = cipher
        .encrypt(&nonce, pri_key_str.expose_secret().as_bytes())
        .map_err(|e| VeilError::Format(format!("ChaCha20-Poly1305 加密失败: {}", e)))?;

    // 5. 组装输出：salt(16) || nonce(12) || ciphertext_with_tag
    let mut output = Vec::with_capacity(16 + 12 + ciphertext.len());
    output.extend_from_slice(&salt);
    output.extend_from_slice(nonce.as_slice());
    output.extend_from_slice(&ciphertext);

    // 6. 清理敏感数据
    derived_key.zeroize();

    Ok(output)
}

/// 用「用户密码 `passphrase`」解密密文私钥 `cip_pri_key`，还原出非对称密钥 `key_pair`。
///
/// # 参数
/// - `cip_pri_key`: 之前 `encrypt_pri_key` 产出的密文私钥字节。
///   用 `&[u8]` 切片只读借用，不关心调用方是 Vec 还是数组，通用且零拷贝。
/// - `passphrase`:  用户输入的密码，用于解密 `cip_pri_key`。
///
/// # 返回
/// - `Ok(Identity)`:   密码正确时还原出的非对称密钥 `key_pair`。
/// - `Err(VeilError)`: 密码错误或数据被篡改。
pub fn decrypt_pri_key(
    cip_pri_key: &[u8],
    passphrase: SecretString,
) -> Result<age::x25519::Identity> {
    // 1. 解析格式：salt(16) || nonce(12) || ciphertext_with_tag
    if cip_pri_key.len() < 28 {
        return Err(VeilError::Format("密文数据过短".into()));
    }

    let salt = &cip_pri_key[0..16];
    let nonce_bytes = &cip_pri_key[16..28];
    let ciphertext = &cip_pri_key[28..];

    // 2. Argon2id 派生密钥
    let params = runtime_params();
    let argon2_params = Params::new(
        params.memory_kb,
        params.iterations,
        params.parallelism,
        Some(32),
    )
    .map_err(|e| VeilError::Format(format!("Argon2 参数无效: {}", e)))?;

    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon2_params);

    let mut derived_key = [0u8; 32];
    argon2
        .hash_password_into(
            passphrase.expose_secret().as_bytes(),
            salt,
            &mut derived_key,
        )
        .map_err(|e| VeilError::Format(format!("Argon2 派生失败: {}", e)))?;

    // 3. 解密私钥
    let cipher = ChaCha20Poly1305::new(&derived_key.into());
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher.decrypt(nonce, ciphertext).map_err(|e| {
        // ChaCha20Poly1305 解密失败通常是密码错误
        VeilError::Decrypt(age::DecryptError::from(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("密码错误或数据已损坏: {}", e),
        )))
    })?;

    // 4. 解析私钥字符串
    let mut pri_key_str = String::from_utf8(plaintext)
        .map_err(|_| VeilError::Format("私钥不是有效的 UTF-8".into()))?;

    let key_pair = pri_key_str
        .parse::<age::x25519::Identity>()
        .map_err(|e| VeilError::Format(format!("私钥解析失败: {}", e)))?;

    // 5. 清理敏感数据
    derived_key.zeroize();
    pri_key_str.zeroize();

    Ok(key_pair)
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
            "② Argon2id 加密私钥 → cip_pri_key: {} 字节，前 16 字节（盐值） = {:02x?}",
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
        println!(
            "明文 {} 字节 → 密文 {} 字节",
            plaintext.len(),
            ciphertext.len()
        );

        // 正确密钥能原样解回
        let back = decrypt_bytes(&key_pair, &ciphertext).unwrap();
        assert_eq!(back, plaintext);

        // 换一对密钥（等于"没有正确私钥"）→ 必须失败
        let other_key_pair = age::x25519::Identity::generate();
        assert!(decrypt_bytes(&other_key_pair, &ciphertext).is_err());
    }
}
