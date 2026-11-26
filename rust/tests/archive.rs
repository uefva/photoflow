//! 集成测试：在临时目录里造一个「合成照片库」，跑真实二进制。
//!
//! 用 CLI 而不是库函数，是为了让测试对齐「用户实际怎么用它」——
//! 和 `test_photoflow.py` 的端到端用例同一套判据。
//!
//! 覆盖：P0 侧车闸门、正常归档、多版本只归档一次、降噪中缀、冲突不覆盖、
//! 重复份清理、多格式歧义、单候选跨格式、指纹锁清单、缺失/孤儿报告、
//! 路径穿越拦截，以及分发侧的改名不顶名。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const DIR_RAW: &str = "04_RAW原图";
const DIR_JPG: &str = "05_JPG图";
const DIR_ED: &str = "03_缓存-调色ed";
const DIR_ARC: &str = "99_图库/原图JPG归档";

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_photoflow")
}

fn unique(tag: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("pf-it-{}-{}-{}", tag, std::process::id(), n))
}

struct Lib {
    root: PathBuf,
    config: PathBuf,
}

impl Lib {
    fn new(tag: &str) -> Self {
        let root = unique(tag);
        for d in [DIR_RAW, DIR_JPG, DIR_ED, DIR_ARC] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        let config = root.join("_no_such_config.json");
        Lib { root, config }
    }

    fn batch(&self) -> &str {
        "2026-11-26-测试"
    }

    fn write(&self, dir: &str, name: &str, data: &[u8]) -> PathBuf {
        let p = self.root.join(dir).join(self.batch()).join(name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, data).unwrap();
        p
    }

    fn list(&self, dir: &str) -> Vec<String> {
        list_names(&self.root.join(dir).join(self.batch()))
    }

    fn arc_list(&self) -> Vec<String> {
        list_names(&self.root.join(DIR_ARC).join(self.batch()))
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(bin())
            .args(args)
            .arg("--root")
            .arg(&self.root)
            .env("PHOTOFLOW_CONFIG", &self.config)
            .output()
            .expect("跑不起二进制");
        let mut text = String::from_utf8_lossy(&out.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), text)
    }
}

impl Drop for Lib {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn list_names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = match fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect(),
        Err(_) => Vec::new(),
    };
    v.sort();
    v
}

// ---------------------------------------------------------------- 归档

