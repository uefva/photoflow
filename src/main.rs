//! photoflow 命令行入口。
//!
//! 所有逻辑都在 `photoflow_rs::cli`——图形界面那支二进制也用同一个函数，
//! 保证两边行为一致。这里只负责传参数、返回退出码。

use std::process::ExitCode;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    ExitCode::from(photoflow_rs::cli::run(argv) as u8)
}
