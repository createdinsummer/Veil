//! Argon2id 密钥派生与参数。
//!
//! 本模块负责上层调用的 Argon2id 派生；SQLCipher 数据库使用 database 模块配置的
//! PBKDF2-HMAC-SHA512，两套派生用途不同，不能把本模块参数当作数据库解锁参数。
//! 标准参数面向常规存储和交互延迟，测试构建可通过专用开关降低参数，避免每次
//! 单元测试都承担完整的内存和时间成本。
//!
//! 所属业务模块：veil-core 的密钥管理层。KDF（密钥派生函数）= 把用户密码和随机
//! 盐值通过单向计算转化为固定长度的密钥材料，让攻击者难以对常见密码做离线猜测。
//! 本模块集中实现 Argon2id 派生；调用方负责明确派生用途、生成盐值并管理密钥生命周期。
//!
//! 文件内核心清单：
//! - `Argon2Params`：内存/迭代/并行度三参数集合，含标准、高、极高三级常量。
//! - `derive_key`：执行一次 Argon2id 派生，把密码+盐变成输出缓冲区里的密钥。
//! - `runtime_params`：按构建类型与测试开关选择当前应使用的参数。
//! - `enable_fast_test_kdf`：仅 Debug 构建的低开销测试开关。
//!
//! 依赖能力：
//! - `argon2` 库：Argon2id 算法的实现；选择 Argon2id 变体（抵抗侧信道与 GPU 攻击
//!   的混合型），版本 V0x13。
//!
//! 整体业务目标：密码派生参数全库统一（标准 256 MiB/3 轮/并行 4），Release 构建
//! 绝不允许通过环境变量或开关降低强度；测试路径用低开销参数保证测试速度。

use crate::error::{Result, VeilError};

#[cfg(debug_assertions)]
use std::sync::atomic::{AtomicBool, Ordering};

/// 快速测试 KDF 开关：置真后派生使用低开销参数（仅 Debug 构建存在）。
/// 由 `enable_fast_test_kdf` 写入，`runtime_params` 读取。
#[cfg(debug_assertions)]
static FAST_TEST_KDF: AtomicBool = AtomicBool::new(false);

/// Argon2id 的内存、迭代和并行度参数。
///
/// 业务定位：一份"派生开销"配置——参数越大，攻击者离线猜测密码越贵，但每次
/// 解锁/派生也越慢越耗内存。标准参数在安全与交互延迟之间取平衡。
///
/// 使用场景：STANDARD 是 runtime_params 的生产默认值；HIGH/MAXIMUM 仅为预定义配置，
/// 当前 derive_key 没有让调用方选择这两个等级的入参，不能据此认为容器已支持等级切换。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Argon2Params {
    /// 每次派生占用的内存，单位为 KiB。
    /// 值越大，攻击者并行猜测的成本越高。
    pub memory_kb: u32,
    /// Argon2 的迭代轮数。
    /// 值越大，单次派生耗时越长，暴力破解越慢。
    pub iterations: u32,
    /// Argon2 内部并行度。
    /// 内存会被切成这么多条通道并行填充。
    pub parallelism: u32,
}

impl Argon2Params {
    /// 标准安全级别。
    ///
    /// 默认参数兼顾抵抗离线猜测和交互延迟：
    /// - 内存：256 MiB（262144 KiB）
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
    /// 功能：为未指定配置的密钥派生使用项目标准强度参数。
    /// 入参：无。
    /// 返回值：Argon2Params::STANDARD，即 256 MiB 工作内存、3 轮、4 条通道。
    /// 异常：只返回常量，不分配派生工作内存，不返回错误。
    /// 注意事项：不会执行派生；runtime_params 在 Debug 测试条件下仍可能选择低开销配置，Default 本身不读测试开关。
    fn default() -> Self {
        Self::STANDARD
    }
}

