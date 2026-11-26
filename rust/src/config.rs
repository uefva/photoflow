//! 配置文件：`%USERPROFILE%\.photoflow.json`（可用 `PHOTOFLOW_CONFIG` 指到别处）。
//!
//! 存三样东西：根目录、当前批次、四个目录名。读不到、某一项不合法，就退回默认值，
//! 不报错——这一点和原 Python 版一致，免得配置文件写坏一次就整个工具不能用。
//!
//! 为了保持「零第三方依赖」，这里自带一个够用的 JSON 解析器：只认对象、字符串、
//! 数字、布尔、null、数组，够读自己的配置文件，也够读手工编辑过的。

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use crate::naming::{area_name, AREA_KEYS, DEFAULT_ARC, DEFAULT_ED, DEFAULT_JPG, DEFAULT_RAW};

#[derive(Debug, Clone)]
pub struct Config {
    pub root: Option<String>,
    pub batch: String,
    pub dirs: BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        let mut dirs = BTreeMap::new();
        dirs.insert("raw".to_string(), DEFAULT_RAW.to_string());
        dirs.insert("jpg".to_string(), DEFAULT_JPG.to_string());
        dirs.insert("ed".to_string(), DEFAULT_ED.to_string());
        dirs.insert("arc".to_string(), DEFAULT_ARC.to_string());
        Config {
            root: None,
            batch: String::new(),
            dirs,
        }
    }
}

static CONFIG: OnceLock<Mutex<Config>> = OnceLock::new();

fn slot() -> &'static Mutex<Config> {
    CONFIG.get_or_init(|| Mutex::new(Config::default()))
}

fn with<R>(f: impl FnOnce(&mut Config) -> R) -> R {
    // 锁中毒了也继续用：配置读坏不该让整个进程崩掉。
    let mut guard = slot().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

/// 当前生效的某个目录名（相对照片库根目录）。
pub fn dir_name(key: &str) -> String {
    with(|c| {
        c.dirs
            .get(key)
            .cloned()
            .unwrap_or_else(|| Config::default().dirs[key].clone())
    })
}

pub fn root() -> Option<String> {
    with(|c| c.root.clone())
}

pub fn batch() -> String {
    with(|c| c.batch.clone())
}

pub fn set_root(v: Option<String>) {
    with(|c| c.root = v);
}

pub fn set_batch(v: String) {
    with(|c| c.batch = v);
}

pub fn set_dirs(dirs: BTreeMap<String, String>) {
    with(|c| {
        for (k, v) in dirs {
            if c.dirs.contains_key(&k) {
                c.dirs.insert(k, v);
            }
        }
    });
}

/// 配置文件位置。`PHOTOFLOW_CONFIG` 环境变量优先（测试用它隔离）。
pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("PHOTOFLOW_CONFIG") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    home_dir().join(".photoflow.json")
}

fn home_dir() -> PathBuf {
    for key in ["USERPROFILE", "HOME"] {
        if let Ok(v) = std::env::var(key) {
            if !v.is_empty() {
                return PathBuf::from(v);
            }
        }
    }
    PathBuf::from(".")
}

/// 校验一个目录设置项。返回 Some(原因) 表示不能用。
pub fn check_dir_setting(value: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty() {
        return Some("不能为空".into());
    }
    if is_absolute(v) {
        return Some("要写相对路径，不要写盘符或绝对路径".into());
    }
    if v.split(['/', '\\']).any(|seg| seg.is_empty() || seg == "." || seg == "..") {
        return Some("不能包含空段、. 或 ..".into());
    }
    if v.chars().any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') || (c as u32) < 32) {
        return Some("含有不能用于文件名的字符".into());
    }
    None
}

