//! `ingest`：把卡里的照片按扩展名分发到 RAW 目录和原图目录。
//!
//! 这是全工具最容易丢片的一条路径，所以判定写得很啰嗦：
//!   - 目标已存在 → 同内容跳过；不同内容先看是不是上次 `--dedupe` 存成 _2/_3 的那份，
//!     再不然就报冲突（不覆盖）；
//!   - 卡内同名多张 → 和「该文件名的所有已知版本」逐一比哈希，只给真正不同的内容加后缀；
//!   - 改名要绕开卡里本来就有的文件名（否则会把人家顶掉，报告还说全成功）；
//!   - 计划收尾再验一次目标名唯一性；执行前再查一次目标是否被占；
//!   - 复制一律「临时文件 → 校验 → 落位」，中途出事不留半截照片。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::fsops::{copy_verified, same_content};
use crate::naming::{
    self, batch_dir, check_batch_name, ext_lower, human, is_jpg_ext, is_raw_ext, name_raw,
};
use crate::{plan, progress, ui};

pub struct Placed {
    pub src: PathBuf,
    pub dst: PathBuf,
}

#[derive(Default)]
struct IngestPlan {
    raw: Vec<Placed>,
    jpg: Vec<Placed>,
    other: Vec<PathBuf>,
    skip: Vec<PathBuf>,
    conflict: Vec<(PathBuf, PathBuf, String)>,
    dup_same: Vec<(PathBuf, PathBuf)>,
    dup_diff: Vec<(PathBuf, PathBuf)>,
    renamed: Vec<(PathBuf, PathBuf)>,
    target_clash: Vec<(PathBuf, PathBuf, PathBuf)>,
}

type NameKey = (String, String);

fn key(kind: &str, name: &str) -> NameKey {
    (kind.to_string(), name.to_lowercase())
}

/// 卡内同名但内容不同：加 _2/_3 后缀。
/// 三重回避：目标已存在、本批已经分配过、卡里本来就叫这个名字。
fn alt_name(
    kind: &str,
    dst_dir: &Path,
    f: &Path,
    used: &HashSet<NameKey>,
    reserved: &HashSet<NameKey>,
) -> PathBuf {
    let stem = f
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = f
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();

    let mut n = 2;
    loop {
        let name = format!("{stem}_{n}{ext}");
        let cand = dst_dir.join(&name);
        let k = key(kind, &name);
        let is_self = name.to_lowercase() == name_raw(f).to_lowercase();
        if !cand.exists() && !used.contains(&k) && (!reserved.contains(&k) || is_self) {
            return cand;
        }
        n += 1;
    }
}

/// 上一次 `--dedupe` 把这张存成了 DSC1_2.jpg，这次要认出来，
/// 否则每次重跑都报「冲突」，逼着人一个个看。
fn find_twin(dst_dir: &Path, f: &Path) -> Option<PathBuf> {
    let stem = f
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = f
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();

    let mut n = 2;
    loop {
        let cand = dst_dir.join(format!("{stem}_{n}{ext}"));
        if !cand.exists() {
            return None;
        }
        if same_content(f, &cand) {
            return Some(cand);
        }
        n += 1;
    }
}