/// 从口令字节与盐派生指定长度的密钥，供上层保护密钥材料。
/// 入参：password（&[u8]）原始口令字节，本层不验证非空/强度；salt（&[u8]）至少 8 字节，调用方生成并保存；output（&mut [u8]）独占输出缓冲，长度至少 4 字节并符合 Argon2 限制。
/// 返回值：Ok(()) 表示 output 写入派生结果；参数或派生错误为 Err(VeilError::Format)，失败后不得使用输出内容。
/// 异常：Argon2 错误转格式错误；内存分配失败等不保证以 Result 返回，也不捕获 panic。
/// 注意事项：同步计算会消耗工作内存和 CPU，运行时参数在 Debug 测试条件可降低成本，Release 固定标准配置；不保存或清除调用方口令/盐，也不等于配置 SQLCipher 数据库派生参数。
pub fn derive_key(password: &[u8], salt: &[u8], output: &mut [u8]) -> Result<()> {
    use argon2::{Algorithm, Argon2, Params, Version};

    let params = runtime_params();
    // 输出长度决定密钥字节数，不代表 Argon2 工作内存大小；工作内存由 memory_kb 决定。
    // 把三参数与目标密钥长度组装成 Argon2 的底层参数对象。
    let argon2_params = Params::new(
        params.memory_kb,
        params.iterations,
        params.parallelism,
        Some(output.len()),
    )
    .map_err(|e| VeilError::Format(format!("Argon2 参数无效: {}", e)))?;

    // 固定使用 Argon2id 变体与 V0x13 版本，保证全平台派生结果一致
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon2_params);

    argon2
        .hash_password_into(password, salt, output)
        .map_err(|e| VeilError::Format(format!("Argon2 派生失败: {}", e)))?;

    Ok(())
}

/// 功能：为当前 Argon2id 调用选择生产或测试开销，避免测试反复承担标准派生成本。
/// 入参：无；Debug 构建读取 cfg!(test)、FAST_TEST_KDF 和 VEIL_TEST_KDF 环境值。
/// 返回值：Argon2Params；任一 Debug 测试条件成立时为 8 MiB/1 轮/1 通道，否则为 STANDARD。
/// 异常：不返回 Result；环境变量读取失败或不是 fast 不启用对应快速条件。
/// 注意事项：Release 不编译快速分支；此处参数不影响 SQLCipher 的独立数据库派生配置，测试开关也不应作用于真实敏感数据。
pub(crate) fn runtime_params() -> Argon2Params {
    // 快速参数只允许在 Debug/测试路径生效，Release 始终走标准强度。
    // 三个条件任一满足即降低参数：单元测试运行中、集成测试显式开启、环境变量开启。
    #[cfg(debug_assertions)]
    if cfg!(test)
        || FAST_TEST_KDF.load(Ordering::Relaxed)
        || std::env::var("VEIL_TEST_KDF").as_deref() == Ok("fast")
    {
        return Argon2Params {
            // 测试专用低开销：8 MiB 内存、1 轮、单线程，足够快且能验证算法路径
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
/// 功能：在 Debug 集成测试中为后续 Argon2id 调用开启低开销派生。
/// 入参：无。
/// 返回值：无；将进程共享 FAST_TEST_KDF 标志置为 true。
/// 异常：原子写入不返回错误。
/// 注意事项：仅 Debug 构建存在；该入口没有恢复 false 的对称方法，开启后会影响本进程后续调用及并行测试。
pub fn enable_fast_test_kdf() {
    FAST_TEST_KDF.store(true, Ordering::Relaxed);
}

/// Argon2id 参数常量的单元测试。
#[cfg(test)]
mod tests {
    use super::*;

    /// 验证标准 Argon2id 参数保持预期常量。
    #[test]
    /// 返回值：无；正常执行至末尾表示本场景通过。
    /// 异常：测试准备、业务调用或断言中的 panic 会使测试失败，不作为生产错误返回。
    /// 注意事项：本方法仅验证注释所述场景；涉及的环境和资源按测试体中的方式初始化与清理。
    fn argon2_params_constants() {
        let standard = Argon2Params::STANDARD;
        assert_eq!(standard.memory_kb, 256 * 1024);
        assert_eq!(standard.iterations, 3);
        assert_eq!(standard.parallelism, 4);
    }
}
