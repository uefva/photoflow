//! 命名、路径、文件枚举——四个命令共用的地基。
//!
//! 对应 `photoflow.py` 里的这几个函数：
//!   check_batch_name / batch_name_error / batch_dir / batch_name_list /
//!   iter_files / strip_ed / human / paths
//!
//! 目录名不是写死的：从 `config` 里取（配置文件或默认值），
//! 这样「设置」里改过的名字在 CLI 上同样生效。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::config;

pub const DEFAULT_RAW: &str = "04_RAW原图";
pub const DEFAULT_JPG: &str = "05_JPG图";
pub const DEFAULT_ED: &str = "03_缓存-调色ed";
pub const DEFAULT_ARC: &str = "99_图库/原图JPG归档";
pub const DEFAULT_VID: &str = "02-视频";

/// 相机 RAW 扩展名（不含点）。
pub const RAW_EXT: &[&str] = &[
    "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2", "raf", "dng", "orf", "rw2", "pef", "srw",
    "raw", "3fr", "x3f",
];

/// 会被当成「原图」的扩展名（不含点）。
pub const JPG_EXT: &[&str] = &["jpg", "jpeg", "jpe", "png", "heic", "heif", "tif", "tiff"];

/// 视频扩展名（不含点）。分发时进视频目录，永不参与照片的成品/原图匹配。
pub const VIDEO_EXT: &[&str] = &[
    "mp4", "mov", "m4v", "avi", "mkv", "wmv", "flv", "webm", "mts", "m2ts", "3gp", "mpg", "mpeg",
    "insv", "lrv",
];

/// 遍历时跳过的系统目录（小写比较）。
const SKIP_DIR_NAMES: &[&str] = &[
    "system volume information",
    "$recycle.bin",
    ".trashes",
    ".spotlight-v100",
    ".fseventsd",
    "dcim_bkp",
];

/// 给用户看的目录名。一律用这套说法，不写 03/04/05/99 这类编号。
pub fn area_name(key: &str) -> &'static str {
    match key {
        "raw" => "RAW 目录",
        "jpg" => "原图目录",
        "ed" => "成品目录",
        "arc" => "归档目录",
        "vid" => "视频目录",
        _ => "目录",
    }
}

pub const AREA_KEYS: [&str; 5] = ["raw", "jpg", "ed", "arc", "vid"];

#[derive(Debug, Clone)]
pub struct Dirs {
    pub raw: PathBuf,
    pub jpg: PathBuf,
    pub ed: PathBuf,
    pub arc: PathBuf,
    pub vid: PathBuf,
    pub logs: PathBuf,
}

/// 五个业务目录（名字按当前配置）。
pub fn dirs(root: &Path) -> Dirs {
    let arc = join_rel(root, &config::dir_name("arc"));
    Dirs {
        raw: join_rel(root, &config::dir_name("raw")),
        jpg: join_rel(root, &config::dir_name("jpg")),
        ed: join_rel(root, &config::dir_name("ed")),
        vid: join_rel(root, &config::dir_name("vid")),
        logs: arc.join("_logs"),
        arc,
    }
}

/// 把配置里的相对目录名（可能带 / 或 \）拼到根目录下。
/// 用 Path::components 逐段拼，避免 Windows 上 `\` 和 `/` 混用出现怪路径。
fn join_rel(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for seg in rel.split(['/', '\\']) {
        if !seg.is_empty() && seg != "." {
            p.push(seg);
        }
    }
    p
}

/// 两个路径是不是同一个位置。
///
/// 先各自 canonicalize（会解析符号链接 / junction、统一大小写和分隔符）；
/// 任一侧不存在时退回字面比较——只是兜底，不该在这条路上报错。
pub fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// 确认根目录可用：连 RAW 目录都没有，说明盘没挂上或者路径填错了。
/// 这一步的提示语要具体，因为这是最常见的失败原因。
pub fn check_root(root: &Path) -> Result<Dirs, String> {
    let d = dirs(root);
    if !d.raw.is_dir() {
        return Err(format!(
            "找不到 RAW 目录：{}\n照片库所在的盘没挂载，或者路径填错了。\
             可以用 --root 指定正确的根目录。",
            d.raw.display()
        ));
    }
    Ok(d)
}

// ---------------------------------------------------------------- 批次名

pub fn is_raw_ext(ext: &str) -> bool {
    RAW_EXT.contains(&ext)
}

