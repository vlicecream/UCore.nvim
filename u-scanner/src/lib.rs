//! UCore scanner/server library entry.
//! UCore 扫描器和 server 的 crate 根入口。
//!
//! This file only wires modules together.
//! 这个文件只负责组织模块，不放具体业务逻辑。

pub mod completion;
pub mod db;
pub mod modify;
pub mod parser;
pub mod query;
pub mod refresh;
pub mod server;
pub mod types;
pub mod uasset;

/// Backward-compatible scanner namespace.
/// 兼容旧代码里的 scanner 命名空间。
///
/// Existing code can keep using `crate::scanner::process_file`.
/// 老代码可以继续使用 `crate::scanner::process_file`。
///
/// New parser modules should live under `crate::parser`.
/// 新 parser 模块建议统一放到 `crate::parser` 下。
pub mod scanner {
    pub use super::parser::cpp::*;
}
