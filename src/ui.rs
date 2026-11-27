//! 输出小工具：级别标签、预演/执行抬头。
//!
//! 标签宽度和 Python 版一模一样（`"  OK  "` 这类定宽 6 字符），
//! 这样两边的输出可以直接 diff 对照。

pub const C_OK: &str = "  OK  ";
pub const C_WARN: &str = " WARN ";
pub const C_ERR: &str = " ERR  ";
pub const C_INFO: &str = " INFO ";

/// 统一输出「预演 / 执行」抬头。
pub fn confirm_apply(apply: bool, what: &str) {
    if apply {
        println!("== 执行：{what} ==\n");
    } else {
        // 抬头和说明是同一次 print：中间只有换行，末尾才空一行，和 Python 版一致
        println!(
            "== 预演：{what} ==\n（下面只是清单，没有动任何文件。确认无误后加 --apply 再跑一次）\n"
        );
    }
}
