//! 密钥派生函数类型与参数。
//!
//! 容器把 KDF 标识写入头部 flags，再由本模块解析；实际派生统一走 Argon2id。
//! 标准参数面向常规存储和交互延迟，测试构建可通过专用开关降低参数，避免每次
//! 单元测试都承担完整的内存和时间成本。

use crate::error::{Result, VeilError};

#[cfg(debug_assertions)]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(debug_assertions)]
static FAST_TEST_KDF: AtomicBool = AtomicBool::new(false);

/// 容器头部可识别的密钥派生算法标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KdfType {
    /// Argon2id，当前容器格式唯一支持的类型。
    Argon2id = 0x1,
}

impl KdfType {
    /// 从容器 Header 的 flags 低四位解析 KDF 类型。
    ///
    /// # 错误
    /// 低四位为未知值时返回 [`VeilError::Format`]，调用方应停止解析该容器。
    pub fn from_flags(flags: u16) -> Result<Self> {
        match flags & 0x0F {
            0x1 => Ok(Self::Argon2id),
            n => Err(VeilError::Format(format!("未知的 KDF 类型: 0x{:x}", n))),
        }
    }

    /// 将 KDF 类型编码为写入 Header flags 的值。
    pub fn to_flags(self) -> u16 {
        self as u16
    }
}

/// Argon2id 的内存、迭代和并行度参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Argon2Params {
    /// 每次派生占用的内存，单位为 KiB。
    pub memory_kb: u32,
    /// Argon2 的迭代轮数。
    pub iterations: u32,
    /// Argon2 内部并行度。
    pub parallelism: u32,
}

impl Argon2Params {
    /// 标准安全级别。
    ///
    /// 默认参数兼顾抵抗离线猜测和交互延迟：
    /// - 内存：256 MB
    /// - 迭代：3 次
    /// - 并行度：4
    pub const STANDARD: Self = Self {
        memory_kb: 256 * 1024,
        iterations: 3,
        parallelism: 4,
    };

    /// 高安全级别：增加内存和迭代次数，以更高的派生延迟换取更高攻击成本。
    pub const HIGH: Self = Self {
        memory_kb: 512 * 1024,
        iterations: 4,
        parallelism: 4,
    };

    /// 极高安全级别：面向高价值容器，不适用于内存受限环境。
    pub const MAXIMUM: Self = Self {
        memory_kb: 1024 * 1024,
        iterations: 5,
        parallelism: 4,
    };
}

impl Default for Argon2Params {
    /// 默认使用 [`Argon2Params::STANDARD`]。
    fn default() -> Self {
        Self::STANDARD
    }
}

/// 使用当前运行时参数执行 Argon2id 派生。
///
/// # 参数
/// - `password`：用户密码的原始字节。
/// - `salt`：盐值；调用方负责为密码上下文生成并持久化盐值。
/// - `output`：输出缓冲区，其长度同时决定派生密钥长度。
///
/// # 错误
/// 参数不满足 Argon2 约束或底层派生失败时返回 [`VeilError::Format`]。
pub fn derive_key(password: &[u8], salt: &[u8], output: &mut [u8]) -> Result<()> {
    use argon2::{Algorithm, Argon2, Params, Version};

    let params = runtime_params();
    // 输出长度同时决定内存参数中的派生密钥长度。
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

/// 返回当前构建和进程应使用的 Argon2id 参数。
///
/// Debug 测试、显式启用快速 KDF，或设置 `VEIL_TEST_KDF=fast` 时使用低开销参数；
/// 其余情况均使用标准参数。Release 构建不会编译测试开关。
pub(crate) fn runtime_params() -> Argon2Params {
    // 快速参数只允许在 Debug/测试路径生效，Release 始终走标准强度。
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

/// 启用低开销测试参数，仅供 Debug 构建的集成测试调用。
///
/// 该开关会降低密钥派生强度，不应在真实容器上使用；Release 构建不包含此入口。
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn enable_fast_test_kdf() {
    FAST_TEST_KDF.store(true, Ordering::Relaxed);
}

/// KDF 标志和参数常量的单元测试。
#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 KDF 类型编码后能够还原。
    #[test]
    fn kdf_type_roundtrip() {
        let argon2 = KdfType::Argon2id;
        assert_eq!(KdfType::from_flags(argon2.to_flags()).unwrap(), argon2);
    }

    /// 验证未知 KDF 标志会被拒绝。
    #[test]
    fn unknown_kdf_type() {
        assert!(KdfType::from_flags(0xFF).is_err());
    }

    /// 验证标准 Argon2id 参数保持预期常量。
    #[test]
    fn argon2_params_constants() {
        let standard = Argon2Params::STANDARD;
        assert_eq!(standard.memory_kb, 256 * 1024);
        assert_eq!(standard.iterations, 3);
        assert_eq!(standard.parallelism, 4);
    }
}
