//! 工作区根目录下的占位二进制入口。
//!
//! 实际 CLI 位于 `crates/veil-cli`；本文件不属于 Cargo workspace 成员，仅保留为
//! 单独执行该源码时的最小程序。

/// 输出占位问候语。
fn main() {
    println!("Hello, world!");
}