fn is_absolute(v: &str) -> bool {
    if v.starts_with('/') || v.starts_with('\\') {
        return true;
    }
    let b = v.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// 目录名归一化后再比较：去空白、分隔符统一成 `/`、去掉结尾斜杠、转小写。
/// Windows 上 `05_JPG图` 和 `05_JPG图/`、`a\b` 和 `a/b` 是同一个目录。
fn norm_dir(v: &str) -> String {
    v.trim()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

/// 四个目录里有没有两个指向同一个位置。
///
/// 必须拦住：`archive` 的工作方式就是「从原图目录搬进归档目录」，两者若是同一个
/// 目录，原图会被当成「归档里已经有一份的重复份」直接删掉——静默丢失原始素材。
/// 界面的保存按钮本来就有这道检查，但命令行是直接读配置文件的，得在这里兜住。
pub fn dirs_conflict_in(dirs: &BTreeMap<String, String>) -> Option<String> {
    let defaults = Config::default().dirs;
    let mut seen: BTreeMap<String, &'static str> = BTreeMap::new();
    for key in AREA_KEYS {
        let raw = dirs.get(key).cloned().unwrap_or_else(|| defaults[key].clone());
        let norm = norm_dir(&raw);
        if let Some(prev) = seen.get(&norm) {
            return Some(format!(
                "四个目录不能指向同一个位置：{} 和 {} 都是「{}」。\n\
                 这样归档会把原图目录当成归档目录，把原图自己当重复份删掉。\n\
                 改 {}（或界面「设置」页）把这两个目录分开，再重新运行。",
                area_name(prev),
                area_name(key),
                raw.trim(),
                config_path().display()
            ));
        }
        seen.insert(norm, key);
    }
    None
}

/// 用当前生效的配置做上面那项检查。
pub fn dirs_conflict() -> Option<String> {
    with(|c| dirs_conflict_in(&c.dirs))
}

/// 读配置文件。读不到、或某一项不合法，就保留当前值（进程启动时即默认值）。
pub fn load() {
    let path = config_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return,
    };
    let parsed = match Json::parse(&text) {
        Some(j) => j,
        None => return,
    };
    let obj = match parsed {
        Json::Object(o) => o,
        _ => return,
    };

    with(|c| {
        if let Some(Json::Str(s)) = obj.get("root") {
            if !s.trim().is_empty() {
                c.root = Some(s.trim().to_string());
            }
        }
        if let Some(Json::Str(s)) = obj.get("batch") {
            c.batch = s.trim().to_string();
        }
        if let Some(Json::Object(dirs)) = obj.get("dirs") {
            for (k, v) in dirs {
                if let Json::Str(s) = v {
                    if check_dir_setting(s).is_none() {
                        c.dirs.insert(k.clone(), s.trim().to_string());
                    }
                }
            }
        }
    });
}

/// 写配置文件（GUI 的「设置」页会用；CLI 只读不写，但保留这个能力）。
pub fn save() -> io::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let (root, batch, dirs) = with(|c| (c.root.clone(), c.batch.clone(), c.dirs.clone()));

    let mut s = String::from("{\n");
    s.push_str(&format!("  \"root\": {},\n", json_str(root.as_deref().unwrap_or(""))));
    s.push_str(&format!("  \"batch\": {},\n", json_str(&batch)));
    s.push_str("  \"dirs\": {\n");
    let items: Vec<String> = dirs
        .iter()
        .map(|(k, v)| format!("    {}: {}", json_str(k), json_str(v)))
        .collect();
    s.push_str(&items.join(",\n"));
    s.push_str("\n  }\n}\n");
    std::fs::write(path, s)
}

fn json_str(v: &str) -> String {
    let mut out = String::from("\"");
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 32 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------- 极简 JSON

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

impl Json {
    pub fn parse(text: &str) -> Option<Json> {
        let cs: Vec<char> = text.chars().collect();
        let mut i = 0;
        let v = parse_value(&cs, &mut i)?;
        skip_ws(&cs, &mut i);
        Some(v)
    }
}

fn skip_ws(cs: &[char], i: &mut usize) {
    while *i < cs.len() && cs[*i].is_whitespace() {
        *i += 1;
    }
}

fn parse_value(cs: &[char], i: &mut usize) -> Option<Json> {
    skip_ws(cs, i);
    match cs.get(*i)? {
        '{' => parse_object(cs, i),
        '[' => parse_array(cs, i),
        '"' => parse_string(cs, i).map(Json::Str),
        't' => expect_lit(cs, i, "true", Json::Bool(true)),
        'f' => expect_lit(cs, i, "false", Json::Bool(false)),
        'n' => expect_lit(cs, i, "null", Json::Null),
        _ => parse_number(cs, i),
    }
}

fn expect_lit(cs: &[char], i: &mut usize, lit: &str, val: Json) -> Option<Json> {
    for c in lit.chars() {
        if cs.get(*i) != Some(&c) {
            return None;
        }
        *i += 1;
    }
    Some(val)
}

fn parse_object(cs: &[char], i: &mut usize) -> Option<Json> {
    *i += 1; // {
    let mut map = BTreeMap::new();
    skip_ws(cs, i);
    if cs.get(*i) == Some(&'}') {
        *i += 1;
        return Some(Json::Object(map));
    }
    loop {
        skip_ws(cs, i);
        let key = parse_string(cs, i)?;
        skip_ws(cs, i);
        if cs.get(*i) != Some(&':') {
            return None;
        }
        *i += 1;
        let val = parse_value(cs, i)?;
        map.insert(key, val);
        skip_ws(cs, i);
        match cs.get(*i) {
            Some(',') => {
                *i += 1;
            }
            Some('}') => {
                *i += 1;
                return Some(Json::Object(map));
            }
            _ => return None,
        }
    }
}

fn parse_array(cs: &[char], i: &mut usize) -> Option<Json> {
    *i += 1; // [
    let mut out = Vec::new();
    skip_ws(cs, i);
    if cs.get(*i) == Some(&']') {
        *i += 1;
        return Some(Json::Array(out));
    }
    loop {
        out.push(parse_value(cs, i)?);
        skip_ws(cs, i);
        match cs.get(*i) {
            Some(',') => {
                *i += 1;
            }
            Some(']') => {
                *i += 1;
                return Some(Json::Array(out));
            }
            _ => return None,
        }
    }
}

fn parse_string(cs: &[char], i: &mut usize) -> Option<String> {
    if cs.get(*i) != Some(&'"') {
        return None;
    }
    *i += 1;
    let mut out = String::new();
    while let Some(&c) = cs.get(*i) {
        *i += 1;
        match c {
            '"' => return Some(out),
            '\\' => {
                let esc = *cs.get(*i)?;
                *i += 1;
                match esc {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let hex: String = cs.get(*i..*i + 4)?.iter().collect();
                        *i += 4;
                        let code = u32::from_str_radix(&hex, 16).ok()?;
                        out.push(char::from_u32(code)?);
                    }
                    _ => return None,
                }
            }
            c => out.push(c),
        }
    }
    None
}