pub fn is_jpg_ext(ext: &str) -> bool {
    JPG_EXT.contains(&ext)
}

pub fn is_video_ext(ext: &str) -> bool {
    VIDEO_EXT.contains(&ext)
}

/// 是不是「照片类」文件（RAW 或 JPG 家族）。归档/体检只把这个范围当原图候选。
pub fn is_photo_ext(ext: &str) -> bool {
    is_raw_ext(ext) || is_jpg_ext(ext)
}

/// 批次名的**日期格式**校验。返回 Err(提示) 表示不合规。
///
/// 对应 Python 的 NAME_RE / LOOSE_NAME_RE 两条正则：
///   - 严格：`^\d{4}-\d{2}-\d{2}-.+$`，且日期必须真实存在、地点不能含空格；
///   - 宽松：月/日没补零，提示改成补零后的写法。
pub fn check_batch_name(name: &str) -> Result<(), String> {
    let cs: Vec<char> = name.chars().collect();

    // 严格形式：4-2-2-非空
    if cs.len() > 11
        && cs[4] == '-'
        && cs[7] == '-'
        && cs[10] == '-'
        && cs[..4].iter().all(|c| c.is_ascii_digit())
        && cs[5..7].iter().all(|c| c.is_ascii_digit())
        && cs[8..10].iter().all(|c| c.is_ascii_digit())
    {
        let y: i32 = cs[..4].iter().collect::<String>().parse().unwrap_or(0);
        let m: u32 = cs[5..7].iter().collect::<String>().parse().unwrap_or(0);
        let d: u32 = cs[8..10].iter().collect::<String>().parse().unwrap_or(0);
        if !valid_date(y, m, d) {
            return Err("日期不存在".into());
        }
        let loc: String = cs[11..].iter().collect();
        if loc.contains(' ') {
            return Err("地点含空格，建议用 - 或 _".into());
        }
        return Ok(());
    }

    // 宽松形式：月/日 1~2 位
    if let Some((y, m, d, rest)) = loose_match(&cs) {
        return Err(format!(
            "月/日未补零，建议改为 {:04}-{:02}-{:02}-{}",
            y, m, d, rest
        ));
    }

    Err("不符合 YYYY-MM-DD-地点 格式".into())
}

fn loose_match(cs: &[char]) -> Option<(i32, u32, u32, String)> {
    let digits = |cs: &[char], i: &mut usize| -> Option<String> {
        let start = *i;
        while *i < cs.len() && cs[*i].is_ascii_digit() && *i - start < 2 {
            *i += 1;
        }
        if *i == start {
            return None;
        }
        Some(cs[start..*i].iter().collect())
    };

    if cs.len() < 4 || !cs[..4].iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let year: i32 = cs[..4].iter().collect::<String>().parse().ok()?;
    let mut i = 4usize;
    if cs.get(i) != Some(&'-') {
        return None;
    }
    i += 1;
    let mo = digits(cs, &mut i)?;
    if cs.get(i) != Some(&'-') {
        return None;
    }
    i += 1;
    let day = digits(cs, &mut i)?;
    if cs.get(i) != Some(&'-') {
        return None;
    }
    i += 1;
    if i >= cs.len() {
        return None;
    }
    let rest: String = cs[i..].iter().collect();
    let m: u32 = mo.parse().ok()?;
    let d: u32 = day.parse().ok()?;
    if m == 0 || d == 0 {
        return None;
    }
    Some((year, m, d, rest))
}

pub fn valid_date(y: i32, m: u32, d: u32) -> bool {
    if !(1..=12).contains(&m) || d == 0 {
        return false;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => return false,
    };
    d <= days
}

/// 批次名的**安全性**校验。返回 Some(原因) 表示不能用。
/// 只判路径安全，不判日期格式；这一项 `--force` 也绕不过去。
pub fn batch_name_error(name: &str) -> Option<String> {
    if name.is_empty() || name == "." || name == ".." {
        return Some("批次名为空，或为 . / ..".into());
    }
    if name.chars().count() > 120 {
        return Some("批次名超过 120 个字符".into());
    }
    let bad: Vec<char> = name
        .chars()
        .filter(|&c| {
            matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
                || (c as u32) < 32
        })
        .collect();
    if !bad.is_empty() {
        let shown: Vec<String> = bad.iter().map(|c| format!("{:?}", c)).collect();
        return Some(format!("含路径分隔符或控制字符：{}", shown.join(" ")));
    }
    if name.trim() != name {
        return Some("首尾不能有空格".into());
    }
    if name.ends_with('.') {
        return Some("结尾不能是点（Windows 会悄悄去掉）".into());
    }
    None
}