#[test]
fn p0_nonimage_ed_does_not_archive() {
    // 成品目录里的 .xmp 伴生文件不能让对应原图被归档。
    let lib = Lib::new("p0");
    lib.write(DIR_JPG, "DSC0001.JPG", b"ORIGINAL");
    lib.write(DIR_ED, "DSC0001-ed.xmp", b"sidecar");

    let (code, out) = lib.run(&["archive", lib.batch(), "--apply"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(lib.list(DIR_JPG), vec!["DSC0001.JPG"], "原图必须留在原图目录：{out}");
    assert!(lib.arc_list().is_empty(), "{out}");
}

#[test]
fn basic_archive_moves_original() {
    let lib = Lib::new("basic");
    lib.write(DIR_JPG, "DSC0001.JPG", b"ORIG");
    lib.write(DIR_ED, "DSC0001-ed.jpg", b"EDITED");

    let (code, out) = lib.run(&["archive", lib.batch(), "--apply"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("归档 1 个"), "{out}");
    assert!(lib.list(DIR_JPG).is_empty(), "{out}");
    assert_eq!(lib.arc_list(), vec!["DSC0001.JPG"]);
}

#[test]
fn two_versions_archive_once() {
    let lib = Lib::new("multi");
    lib.write(DIR_JPG, "DSC00596.JPG", b"orig");
    lib.write(DIR_ED, "DSC00596-ed.jpg", b"v1");
    lib.write(DIR_ED, "DSC00596-ed2.jpg", b"v2");

    let (code, out) = lib.run(&["archive", lib.batch(), "--apply"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("多张成品对应同一张原图"), "应报「共享原图」：{out}");
    assert_eq!(lib.arc_list(), vec!["DSC00596.JPG"], "同一原图只归档一次：{out}");
}

#[test]
fn denoise_infix_matches() {
    let lib = Lib::new("denoise");
    lib.write(DIR_JPG, "DSC07287.JPG", b"orig");
    lib.write(DIR_ED, "DSC07287-已增强-降噪-ed.JPG", b"edited");
    lib.write(DIR_JPG, "DSC07332.JPG", b"orig2");
    lib.write(DIR_ED, "DSC07332-Enhanced-NR-ed.jpg", b"edited2");

    let (code, out) = lib.run(&["archive", lib.batch(), "--apply"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        lib.arc_list(),
        vec!["DSC07287.JPG", "DSC07332.JPG"],
        "中缀应被剥掉后匹配到原图：{out}"
    );
}

#[test]
fn conflict_when_arc_has_same_name_different_content() {
    let lib = Lib::new("conflict");
    lib.write(DIR_JPG, "DSC0002.jpg", b"original-from-card");
    lib.write(DIR_ARC, "DSC0002.jpg", b"totally-different");
    lib.write(DIR_ED, "DSC0002-ed.jpg", b"edited");

    let (code, out) = lib.run(&["archive", lib.batch(), "--apply"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("内容冲突"), "应报内容冲突：{out}");

    let arc = lib.root.join(DIR_ARC).join(lib.batch()).join("DSC0002.jpg");
    assert_eq!(fs::read(&arc).unwrap(), b"totally-different", "归档里那份不能被覆盖");
    assert_eq!(lib.list(DIR_JPG), vec!["DSC0002.jpg"], "原图仍应在原图目录");
}

#[test]
fn redundant_copy_in_jpg_is_removed() {
    let lib = Lib::new("redundant");
    lib.write(DIR_ARC, "DSC0003.jpg", b"SAME");
    lib.write(DIR_JPG, "DSC0003.jpg", b"SAME");
    lib.write(DIR_ED, "DSC0003-ed.jpg", b"edited");

    let (code, out) = lib.run(&["archive", lib.batch(), "--apply"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("重复份已删除"), "应报删掉重复份：{out}");
    assert!(lib.list(DIR_JPG).is_empty(), "重复份应被删除：{out}");
    assert_eq!(lib.arc_list(), vec!["DSC0003.jpg"]);
}

#[test]
fn ambiguous_multi_format_no_move() {
    // 05 有 .HEIC + .PNG，03 是 -ed.jpg：扩展名都对不上且有多个候选 → 不自动移动
    let lib = Lib::new("ambig");
    lib.write(DIR_JPG, "DSC0001.HEIC", b"heic");
    lib.write(DIR_JPG, "DSC0001.PNG", b"png");
    lib.write(DIR_ED, "DSC0001-ed.jpg", b"edited");

    let (code, out) = lib.run(&["archive", lib.batch(), "--apply"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("匹配不唯一"), "{out}");
    assert!(lib.arc_list().is_empty(), "多个候选时不能动手：{out}");
}

#[test]
fn single_cross_format_candidate_is_archived_with_note() {
    // 05 只有 .HEIC，03 是 -ed.jpg：单候选跨扩展名，放行并标注
    let lib = Lib::new("crossfmt");
    lib.write(DIR_JPG, "DSC0002.HEIC", b"heic");
    lib.write(DIR_ED, "DSC0002-ed.jpg", b"edited");

    let (code, out) = lib.run(&["archive", lib.batch(), "--apply"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("扩展名不同"), "应标注扩展名不同：{out}");
    assert_eq!(lib.arc_list(), vec!["DSC0002.HEIC"]);
}

#[test]
fn expect_plan_blocks_when_changed() {
    let lib = Lib::new("plan");
    lib.write(DIR_JPG, "IMG_0001.JPG", b"o1");
    lib.write(DIR_ED, "IMG_0001-ed.JPG", b"e1");
    lib.write(DIR_JPG, "IMG_0002.JPG", b"o2");
    lib.write(DIR_ED, "IMG_0002-ed.JPG", b"e2");

    let (code, preview) = lib.run(&["archive", lib.batch(), "--progress"]);
    assert_eq!(code, 0, "{preview}");
    let token = preview
        .lines()
        .find_map(|l| l.strip_prefix("@@PLAN@@\t"))
        .expect("预演应输出计划指纹")
        .trim()
        .to_string();

    // 预演之后又来一张
    lib.write(DIR_JPG, "IMG_0003.JPG", b"o3");
    lib.write(DIR_ED, "IMG_0003-ed.JPG", b"e3");

    let (code, out) = lib.run(&["archive", lib.batch(), "--apply", "--expect-plan", &token]);
    assert_ne!(code, 0, "清单变了却照跑了：{out}");
    assert!(out.contains("计划已经变了"), "{out}");
    assert!(lib.arc_list().is_empty(), "拒绝执行时不能动文件：{out}");

    // 重新预演拿新指纹后放行
    let (_, preview2) = lib.run(&["archive", lib.batch(), "--progress"]);
    let token2 = preview2
        .lines()
        .find_map(|l| l.strip_prefix("@@PLAN@@\t"))
        .expect("重新预演应输出指纹")
        .trim()
        .to_string();
    let (code, out) = lib.run(&["archive", lib.batch(), "--apply", "--expect-plan", &token2]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(lib.arc_list().len(), 3, "{out}");
}

#[test]
fn missing_and_orphan_reported() {
    let lib = Lib::new("misc");
    lib.write(DIR_ED, "NOTHING-ed.jpg", b"e"); // 没有对应原图
    lib.write(DIR_ED, "plain.jpg", b"e"); // 没有 -ed

    let (code, out) = lib.run(&["archive", lib.batch()]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("找不到对应原图"), "{out}");
    assert!(out.contains("没有 -ed 后缀的文件"), "{out}");
}

#[test]
fn batch_name_path_traversal_rejected() {
    let lib = Lib::new("evil");
    let (code, out) = lib.run(&["archive", "../escape"]);
    assert_ne!(code, 0, "批次名带 .. 必须被拒绝：{out}");
    assert!(out.contains("批次名不安全"), "{out}");
}

// ---------------------------------------------------------------- 分发

#[test]
fn ingest_dedupe_does_not_clobber_card_name() {
    // 卡里本来就有 DSC1_2.jpg，改名出来的 _2 不能把它顶掉
    let lib = Lib::new("dedupe");
    let card = lib.root.join("card");
    fs::create_dir_all(card.join("100CANON")).unwrap();
    fs::create_dir_all(card.join("101CANON")).unwrap();
    fs::write(card.join("100CANON/DSC1.jpg"), vec![b'A'; 100]).unwrap();
    fs::write(card.join("101CANON/DSC1.jpg"), vec![b'B'; 200]).unwrap();
    fs::write(card.join("101CANON/DSC1_2.jpg"), vec![b'C'; 300]).unwrap();

    let (code, out) = lib.run(&["ingest", card.to_str().unwrap(), lib.batch(), "--dedupe", "--apply"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(lib.list(DIR_JPG).len(), 3, "三张都要落盘：{out}");
    assert!(out.contains("JPG 3 个"), "{out}");
}

#[test]
fn ingest_leaves_no_temp_file() {
    let lib = Lib::new("clean");
    let card = lib.root.join("card");
    fs::create_dir_all(&card).unwrap();
    fs::write(card.join("IMG_0001.JPG"), vec![b'A'; 1000]).unwrap();

    let (code, out) = lib.run(&["ingest", card.to_str().unwrap(), lib.batch(), "--apply", "--verify"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(lib.list(DIR_JPG), vec!["IMG_0001.JPG"], "{out}");

    let leftovers: Vec<String> = fs::read_dir(lib.root.join(DIR_JPG).join(lib.batch()))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".phototmp"))
        .collect();
    assert!(leftovers.is_empty(), "不应留下临时文件：{leftovers:?}");
}

// ---------------------------------------------------------------- 配置安全

#[test]
fn archive_refuses_when_original_and_archive_dirs_are_the_same() {
    // 配置里 jpg 和 arc 指向同一个目录：归档会把原图当「已归档的重复份」删掉。
    // 必须在动手之前就拒绝，原图一个都不能少。
    let lib = Lib::new("same-dir");
    fs::write(
        &lib.config,
        r#"{"dirs":{"jpg":"05_JPG图","arc":"05_JPG图"}}"#,
    )
    .unwrap();
    let orig = lib.write(DIR_JPG, "DSC0001.JPG", b"ORIGINAL-JPEG");
    lib.write(DIR_ED, "DSC0001-ed.jpg", b"EDITED");

    let (code, out) = lib.run(&["archive", lib.batch(), "--apply"]);
    assert_eq!(code, 2, "目录撞在一起时必须拒绝执行：{out}");
    assert!(out.contains("同一个位置"), "{out}");
    assert!(orig.exists(), "原图不能被删掉：{out}");
}

#[test]
fn ingest_also_refuses_duplicate_dirs() {
    // 会写文件的命令都走同一道检查，不是只堵 archive
    let lib = Lib::new("dup-ingest");
    fs::write(
        &lib.config,
        r#"{"dirs":{"raw":"05_JPG图","jpg":"05_JPG图"}}"#,
    )
    .unwrap();
    let card = lib.root.join("card");
    fs::create_dir_all(&card).unwrap();
    fs::write(card.join("IMG_0001.JPG"), vec![b'A'; 10]).unwrap();

    let (code, out) = lib.run(&["ingest", card.to_str().unwrap(), lib.batch(), "--apply"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("同一个位置"), "{out}");
}
