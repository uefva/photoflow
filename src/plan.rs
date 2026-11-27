//! 计划指纹：把「这次要动哪些文件」压成一个短字符串。
//!
//! 预演时算一次交给界面，执行时再算一次，对不上就说明中间被动过——
//! 这一条是「先预演、确认清单无误再执行」这个承诺的唯一硬保障。
//!
//! 指纹只吃 路径、大小、修改时间：内容不变、位置不变，指纹就不变。

use std::path::Path;
use std::time::UNIX_EPOCH;

use crate::sha256::Sha256;

/// (大小, 修改时间纳秒)。文件不在了就返回 (None, None)，交给指纹体现差异。
pub fn stat_sig(p: &Path) -> (Option<u64>, Option<u128>) {
    match std::fs::metadata(p) {
        Ok(md) => {
            let mt = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos());
            (Some(md.len()), mt)
        }
        Err(_) => (None, None),
    }
}

/// 把若干行压成 16 位十六进制指纹。行之间用 \\x1f 分隔、每行以 \\x1e 结束，
/// 和 Python 版 `plan_token` 的拼法一致，避免「同一份清单两个实现算出不同指纹」。
pub fn token(rows: &[Vec<String>]) -> String {
    let mut h = Sha256::new();
    for row in rows {
        h.update(row.join("\u{1f}").as_bytes());
        h.update(b"\x1e");
    }
    let digest = h.finalize();
    digest.iter().take(8).map(|b| format!("{:02x}", b)).collect()
}

/// 执行前核对：现在这份计划，和用户预演时确认的还是不是同一份。
pub fn check_plan(expected: Option<&str>, actual: &str) -> Result<(), String> {
    match expected {
        None | Some("") => Ok(()),
        Some(exp) if exp == actual => Ok(()),
        Some(exp) => Err(format!(
            "计划已经变了，不能直接执行。\n  预演时：{exp}\n  现在　：{actual}\n\
             这中间照片库里多了、少了、或者有文件被改动过。\n\
             请重新预演，看清新的清单再执行。"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: &str, src: &str, dst: &str, size: &str, mt: &str) -> Vec<String> {
        vec![
            kind.to_string(),
            src.to_string(),
            dst.to_string(),
            size.to_string(),
            mt.to_string(),
        ]
    }

    #[test]
    fn token_is_stable_and_sensitive() {
        let rows = vec![
            row("jpg", "/card/IMG_1.JPG", "/lib/05/IMG_1.JPG", "100", "111"),
            row("jpg", "/card/IMG_2.JPG", "/lib/05/IMG_2.JPG", "200", "222"),
        ];
        let base = token(&rows);
        assert_eq!(base, token(&rows.clone()));
        assert_eq!(base.len(), 16);

        let mut bigger = rows.clone();
        bigger[0][3] = "101".into();
        assert_ne!(base, token(&bigger), "大小变了指纹必须变");

        let mut touched = rows.clone();
        touched[1][4] = "223".into();
        assert_ne!(base, token(&touched), "修改时间变了指纹必须变");

        let reversed: Vec<Vec<String>> = rows.iter().rev().cloned().collect();
        assert_ne!(base, token(&reversed), "顺序变了指纹也该变");
    }

    #[test]
    fn stat_sig_missing_file() {
        let p = std::env::temp_dir().join("pf-no-such-file-12345.bin");
        assert_eq!(stat_sig(&p), (None, None));
    }

    #[test]
    fn check_plan_messages() {
        assert!(check_plan(None, "abc").is_ok());
        assert!(check_plan(Some(""), "abc").is_ok());
        assert!(check_plan(Some("abc"), "abc").is_ok());
        let e = check_plan(Some("old"), "new").unwrap_err();
        assert!(e.contains("计划已经变了"), "{e}");
        assert!(e.contains("old") && e.contains("new"), "{e}");
    }
}