fn parse_number(cs: &[char], i: &mut usize) -> Option<Json> {
    let start = *i;
    if cs.get(*i) == Some(&'-') {
        *i += 1;
    }
    while let Some(c) = cs.get(*i) {
        if c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-') {
            *i += 1;
        } else {
            break;
        }
    }
    let s: String = cs[start..*i].iter().collect();
    s.parse::<f64>().ok().map(Json::Num)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_setting_branches() {
        assert!(check_dir_setting("04_RAW原图").is_none());
        assert!(check_dir_setting("99_图库/原图JPG归档").is_none());
        assert!(check_dir_setting("").unwrap().contains("不能为空"));
        assert!(check_dir_setting("   ").unwrap().contains("不能为空"));
        assert!(check_dir_setting("D:\\photos").unwrap().contains("绝对路径"));
        assert!(check_dir_setting("/abs/path").unwrap().contains("绝对路径"));
        assert!(check_dir_setting("a/../b").unwrap().contains(".."));
        assert!(check_dir_setting("a//b").unwrap().contains("空段"));
        assert!(check_dir_setting(".").is_some());
        assert!(check_dir_setting("a*b").unwrap().contains("不能用于文件名"));
        // 冒号不在盘符位置也会被拒（提示语说的是盘符，略不贴切，但拒绝是对的）
        assert!(check_dir_setting("a:b").is_some());
    }

    #[test]
    fn dirs_must_be_distinct() {
        // 默认配置本身合法
        assert!(dirs_conflict_in(&Config::default().dirs).is_none());

        // 原图目录 = 归档目录：archive 会把原图当重复份删掉，必须拦
        let mut d = Config::default().dirs;
        d.insert("arc".into(), "05_JPG图".into());
        let msg = dirs_conflict_in(&d).expect("应判定为冲突");
        assert!(msg.contains("原图目录") && msg.contains("归档目录"), "{msg}");

        // 大小写、分隔符、结尾斜杠都算同一个目录
        let mut d2 = Config::default().dirs;
        d2.insert("jpg".into(), "A/B".into());
        d2.insert("arc".into(), "a\\b\\".into());
        assert!(dirs_conflict_in(&d2).is_some());

        // 换个名字就合法
        let mut d3 = Config::default().dirs;
        d3.insert("arc".into(), "A/B/归档".into());
        assert!(dirs_conflict_in(&d3).is_none());
    }

    #[test]
    fn json_roundtrip() {
        let j = Json::parse(r#"{"root":"J:\\lib","batch":"2026-11-26-x","dirs":{"raw":"底片"}}"#)
            .expect("should parse");
        let obj = match j {
            Json::Object(o) => o,
            _ => panic!("not object"),
        };
        match obj.get("root") {
            Some(Json::Str(s)) => assert_eq!(s, "J:\\lib"),
            other => panic!("{other:?}"),
        }
        match obj.get("dirs") {
            Some(Json::Object(d)) => match d.get("raw") {
                Some(Json::Str(s)) => assert_eq!(s, "底片"),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn broken_json_returns_none() {
        assert!(Json::parse("{ 这不是 json").is_none());
        assert!(Json::parse("").is_none());
    }
}
