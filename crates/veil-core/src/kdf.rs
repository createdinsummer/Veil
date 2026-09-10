//! # kdf —— 密钥派生函数（KDF）类型与参数
//!
//! 本模块定义 Veil 支持的 KDF 类型及其参数。

use crate::error::{Result, VeilError};

#[cfg(debug_assertions)]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(debug_assertions)]
static FAST_TEST_KDF: AtomicBool = AtomicBool::new(false);

/// KDF 类型标识
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KdfType {
    /// Argon2id
    Argon2id = 0x1,
}

impl KdfType {
    /// 从容器 flags 字段解析 KDF 类型
    pub fn from_flags(flags: u16) -> Result<Self> {
        match flags & 0x0F {
            0x1 => Ok(Self::Argon2id),
            n => Err(VeilError::Format(format!("未知的 KDF 类型: 0x{:x}", n))),
        }
    }

    /// 转换为 flags 字段值
    pub fn to_flags(self) -> u16 {
        self as u16
    }
}

/// Argon2id 参数配置
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Argon2Params {
    /// 内存消耗（KB）
    pub memory_kb: u32,
    /// 迭代次数
    pub iterations: u32,
    /// 并行度（线程数）
    pub parallelism: u32,
}

impl Argon2Params {
    /// 标准安全级别（OWASP 推荐）
    /// - 内存：256 MB
    /// - 时间：约 1.5 秒
    /// - 防御强度：良好
    pub const STANDARD: Self = Self {
        memory_kb: 256 * 1024, // 256 MB
        iterations: 3,
        parallelism: 4,
    };

    /// 高安全级别
    /// - 内存：512 MB
    /// - 时间：约 3 秒
    /// - 防御强度：很好
    pub const HIGH: Self = Self {
        memory_kb: 512 * 1024, // 512 MB
        iterations: 4,
        parallelism: 4,
    };

    /// 极高安全级别
    /// - 内存：1 GB
    /// - 时间：约 6 秒
    /// - 防御强度：极好
    pub const MAXIMUM: Self = Self {
        memory_kb: 1024 * 1024, // 1 GB
        iterations: 5,
        parallelism: 4,
    };
}

impl Default for Argon2Params {
    fn default() -> Self {
        Self::STANDARD
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kdf_type_roundtrip() {
        let argon2 = KdfType::Argon2id;
        assert_eq!(KdfType::from_flags(argon2.to_flags()).unwrap(), argon2);
    }

    #[test]
    fn unknown_kdf_type() {
        assert!(KdfType::from_flags(0xFF).is_err());
    }

    #[test]
    fn argon2_params_constants() {
        let standard = Argon2Params::STANDARD;
        assert_eq!(standard.memory_kb, 256 * 1024);
        assert_eq!(standard.iterations, 3);
        assert_eq!(standard.parallelism, 4);
    }
}

/// 使用 Argon2id 派生密钥
///
/// # 参数
/// - `password`: 用户密码
/// - `salt`: 盐值（推荐 32 字节）
/// - `output`: 输出密钥缓冲区（通常 32 字节）
pub fn derive_key(password: &[u8], salt: &[u8], output: &mut [u8]) -> Result<()> {
    use argon2::{Algorithm, Argon2, Params, Version};

    let params = runtime_params();
    let argon2_params = Params::new(
        params.memory_kb,
        params.iterations,
        params.parallelism,
        Some(output.len()),
    )
    .map_err(|e| VeilError::Format(format!("Argon2 参数无效: {}", e)))?;

    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon2_params);

    argon2
        .hash_password_into(password, salt, output)
        .map_err(|e| VeilError::Format(format!("Argon2 派生失败: {}", e)))?;

    Ok(())
}

pub(crate) fn runtime_params() -> Argon2Params {
    #[cfg(debug_assertions)]
    if cfg!(test)
        || FAST_TEST_KDF.load(Ordering::Relaxed)
        || std::env::var("VEIL_TEST_KDF").as_deref() == Ok("fast")
    {
        return Argon2Params {
            memory_kb: 8 * 1024,
            iterations: 1,
            parallelism: 1,
        };
    }

    Argon2Params::STANDARD
}

/// 仅供 debug 集成测试使用。release 构建不提供该入口。
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn enable_fast_test_kdf() {
    FAST_TEST_KDF.store(true, Ordering::Relaxed);
}