fn file_size(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

pub fn run(
    root: &Path,
    src: &Path,
    batch: &str,
    apply: bool,
    dedupe: bool,
    verify: bool,
    expect_plan: Option<&str>,
) -> Result<(), String> {
    if !src.is_dir() {
        return Err(format!("源目录不存在：{}", src.display()));
    }
    let d = naming::check_root(root)?;

    if let Err(hint) = check_batch_name(batch) {
        println!("{}批次名不合规：{hint}", ui::C_WARN);
    }

    let dst_raw = batch_dir(&d.raw, batch)?;
    let dst_jpg = batch_dir(&d.jpg, batch)?;

    let files = naming::iter_files(src);

    // 卡里所有会被导入的文件名，先收齐。改名时必须绕开它们：
    // 卡里本来就有一张 DSC1_2.jpg 的时候，改名改出来的 DSC1_2.jpg 会把它顶掉。
    let mut reserved: HashSet<NameKey> = HashSet::new();
    for f in &files {
        let ext = ext_lower(f);
        if is_raw_ext(&ext) {
            reserved.insert(key("raw", &name_raw(f)));
        } else if is_jpg_ext(&ext) {
            reserved.insert(key("jpg", &name_raw(f)));
        }
    }

    let mut p = IngestPlan::default();
    let mut families: HashMap<NameKey, Vec<(PathBuf, PathBuf)>> = HashMap::new();
    let mut used: HashSet<NameKey> = HashSet::new();

    progress::progress(0, files.len(), "扫描");
    for (idx, f) in files.iter().enumerate() {
        // 重复分发时最慢的是这里（同名文件要两头各算一遍哈希），进度放在最前面
        progress::progress(idx + 1, files.len(), "扫描");

        let ext = ext_lower(f);
        let (kind, dst_dir) = if is_raw_ext(&ext) {
            ("raw", &dst_raw)
        } else if is_jpg_ext(&ext) {
            ("jpg", &dst_jpg)
        } else {
            p.other.push(f.clone());
            continue;
        };

        let k = key(kind, &name_raw(f));
        let dst = dst_dir.join(name_raw(f));
        used.insert(key(kind, &name_raw(f)));

        if dst.exists() {
            if same_content(f, &dst) {
                p.skip.push(dst);
                continue;
            }
            if let Some(twin) = find_twin(dst_dir, f) {
                p.dup_same.push((f.clone(), twin));
                continue;
            }
            let same_size = match (std::fs::metadata(&dst), std::fs::metadata(f)) {
                (Ok(a), Ok(b)) => a.len() == b.len(),
                _ => false,
            };
            p.conflict.push((
                f.clone(),
                dst,
                if same_size {
                    "大小相同但内容不同".to_string()
                } else {
                    "大小不同".to_string()
                },
            ));
            continue;
        }

        // 跟这个文件名下的每一个已知版本逐一比对，而不是只跟第一个比。
        // 否则 a=AAAA、b=BBBB、c=BBBB 会把 c 也当成新照片，多出 IMG_3。
        let matched = families
            .get(&k)
            .and_then(|vs| vs.iter().find(|(s, _)| same_content(f, s)).cloned());
        if let Some((first, _)) = matched {
            p.dup_same.push((f.clone(), first));
            continue;
        }

        let has_versions = families.get(&k).map(|v| !v.is_empty()).unwrap_or(false);
        if has_versions && !dedupe {
            let first = families[&k][0].0.clone();
            p.dup_diff.push((f.clone(), first));
            continue;
        }

        let target = if !has_versions {
            dst
        } else {
            let t = alt_name(kind, dst_dir, f, &used, &reserved);
            used.insert(key(kind, &name_raw(&t)));
            p.renamed.push((f.clone(), t.clone()));
            t
        };

        families
            .entry(k)
            .or_default()
            .push((f.clone(), target.clone()));
        if kind == "raw" {
            p.raw.push(Placed { src: f.clone(), dst: target });
        } else {
            p.jpg.push(Placed { src: f.clone(), dst: target });
        }
    }

    // 收尾再验一遍：计划里不能有两个源文件落到同一个目标名上。
    // 真出现了就是会静默覆盖丢片，宁可拦下来报冲突。
    let mut seen: HashMap<NameKey, PathBuf> = HashMap::new();
    let mut clashes: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new();
    let mut raw_keep: Vec<Placed> = Vec::new();
    for placed in std::mem::take(&mut p.raw) {
        let k = key("raw", &name_raw(&placed.dst));
        if let Some(first) = seen.get(&k) {
            clashes.push((placed.src, placed.dst, first.clone()));
        } else {
            seen.insert(k, placed.src.clone());
            raw_keep.push(placed);
        }
    }
    let mut jpg_keep: Vec<Placed> = Vec::new();
    for placed in std::mem::take(&mut p.jpg) {
        let k = key("jpg", &name_raw(&placed.dst));
        if let Some(first) = seen.get(&k) {
            clashes.push((placed.src, placed.dst, first.clone()));
        } else {
            seen.insert(k, placed.src.clone());
            jpg_keep.push(placed);
        }
    }
    p.raw = raw_keep;
    p.jpg = jpg_keep;
    p.target_clash = clashes;

    let total_bytes: u64 = p.raw.iter().chain(p.jpg.iter()).map(|x| file_size(&x.src)).sum();

    // 清单 = 真正要复制的那些 (类型, 源, 目标, 大小, 修改时间)
    let rows: Vec<Vec<String>> = ["raw", "jpg"]
        .iter()
        .flat_map(|kind| {
            let list = if *kind == "raw" { &p.raw } else { &p.jpg };
            list.iter().map(move |x| {
                let (size, mt) = plan::stat_sig(&x.src);
                vec![
                    kind.to_string(),
                    x.src.display().to_string(),
                    x.dst.display().to_string(),
                    size.map(|v| v.to_string()).unwrap_or_default(),
                    mt.map(|v| v.to_string()).unwrap_or_default(),
                ]
            })
        })
        .collect();
    let token = plan::token(&rows);
    plan::check_plan(expect_plan, &token)?;
    progress::announce_plan(&token);

    ui::confirm_apply(
        apply,
        &format!("从 {} 分发到批次 {}", src.display(), batch),
    );
    println!("RAW  待复制 {:>4} 个", p.raw.len());
    println!("JPG  待复制 {:>4} 个", p.jpg.len());
    println!("已存在跳过 {:>4} 个", p.skip.len());
    println!("合计待复制 {}", human(total_bytes));
    if let Some(first) = p.other.first() {
        println!(
            "{}非 RAW/JPG 跳过 {} 个（如 {}）",
            ui::C_INFO,
            p.other.len(),
            name_raw(first)
        );
    }

    if !p.dup_same.is_empty() {
        println!(
            "{}卡内同名且内容相同 {} 个 —— 已校验哈希，跳过不重复导入",
            ui::C_INFO,
            p.dup_same.len()
        );
    }
    if !p.renamed.is_empty() {
        println!(
            "{}卡内同名但内容不同 {} 个 —— 已改用 _2/_3 后缀，避免丢片：",
            ui::C_WARN,
            p.renamed.len()
        );
        for (s, d) in p.renamed.iter().take(20) {
            println!("       {}  ->  {}", s.display(), name_raw(d));
        }
    }
    if !p.dup_diff.is_empty() {
        println!(
            "\n{}冲突：卡内同名但内容不同 {} 个 —— 判定为不同照片，未复制：",
            ui::C_WARN,
            p.dup_diff.len()
        );
        for (s, first) in p.dup_diff.iter().take(20) {
            println!("       {}  ({})", s.display(), human(file_size(s)));
            println!("       与 {}  ({}) 撞名", first.display(), human(file_size(first)));
        }
        println!("       加 --dedupe 自动加 _2 后缀；或手动改名后重跑。");
    }

    if !p.conflict.is_empty() {
        println!(
            "\n{}冲突：目标已存在但内容不同 {} 个 —— 不覆盖，需你人工判断：",
            ui::C_WARN,
            p.conflict.len()
        );
        for (s, d, reason) in p.conflict.iter().take(20) {
            println!("       源   {}  ({})", s.display(), human(file_size(s)));
            println!("       已有 {}  ({}, {})", d.display(), human(file_size(d)), reason);
        }
    }

    if !p.target_clash.is_empty() {
        println!(
            "\n{}两个源文件算出了同一个目标名 {} 个 —— 已拦下，不复制：",
            ui::C_WARN,
            p.target_clash.len()
        );
        for (s, d, first) in p.target_clash.iter().take(20) {
            println!("       {}", s.display());
            println!("       会和 {} 一起落到 {}", first.display(), name_raw(d));
        }
        println!("       这是脚本分配文件名时出的错，不是你的问题。加 --dedupe 重跑，或先手动改名。");
    }

    if !apply {
        println!("\n加 --apply 真正复制。想锁定这份清单，就加 --expect-plan {token}");
        return Ok(());
    }

    let mut errors = 0usize;
    let mut raced = 0usize;
    let mut copied_raw = 0usize;
    let mut copied_jpg = 0usize;
    let plan_total = p.raw.len() + p.jpg.len();
    let mut done = 0usize;

    progress::progress(0, plan_total, "复制");
    for (kind, label, dst_dir, list) in [
        ("raw", "RAW", &dst_raw, &p.raw),
        ("jpg", "JPG", &dst_jpg, &p.jpg),
    ] {
        std::fs::create_dir_all(dst_dir)
            .map_err(|e| format!("建目录失败 {}：{e}", dst_dir.display()))?;
        for (i, placed) in list.iter().enumerate() {
            done += 1;
            // 计划之后、落盘之前，目标位置被别的程序占了：报出来，绝不覆盖
            if placed.dst.exists() {
                raced += 1;
                println!(
                    "{}{}: 目标位置在执行期间被占用，跳过（不覆盖）",
                    ui::C_ERR,
                    name_raw(&placed.src)
                );
                progress::progress(done, plan_total, "复制");
                continue;
            }
            match copy_verified(&placed.src, &placed.dst, verify) {
                Ok(()) => {
                    if kind == "raw" {
                        copied_raw += 1;
                    } else {
                        copied_jpg += 1;
                    }
                    if (i + 1) % 50 == 0 {
                        println!("       {} {}/{} ...", label, i + 1, list.len());
                    }
                }
                Err(e) => {
                    errors += 1;
                    println!("{}{}: {}", ui::C_ERR, name_raw(&placed.src), e);
                }
            }
            progress::progress(done, plan_total, "复制");
        }
    }

    println!("\n完成：RAW {copied_raw} 个，JPG {copied_jpg} 个，失败 {errors} 个。");
    if raced > 0 {
        println!("（另有 {raced} 个的目标位置在执行期间被别的程序占了，已跳过，未覆盖）");
    }
    if errors > 0 {
        println!("失败项未复制，请检查网络连接后重跑（已复制的会自动跳过）。");
    }
    Ok(())
}
