//! 认证加密原语封装，供上层事务流程调用。
//!
//! 所属业务模块：veil-core 的加密底层。本模块是"单块字节加解密"的最小原语层，
//! 对外提供无附加认证数据参数的单段工具；file_ops 当前直接使用底层算法并传入分块 AAD，
//! 不是经由本封装实现文件流式读写，不能据算法名称相同就替换两种入口。
//!
//! 文件内核心清单：
//! - `encrypt_chacha20poly1305`：加密一段明文，产出密文 + 16 字节认证标签。
//! - `decrypt_chacha20poly1305`：先校验认证标签再解出明文。
//!
//! 依赖能力：
//! - `chacha20poly1305` 库：RustCrypto 的 AEAD（加密同时防篡改）实现；解密前必须
//!   通过认证标签校验，密文被改动或密钥错误都会立刻失败，不会产出错误明文。
//! - `crate::error::VeilError`：统一错误类型，本模块只产出加密/解密两类错误。
//!
//! 整体业务目标：为已具备密钥与 nonce 的调用方提供简洁一致的认证加解密结果与错误类型；
//! 密钥分配、nonce 唯一使用、持久化格式和业务身份校验仍由具体调用流程负责。

use crate::error::VeilError;
use chacha20poly1305::{
    ChaCha20Poly1305,
    aead::{Aead, KeyInit, generic_array::GenericArray},
};

/// 功能：把一段内存明文加密并附加认证标签，供需要单段认证加密的调用方使用。
/// 入参：key: &[u8; 32] 为对称密钥；plaintext: &[u8] 为待保护内容，可为空；nonce: &[u8; 12] 为本次一次性数。
/// 返回值：Ok(`Vec<u8>`) 为密文和末尾 16 字节标签，长度为明文长度加 16。
/// 异常：底层加密失败返回 EncryptionError；固定长度参数避免错误密钥或 nonce 长度进入此接口。
/// 注意事项：同一密钥下不得复用 nonce 加密不同内容；本封装没有 AAD 参数，使用底层默认空附加认证数据。
/// 不持有共享状态，可并发调用，但不自动清零调用方密钥、明文或临时副本，也不定义文件分块格式。
pub fn encrypt_chacha20poly1305(
    key: &[u8; 32],
    plaintext: &[u8],
    nonce: &[u8; 12],
) -> Result<Vec<u8>, VeilError> {
    ChaCha20Poly1305::new(GenericArray::from_slice(key))
        // 用 key 初始化加密器实例，密钥长度固定 32 字节
        .encrypt(GenericArray::from_slice(nonce), plaintext)
        // 用 nonce 和明文执行加密，产出密文与认证标签
        .map_err(|error| VeilError::EncryptionError(format!("加密失败: {error}")))
        // 底层错误统一转成 veil-core 的加密错误，便于上层按统一类型处理
}

/// 功能：解出一段经认证保护的内存内容，只向调用方返回通过标签检查的明文。
/// 入参：key: &[u8; 32] 为原加密密钥；ciphertext: &[u8] 为包含末尾标签的完整密文；nonce: &[u8; 12] 须与原加密一致。
/// 返回值：Ok(`Vec<u8>`) 为认证通过后的明文，长度为密文长度减 16。
/// 异常：错误密钥、nonce、损坏/截断标签或其他认证失败返回 DecryptionError，不返回本段不可信明文。
/// 注意事项：使用空 AAD，不能直接替代 file_ops 含块序号 AAD 的分块解密；不校验业务身份或完整文件长度。
/// 无共享可变状态，返回的明文及调用方密钥缓冲的清理由各自所有者负责。
pub fn decrypt_chacha20poly1305(
    key: &[u8; 32],
    ciphertext: &[u8],
    nonce: &[u8; 12],
) -> Result<Vec<u8>, VeilError> {
    ChaCha20Poly1305::new(GenericArray::from_slice(key))
        // 用同一密钥重建解密器实例
        .decrypt(GenericArray::from_slice(nonce), ciphertext)
        // 先验证认证标签再解出明文，认证失败直接返回错误
        .map_err(|error| {
            VeilError::DecryptionError(format!("解密失败（密码错误或数据损坏）: {error}"))
            // 统一转成解密错误；错误文案提示两种最常见原因：密钥不对或密文损坏
        })
}
