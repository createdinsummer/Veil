//! 认证加密原语封装，供上层事务流程调用。

use crate::error::VeilError;
use chacha20poly1305::{
    ChaCha20Poly1305,
    aead::{Aead, KeyInit, generic_array::GenericArray},
};

/// 使用 ChaCha20-Poly1305 加密字节。
pub fn encrypt_chacha20poly1305(
    key: &[u8; 32],
    plaintext: &[u8],
    nonce: &[u8; 12],
) -> Result<Vec<u8>, VeilError> {
    ChaCha20Poly1305::new(GenericArray::from_slice(key))
        .encrypt(GenericArray::from_slice(nonce), plaintext)
        .map_err(|error| VeilError::EncryptionError(format!("加密失败: {error}")))
}

/// 解密并验证 ChaCha20-Poly1305 密文。
pub fn decrypt_chacha20poly1305(
    key: &[u8; 32],
    ciphertext: &[u8],
    nonce: &[u8; 12],
) -> Result<Vec<u8>, VeilError> {
    ChaCha20Poly1305::new(GenericArray::from_slice(key))
        .decrypt(GenericArray::from_slice(nonce), ciphertext)
        .map_err(|error| {
            VeilError::DecryptionError(format!("解密失败（密码错误或数据损坏）: {error}"))
        })
}
