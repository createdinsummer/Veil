//! 容器 x25519 密钥模型与私钥封装。
//!
//! 本模块负责 Veil 加密模型中的密钥层：
//!
//! - 容器随机生成一对 **x25519 非对称密钥** `key_pair`（含私钥、可用 `.to_public()`
//!   导出公钥 `pub_key`）：公钥加密、`key_pair` 解密；
//! - 用户只记一个**密码**，密码经 Argon2id 派生后把 `key_pair` 的私钥**加密**成
//!   `cip_pri_key`（密文私钥），存进 Header（[`encrypt_pri_key`]）；
//! - 打开容器时用密码把 `cip_pri_key` **解密**，取回 `key_pair`（[`decrypt_pri_key`]）。
//!
//! 设计要点：Argon2id 只在封装或解封容器私钥时运行，文件内容继续复用已解出的
//! x25519 身份，避免每次读写文件都承担昂贵的密码派生。私钥与密码不写入明文日志。

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

/// 使用用户密码保护容器的 x25519 私钥。
///
/// 方法生成随机盐，经 Argon2id 派生 32 字节密钥，再用 ChaCha20-Poly1305 加密私钥
/// 字符串。输出布局为 `salt(16) || nonce(12) || ciphertext_with_tag`，可直接写入
/// 容器 Header。
///
/// # 参数
/// - `key_pair`：容器的 x25519 身份；仅借用，调用方之后仍可使用。
/// - `passphrase`：用于保护私钥的用户密码，使用 [`SecretString`] 避免默认 Debug
///   输出泄露内容。
///
/// # 返回
/// - `Ok(Vec<u8>)`：可直接写入容器 Header 的受保护私钥。
/// - `Err(VeilError)`：随机源、Argon2 参数或派生、ChaCha 加密失败。
pub fn encrypt_pri_key(
    key_pair: &age::x25519::Identity,
    passphrase: SecretString,
) -> Result<Vec<u8>> {
    // 私钥先用 SecretString 承载，避免中间字符串进入普通 Debug 输出。
    let pri_key_str: SecretString = key_pair.to_string();

    // 每次封装使用独立盐值，避免相同密码产生可复用的派生密钥。
    let mut salt = [0u8; 16];
    getrandom::getrandom(&mut salt)
        .map_err(|e| VeilError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

    // 密码到加密密钥只经过这里一次，后续文件读写复用解出的 x25519 身份。
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

    let cipher = ChaCha20Poly1305::new(&derived_key.into());
    // 盐值负责派生密钥，nonce 负责本次 ChaCha20-Poly1305 加密，两者都随密文保存。
    let nonce_array = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    let nonce = Nonce::from(nonce_array);

    let ciphertext = cipher
        .encrypt(&nonce, pri_key_str.expose_secret().as_bytes())
        .map_err(|e| VeilError::Format(format!("ChaCha20-Poly1305 加密失败: {}", e)))?;

    // 输出同时携带解密所需的盐值和 nonce。
    let mut output = Vec::with_capacity(16 + 12 + ciphertext.len());
    output.extend_from_slice(&salt);
    output.extend_from_slice(nonce.as_slice());
    output.extend_from_slice(&ciphertext);

    // 派生密钥只用于本次封装，返回前主动清零。
    derived_key.zeroize();

    Ok(output)
}

/// 使用用户密码解封容器私钥。
///
/// # 参数
/// - `cip_pri_key`：[`encrypt_pri_key`] 生成的受保护私钥字节。
/// - `passphrase`：用于解封私钥的用户密码。
///
/// # 返回
/// - `Ok(Identity)`：密码和认证标签均正确时还原出的 x25519 身份。
/// - `Err(VeilError)`：输入过短、参数无效、密码错误、密文损坏或私钥文本无法解析。
pub fn decrypt_pri_key(
    cip_pri_key: &[u8],
    passphrase: SecretString,
) -> Result<age::x25519::Identity> {
    if cip_pri_key.len() < 28 {
        return Err(VeilError::Format("密文数据过短".into()));
    }

    // 与加密端约定一致的布局：salt || nonce || ciphertext_with_tag。
    let salt = &cip_pri_key[0..16];
    let nonce_bytes = &cip_pri_key[16..28];
    let ciphertext = &cip_pri_key[28..];

    // 使用头部中保存的盐值重新派生同一把对称密钥。
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

    let cipher = ChaCha20Poly1305::new(&derived_key.into());
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher.decrypt(nonce, ciphertext).map_err(|e| {
        // 认证失败无法区分密码错误和密文损坏，统一映射为 Decrypt。
        VeilError::Decrypt(age::DecryptError::from(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("密码错误或数据已损坏: {}", e),
        )))
    })?;

    // 解密成功只证明认证标签有效；私钥文本仍需再次解析为 x25519 身份。
    let mut pri_key_str = String::from_utf8(plaintext)
        .map_err(|_| VeilError::Format("私钥不是有效的 UTF-8".into()))?;

    let key_pair = pri_key_str
        .parse::<age::x25519::Identity>()
        .map_err(|e| VeilError::Format(format!("私钥解析失败: {}", e)))?;

    // 明文私钥字符串和派生密钥都不在调用结束后继续保留。
    derived_key.zeroize();
    pri_key_str.zeroize();

    Ok(key_pair)
}

/// 使用容器公钥执行 age x25519 加密。
/// 每个文件的 blob、以及整棵目录树 Index，都用它加密。
///
/// # 参数
/// - `pub_key`：容器公钥。
/// - `plaintext`：待加密的明文字节。
/// # 返回
/// - `Ok(Vec<u8>)`：包含 age 封装信息的完整密文。
///
/// # 错误
/// age 加密器初始化、流式写入或完成密文写出失败时返回错误。
pub fn encrypt_bytes(pub_key: &age::x25519::Recipient, plaintext: &[u8]) -> Result<Vec<u8>> {
    // age 加密器接收 trait object 列表；当前容器只使用一个 x25519 公钥。
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(pub_key as &dyn age::Recipient))?;

    // wrap_output 提供写入接口，finish 负责写完 age 流的收尾数据。
    let mut out = Vec::new();
    let mut writer = encryptor.wrap_output(&mut out)?;
    writer.write_all(plaintext)?;
    writer.finish()?;
    Ok(out)
}

/// 使用容器身份把 age 密文解密回明文字节。
///
/// # 参数
/// - `key_pair`：容器 x25519 身份。
/// - `ciphertext`：[`encrypt_bytes`] 生成的完整 age 密文。
/// # 返回
/// - `Ok(Vec<u8>)`：通过认证并还原出的明文。
/// - `Err(VeilError)`：密文格式无效、密钥不匹配、内容被篡改或读取失败。
pub fn decrypt_bytes(key_pair: &age::x25519::Identity, ciphertext: &[u8]) -> Result<Vec<u8>> {
    // Decryptor 先解析 age 信封，再用提供的身份尝试解封内部密钥。
    let decryptor = age::Decryptor::new(ciphertext)?;
    let mut reader = decryptor.decrypt(std::iter::once(key_pair as &dyn age::Identity))?;

    // blob 是任意二进制，因此不做 UTF-8 解释，直接读完整字节。
    let mut out = Vec::new();
    reader.read_to_end(&mut out)?;
    Ok(out)
}

/// 私钥封装和 age 密钥加解密往返测试。
#[cfg(test)]
mod tests {
    use super::*;

    /// 验证密码封装私钥后可还原相同公钥，错误密码会被拒绝。
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

    /// 验证 age 字节加解密保持二进制内容不变。
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