/// 把批次名拼到 base 下，并确认最终路径没有跑出 base。
///
/// 不用 canonicalize：极空间这类网络卷上 Windows 解析真实路径会失败
/// （WinError 1005），连存在的目录都解析不了。这里只看字符串。
pub fn batch_dir(base: &Path, name: &str) -> Result<PathBuf, String> {
    if let Some(err) = batch_name_error(name) {
        return Err(format!(
            "批次名不安全：{name}\n  {err}\n  这一项不受 --force 影响，请先改成安全的名称。"
        ));
    }
    let target = base.join(name);
    if target.file_name().and_then(|s| s.to_str()) != Some(name) {
        return Err(format!("批次路径越界，已拒绝执行：{}", target.display()));
    }
    Ok(target)
}

/// 列出批次目录，跳过 `_logs` 这类下划线/点开头的辅助目录。
pub fn batch_name_list(folder: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(folder) {
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().to_string();
            if name.starts_with('_') || name.starts_with('.') {
                continue;
            }
            if ent.path().is_dir() {
                out.push(name);
            }
        }
    }
    out.sort();
    out
}

// ---------------------------------------------------------------- 文件枚举

/// 递归列文件，跳过系统目录。按小写全路径排序，保证多次运行顺序一致
/// （对应 Python 的 `sorted(..., key=lambda p: str(p).lower())`）。
pub fn iter_files(folder: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(folder, &mut out);
    out.sort_by_key(|p| p.to_string_lossy().to_lowercase());
    out
}

fn walk(folder: &Path, out: &mut Vec<PathBuf>) {
    let rd = match std::fs::read_dir(folder) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for ent in rd.flatten() {
        let path = ent.path();
        let ft = match ent.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_dir() {
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if SKIP_DIR_NAMES.contains(&name.as_str()) {
                continue;
            }
            walk(&path, out);
        } else if ft.is_file() {
            out.push(path);
        }
    }
}

