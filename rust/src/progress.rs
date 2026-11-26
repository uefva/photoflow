//! 给图形界面用的两条机器可读输出：进度行、计划指纹。
//!
//! 只有带 `--progress` 时才输出，命令行用户永远看不到这些行。
//! 前缀故意用不可能出现在正常输出里的字符串，界面按前缀整行截走。

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

pub const PROGRESS_TAG: &str = "@@PROGRESS@@";
pub const PLAN_TAG: &str = "@@PLAN@@";

static ON: AtomicBool = AtomicBool::new(false);

pub fn enable(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

/// 打一行进度。节流到最多约 200 次，避免几万个文件把管道刷爆。
///
/// `done` 必须真的递增——它只喂这一行和界面上的进度条，不参与任何统计，
/// 所以少加一次不会影响结果，只是进度条不动（这类退化测试是盯着的）。
pub fn progress(done: usize, total: usize, label: &str) {
    if !enabled() || total == 0 {
        return;
    }
    let step = std::cmp::max(1, total / 200);
    if done < total && done % step != 0 {
        return;
    }
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{}\t{}\t{}\t{}", PROGRESS_TAG, done, total, label);
    let _ = out.flush();
}

/// 把计划指纹交给图形界面（同样只在 `--progress` 时输出）。
pub fn announce_plan(token: &str) {
    if !enabled() {
        return;
    }
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{}\t{}", PLAN_TAG, token);
    let _ = out.flush();
}
