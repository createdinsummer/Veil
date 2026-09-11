//! Veil CLI 的可复用库入口。
//!
//! 本库暴露命令实现、国际化、提示渲染和终端输出编码，供二进制入口及集成测试复用。
//! 终端参数解析和进程退出仍由 `main.rs` 负责，库层函数统一返回错误值。

pub mod commands;
pub mod i18n;
pub mod output_encoding;
pub mod hints;