pub fn ext_lower(p: &Path) -> String {
    p.extension()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

pub fn stem_lower(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

pub fn stem_raw(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

pub fn name_raw(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------- 剥 -ed

const DENOISE_SUFFIXES: &[&str] = &["-已增强-降噪", "-Enhanced-NR"];
const EDIT_SUFFIXES: &[&str] = &["-Edit"];

/// 判断 `-ed` 之后的尾巴能否被 `(?:\d+|[-_]\w+)*` 完整吃掉。
fn tail_ok(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_digit() {
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
        } else if c == '-' || c == '_' {
            i += 1;
            if i >= chars.len() || !is_word_char(chars[i]) {
                return false;
            }
            while i < chars.len() && is_word_char(chars[i]) {
                i += 1;
            }
        } else {
            return false;
        }
    }
    true
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// 对应 `ED_RE = ^(?P<base>.+?)-ed(?:\d+|[-_][\w]+)*$`（忽略大小写，惰性取最左）。
/// 剥出 base 后，再一层层去掉降噪 / `-Edit` 中缀。
pub fn strip_ed(stem: &str) -> Option<String> {
    let orig: Vec<char> = stem.chars().collect();
    let lower: Vec<char> = stem.to_lowercase().chars().collect();
    // 大小写转换改了字符数（极少见）时放弃匹配，避免切片错位。
    if lower.len() != orig.len() {
        return None;
    }

    let mut base_end = None;
    let mut i = 1; // base 至少 1 个字符
    while i + 3 <= lower.len() {
        if lower[i] == '-' && lower[i + 1] == 'e' && lower[i + 2] == 'd' {
            let rest: String = orig[i + 3..].iter().collect();
            if tail_ok(&rest) {
                base_end = Some(i);
                break;
            }
        }
        i += 1;
    }

    let end = base_end?;
    let mut base: String = orig[..end].iter().collect();

    let mut changed = true;
    while changed {
        changed = false;
        for suf in DENOISE_SUFFIXES.iter().chain(EDIT_SUFFIXES.iter()) {
            if ends_with_ci(&base, suf) && base.chars().count() > suf.chars().count() {
                let keep = base.chars().count() - suf.chars().count();
                base = base.chars().take(keep).collect();
                changed = true;
                break;
            }
        }
    }
    Some(base)
}

fn ends_with_ci(s: &str, suf: &str) -> bool {
    s.to_lowercase().ends_with(&suf.to_lowercase())
}

// ---------------------------------------------------------------- 杂项

/// 人类可读的大小。为 0 时是「0B」，其余保留一位小数。
pub fn human(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{}B", n);
    }
    let mut v = n as f64;
    let mut idx = 0;
    while v >= 1024.0 && idx < UNITS.len() - 1 {
        v /= 1024.0;
        idx += 1;
    }
    format!("{:.1}{}", v, UNITS[idx])
}

/// 判断一批文件名里是否已经有这个（小写）名字。
pub fn contains_name(set: &HashSet<(String, String)>, kind: &str, name: &str) -> bool {
    set.contains(&(kind.to_string(), name.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_name_branches() {
        assert!(check_batch_name("2026-11-26-梧桐山").is_ok());
        assert!(check_batch_name("2026-11-26-梧桐岛+西湾红树林").is_ok());

        let e = check_batch_name("2026-7-25-大通山").unwrap_err();
        assert!(e.contains("未补零"), "{e}");
        assert!(e.contains("2026-07-25-大通山"), "{e}");

        assert!(check_batch_name("2026-7-5-x").unwrap_err().contains("2026-07-05-x"));
        assert!(check_batch_name("比特").unwrap_err().contains("不符合"));
        assert!(check_batch_name("2026-13-01-不存在").unwrap_err().contains("日期不存在"));
        assert!(check_batch_name("2026-02-30-不存在").unwrap_err().contains("日期不存在"));
        assert!(check_batch_name("2026-11-26-梧桐 山").unwrap_err().contains("空格"));
    }

    #[test]
    fn batch_name_safety() {
        assert!(batch_name_error("2025-10-01-梧桐岛+西湾红树林").is_none());
        assert!(batch_name_error("").is_some());
        assert!(batch_name_error(".").is_some());
        assert!(batch_name_error("..").is_some());
        assert!(batch_name_error("x/y").is_some());
        assert!(batch_name_error("x\\y").is_some());
        assert!(batch_name_error("a:b").is_some());
        assert!(batch_name_error(" 前导空格").is_some());
        assert!(batch_name_error("结尾点.").is_some());
        assert!(batch_name_error(&"长".repeat(121)).is_some());
    }

    #[test]
    fn strip_ed_table() {
        assert_eq!(strip_ed("DSC1-ed").as_deref(), Some("DSC1"));
        assert_eq!(strip_ed("DSC1-ED").as_deref(), Some("DSC1"));
        assert_eq!(strip_ed("DSC1-ed2").as_deref(), Some("DSC1"));
        assert_eq!(strip_ed("DSC1-ed-2").as_deref(), Some("DSC1"));
        assert_eq!(strip_ed("DSC1-ed-Edit").as_deref(), Some("DSC1"));
        assert_eq!(strip_ed("DSC1-Edit-ed").as_deref(), Some("DSC1"));
        assert_eq!(strip_ed("DSC1-已增强-降噪-ed").as_deref(), Some("DSC1"));
        assert_eq!(strip_ed("DSC1-Enhanced-NR-ed").as_deref(), Some("DSC1"));
        assert_eq!(strip_ed("DSC1-Edit-已增强-降噪-ed").as_deref(), Some("DSC1"));
        assert_eq!(strip_ed("DSC1-edited"), None);
        assert_eq!(strip_ed("DSC1-edraw"), None);
        assert_eq!(strip_ed("DSC1"), None);
    }

    #[test]
    fn batch_dir_rejects_traversal_without_touching_fs() {
        let base = std::path::Path::new("/tmp/pf-nonexistent-lib/04_RAW原图");
        assert!(batch_dir(base, "../escape").is_err());
        assert!(batch_dir(base, "a/b").is_err());
        assert!(batch_dir(base, "..").is_err());
        // 合法名字应当直接拼出来，不需要目录真的存在（不碰文件系统，
        // 也就不存在网络卷上 realpath 失败的问题）
        let ok = batch_dir(base, "2026-11-26-测试").unwrap();
        assert_eq!(ok.file_name().unwrap().to_string_lossy(), "2026-11-26-测试");
    }

    #[test]
    fn human_units() {
        assert_eq!(human(0), "0B");
        assert_eq!(human(1023), "1023B");
        assert_eq!(human(1024), "1.0KB");
        assert_eq!(human(1024 * 1024), "1.0MB");
    }
}
