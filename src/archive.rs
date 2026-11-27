//! `archive`：把已修照片的原图从原图目录移进归档目录。
//!
//! 对应 `photoflow.py` 的 `cmd_archive`，判定逐条复刻：
//!   - 成品多重后缀（-ed / -ed2 / -ed-2 / -ed-Edit / 降噪中缀）逐层剥壳；
//!   - 同名多候选判歧义、跨扩展名只在「单候选」时放行并标注；
//!   - 一张原图只被一张成品认领（多版本只归档一次）；
//!   - 归档目录已有同内容 → 删原图目录那份重复的；同名不同内容 → 冲突不覆盖；
//!   - 同一张照片的其它格式留在原图目录里，只报不动；
//!   - 预演指纹（路径/大小/mtime）锁住清单。
//!
//! 比原版多做的一道闸门：成品扩展名必须是图片。`.xmp` / `.aae` / `.dop` 这类
//! 伴生文件如果带了 `-ed`，原版会把它当成品、进而把真实原图归档掉；这里只当侧车列出。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::fsops::{move_verified, same_content, MoveKind};
use crate::naming::{
    self, batch_dir, ext_lower, human, iter_files, name_raw, stem_lower, stem_raw, strip_ed,
};
use crate::{plan, progress, ui};

/// 允许当作「成品」的扩展名。故意不含 xmp / aae / dop / on1 / acr / cos
/// 这些伴生文件后缀——它们冒充成品会把真实原图误归档。
const IMAGE_EXT: &[&str] = &[
    "jpg", "jpeg", "jpe", "png", "heic", "heif", "tif", "tiff", "webp", "avif", "dng", "psd",
    "psb",
];

fn is_image_ext(ext: &str) -> bool {
    IMAGE_EXT.contains(&ext)
}

pub struct MovedItem {
    pub ed: PathBuf,
    pub orig: PathBuf,
    pub dest: PathBuf,
    pub note: String,
}

#[derive(Default)]
struct ArchivePlan {
    batch: String,
    arc_dir: PathBuf,
    log_dir: PathBuf,

    moved: Vec<MovedItem>,
    already: usize,
    missing: Vec<(PathBuf, String)>,
    orphan: Vec<PathBuf>,
    sidecars: Vec<PathBuf>,
    ambiguous: Vec<(PathBuf, Vec<PathBuf>, String)>,
    shared: Vec<(PathBuf, PathBuf, PathBuf)>,
    conflicts: Vec<(PathBuf, PathBuf, PathBuf)>,
    archived_other: Vec<(PathBuf, String, Vec<String>)>,
    siblings_left: Vec<(PathBuf, PathBuf)>,
    redundant: Vec<PathBuf>,
    untouched: usize,
    /// 视频目录里这个批次的视频数。只统计，不参与归档、不移动。
    videos: usize,
    token: String,
}

/// 归档日志里记过的原图文件名。用来区分两种「99 里有个同名不同格式的文件」：
/// 一种是我们自己上一趟移过去的，另一种来路不明——后者仍要按「找不到原图」报警。
fn logged_names(log_dir: &Path) -> HashSet<String> {
    let mut names = HashSet::new();
    let rd = match std::fs::read_dir(log_dir) {
        Ok(rd) => rd,
        Err(_) => return names,
    };
    for ent in rd.flatten() {
        let path = ent.path();
        let fname = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if !(fname.starts_with("archive-") && fname.ends_with(".tsv")) {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                let bits: Vec<&str> = line.split('\t').collect();
                if bits.len() >= 3 {
                    names.insert(bits[2].to_lowercase());
                }
            }
        }
    }
    names
}

fn build_plan(root: &Path, batch: &str) -> Result<ArchivePlan, String> {
    let d = naming::check_root(root)?;
    let ed_dir = batch_dir(&d.ed, batch)?;
    let jpg_dir = batch_dir(&d.jpg, batch)?;
    let arc_dir = batch_dir(&d.arc, batch)?;
    let vid_dir = batch_dir(&d.vid, batch)?;

    // 兜底：即便配置校验被绕过（目录被符号链接 / junction 别名到一处），也绝不能让
    // 「归档」退化成「把原图当重复份删掉」。
    if naming::same_path(&jpg_dir, &arc_dir) {
        return Err(format!(
            "归档目录和原图目录指向同一个位置：{}\n\
             这样归档会把原图当成「归档里已有的重复份」删掉。\n\
             请把这两个目录改成两个不同的目录再运行。",
            jpg_dir.display()
        ));
    }

    if !ed_dir.is_dir() {
        return Err(format!(
            "找不到成品目录：{}\n（这个批次还没有修好的照片。先在 Lightroom 里导出，\
             文件名带 -ed，放进这个目录之后再归档。）",
            ed_dir.display()
        ));
    }

    // 原图索引：主名 -> 扩展名 -> [文件]。
    // 只收照片类文件：视频既不是成品也不是原图，混进来会被当成「另一种格式的原图」搬走。
    let mut index: HashMap<String, HashMap<String, Vec<PathBuf>>> = HashMap::new();
    if jpg_dir.is_dir() {
        for f in iter_files(&jpg_dir) {
            if !naming::is_photo_ext(&ext_lower(&f)) {
                continue;
            }
            index
                .entry(stem_lower(&f))
                .or_default()
                .entry(ext_lower(&f))
                .or_default()
                .push(f);
        }
    }

    // 归档索引：按「主名+扩展名」区分；另按主名收一份，用于「已归档但换了格式」
    let mut arc_index: HashSet<(String, String)> = HashSet::new();
    let mut arc_by_stem: HashMap<String, HashSet<String>> = HashMap::new();
    if arc_dir.is_dir() {
        for f in iter_files(&arc_dir) {
            let s = stem_lower(&f);
            let e = ext_lower(&f);
            arc_index.insert((s.clone(), e.clone()));
            // 这里带点存（.heic），因为要和日志里的文件名拼起来比对，
            // 也是打印「归档里是 IMG_0001.heic」时需要的形状。
            arc_by_stem
                .entry(s)
                .or_default()
                .insert(format!(".{}", e));
        }
    }
    let logged = logged_names(&d.logs);

    let mut p = ArchivePlan {
        batch: batch.to_string(),
        arc_dir: arc_dir.clone(),
        log_dir: d.logs.clone(),
        ..Default::default()
    };

    let mut referenced: HashSet<PathBuf> = HashSet::new();
    let mut claimed: HashMap<PathBuf, PathBuf> = HashMap::new();

    let ed_files = iter_files(&ed_dir);
    progress::progress(0, ed_files.len(), "匹配");
    for (i, f) in ed_files.iter().enumerate() {
        progress::progress(i + 1, ed_files.len(), "匹配");

        let base = match strip_ed(&stem_raw(f)) {
            Some(b) => b,
            None => {
                p.orphan.push(f.clone());
                continue;
            }
        };
        let ext = ext_lower(f);

        // 闸门：带 -ed 但不是图片 → 侧车，不触发归档
        if !is_image_ext(&ext) {
            p.sidecars.push(f.clone());
            continue;
        }

        let base_low = base.to_lowercase();
        let groups = index.get(&base_low);

        let cand: Vec<PathBuf> = match groups {
            Some(g) => {
                if let Some(v) = g.get(&ext) {
                    v.clone()
                } else {
                    let others: Vec<PathBuf> = g.values().flatten().cloned().collect();
                    if others.len() > 1 {
                        for o in &others {
                            referenced.insert(o.clone());
                        }
                        p.ambiguous.push((
                            f.clone(),
                            others,
                            "扩展名不一致，且有多个同名候选".into(),
                        ));
                        continue;
                    }
                    others
                }
            }
            None => Vec::new(),
        };

        if cand.len() > 1 {
            for c in &cand {
                referenced.insert(c.clone());
            }
            p.ambiguous
                .push((f.clone(), cand, "同名同扩展名，但原图目录里有多个文件".into()));
            continue;
        }

        if cand.is_empty() {
            if arc_index.contains(&(base_low.clone(), ext.clone())) {
                p.already += 1;
            } else if let Some(exts) = arc_by_stem.get(&base_low) {
                let any_logged = exts
                    .iter()
                    .any(|e| logged.contains(&format!("{}{}", base_low, e)));
                if any_logged {
                    let mut v: Vec<String> = exts.iter().cloned().collect();
                    v.sort();
                    p.archived_other.push((f.clone(), base.clone(), v));
                } else {
                    p.missing.push((f.clone(), base.clone()));
                }
            } else {
                p.missing.push((f.clone(), base.clone()));
            }
            continue;
        }

        let h = cand[0].clone();
        referenced.insert(h.clone());

        // 同一张照片可能同时有 .jpg 和 .heic 两份原图。按「匹配不唯一就不动手」的规矩，
        // 这里只报出来，不自动搬；但要标成「被指向过」，免得被算进「未修」。
        if let Some(g) = groups {
            for files in g.values() {
                for sib in files {
                    if *sib != h {
                        referenced.insert(sib.clone());
                        if !p.siblings_left.iter().any(|(_, s)| s == sib) {
                            p.siblings_left.push((f.clone(), sib.clone()));
                        }
                    }
                }
            }
        }

        if let Some(first) = claimed.get(&h) {
            p.shared.push((f.clone(), h.clone(), first.clone()));
            continue;
        }

        let dest = arc_dir.join(name_raw(&h));
        if dest.exists() {
            // 已经归档过：内容一致才算完成，不一致说明 99 里那份不是这张原图
            if same_content(&h, &dest) {
                claimed.insert(h.clone(), f.clone());
                p.already += 1;
                p.redundant.push(h);
            } else {
                p.conflicts.push((f.clone(), h, dest));
            }
            continue;
        }

        claimed.insert(h.clone(), f.clone());
        let note = if ext_lower(&h) == ext {
            String::new()
        } else {
            format!("扩展名不同（原图 .{} / 成品 .{}）", ext_lower(&h), ext)
        };
        p.moved.push(MovedItem {
            ed: f.clone(),
            orig: h,
            dest,
            note,
        });
    }

    // 视频目录里这个批次的视频：只报数，留在原地。
    if vid_dir.is_dir() {
        p.videos = iter_files(&vid_dir)
            .into_iter()
            .filter(|f| naming::is_video_ext(&ext_lower(f)))
            .count();
    }

    // 原图目录里没被任何成品指向的（只数照片；视频不属于照片流程）
    if jpg_dir.is_dir() {
        p.untouched = iter_files(&jpg_dir)
            .into_iter()
            .filter(|f| naming::is_photo_ext(&ext_lower(f)))
            .filter(|f| !referenced.contains(f))
            .count();
    }

    // 清单 = 真正要移动的那些 (源, 目标, 大小, 修改时间)
    let rows: Vec<Vec<String>> = p
        .moved
        .iter()
        .map(|m| {
            let (size, mt) = plan::stat_sig(&m.orig);
            vec![
                m.orig.display().to_string(),
                m.dest.display().to_string(),
                size.map(|v| v.to_string()).unwrap_or_default(),
                mt.map(|v| v.to_string()).unwrap_or_default(),
            ]
        })
        .collect();
    p.token = plan::token(&rows);
    Ok(p)
}

fn file_size(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

pub fn run(root: &Path, batch: &str, apply: bool, expect_plan: Option<&str>) -> Result<(), String> {
    let p = build_plan(root, batch)?;
    plan::check_plan(expect_plan, &p.token)?;
    progress::announce_plan(&p.token);

    ui::confirm_apply(apply, &format!("归档批次 {batch} 的原图 JPG"));
    println!("待归档 {:>4} 个", p.moved.len());
    println!("原图目录保留（未修） {:>4} 个", p.untouched);
    if p.videos > 0 {
        println!("视频目录保留 {:>4} 个（不参与归档，留在原地）", p.videos);
    }

    if p.already > 0 {
        println!(
            "{}已在归档中 {} 个（重复跑命令不会出错）",
            ui::C_INFO,
            p.already
        );
    }
    if !p.redundant.is_empty() {
        println!(
            "{}归档目录里已经有同内容的一份 {} 个 —— 执行时会删掉原图目录里的重复份：",
            ui::C_INFO,
            p.redundant.len()
        );
        for h in p.redundant.iter().take(20) {
            println!("       {}", name_raw(h));
        }
    }
    if !p.archived_other.is_empty() {
        println!(
            "\n{}原图目录里已经没有了，归档目录里有一份同名的其它格式 {} 个（日志里记着是之前归档过去的）：",
            ui::C_INFO,
            p.archived_other.len()
        );
        for (f, base, exts) in p.archived_other.iter().take(20) {
            println!(
                "       {}  -> 归档里是 {}{}",
                name_raw(f),
                base,
                exts.join("、")
            );
        }
    }
    if !p.siblings_left.is_empty() {
        println!(
            "\n{}同一张照片在原图目录里还有其它格式的原图 {} 个 —— 没动，需你确认：",
            ui::C_WARN,
            p.siblings_left.len()
        );
        for (f, sib) in p.siblings_left.iter().take(20) {
            println!(
                "       {}（成品 {} 对应的那张已经归档，这份还在原图目录里）",
                name_raw(sib),
                name_raw(f)
            );
        }
        println!("       是同一张照片的另一种格式（比如 HEIC 原图 + JPG 成品）就手动挪进归档目录；");
        println!("       是有意留着的导出件就不用管。脚本不替你判断。");
    }
    if !p.shared.is_empty() {
        println!(
            "{}多张成品对应同一张原图 {} 组 —— 只归档一次：",
            ui::C_INFO,
            p.shared.len()
        );
        for (f, h, first) in p.shared.iter().take(20) {
            println!(
                "       {} 与 {} 都指向 {}",
                name_raw(f),
                name_raw(first),
                name_raw(h)
            );
        }
    }
    if !p.ambiguous.is_empty() {
        println!(
            "\n{}匹配不唯一 {} 个 —— 不自动移动，需你人工确认：",
            ui::C_WARN,
            p.ambiguous.len()
        );
        for (f, cand, why) in p.ambiguous.iter().take(20) {
            println!("       {}：{}", name_raw(f), why);
            for c in cand.iter().take(5) {
                println!("           候选 {}", c.display());
            }
        }
    }
    if !p.conflicts.is_empty() {
        println!(
            "\n{}冲突：归档目录里已有同名但内容不同的文件 {} 个 —— 不覆盖：",
            ui::C_WARN,
            p.conflicts.len()
        );
        for (_, h, d) in p.conflicts.iter().take(20) {
            println!("       原图 {}", h.display());
            println!("       归档 {}  （内容不同）", d.display());
        }
    }
    if !p.missing.is_empty() {
        println!(
            "\n{}有成品，但原图目录和归档目录里都找不到对应原图 {} 个 —— 需你人工确认：",
            ui::C_WARN,
            p.missing.len()
        );
        for (f, base) in p.missing.iter().take(30) {
            println!("       {}  -> 找不到 {}.*", name_raw(f), base);
        }
    }
    if !p.orphan.is_empty() {
        println!(
            "\n{}成品目录里没有 -ed 后缀的文件 {} 个 —— 不在归档判定范围内：",
            ui::C_WARN,
            p.orphan.len()
        );
        for f in p.orphan.iter().take(30) {
            println!("       {}", f.display());
        }
    }
    if !p.sidecars.is_empty() {
        println!(
            "\n{}成品目录里带 -ed 但不是图片的文件 {} 个 —— 不会触发归档：",
            ui::C_WARN,
            p.sidecars.len()
        );
        for f in p.sidecars.iter().take(30) {
            println!("       {}", f.display());
        }
    }

    if !p.moved.is_empty() {
        println!("\n将移动（原图目录 → 归档目录）：");
        for m in p.moved.iter().take(40) {
            let extra = if m.note.is_empty() {
                String::new()
            } else {
                format!("  [{}]", m.note)
            };
            println!(
                "       {:<32} {:>9}{}",
                name_raw(&m.orig),
                human(file_size(&m.orig)),
                extra
            );
        }
        if p.moved.len() > 40 {
            println!("       …… 另有 {} 个", p.moved.len() - 40);
        }
    }

    if !apply {
        println!(
            "\n加 --apply 真正移动。想锁定这份清单，就加 --expect-plan {}",
            p.token
        );
        return Ok(());
    }

    std::fs::create_dir_all(&p.arc_dir)
        .map_err(|e| format!("建归档目录失败 {}：{e}", p.arc_dir.display()))?;
    std::fs::create_dir_all(&p.log_dir)
        .map_err(|e| format!("建日志目录失败 {}：{e}", p.log_dir.display()))?;

    let log_path = p
        .log_dir
        .join(format!("archive-{}.tsv", crate::timefmt::current_month()));
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("打开日志失败 {}：{e}", log_path.display()))?;

    use std::io::Write;
    let stamp = crate::timefmt::current_datetime();

    let mut errors = 0usize;
    let mut skipped = 0usize;
    let mut cross_vol = 0usize;
    let mut removed = 0usize;
    let mut archived = 0usize;

    progress::progress(0, p.moved.len(), "归档");
    for (i, m) in p.moved.iter().enumerate() {
        if m.dest.exists() {
            // 规划后被别的进程抢先放了文件
            skipped += 1;
            println!("{}目标已存在，跳过 {}", ui::C_INFO, name_raw(&m.dest));
        } else {
            match move_verified(&m.orig, &m.dest) {
                Ok(kind) => {
                    let _ = writeln!(
                        log,
                        "{}\t{}\t{}\t{}\t{}",
                        stamp,
                        p.batch,
                        name_raw(&m.orig),
                        m.orig.display(),
                        m.dest.display()
                    );
                    if kind == MoveKind::Copy {
                        cross_vol += 1;
                    }
                    archived += 1;
                }
                Err(e) => {
                    errors += 1;
                    println!("{}{}: {}", ui::C_ERR, name_raw(&m.orig), e);
                }
            }
        }
        progress::progress(i + 1, p.moved.len(), "归档");
    }

    // 归档里已经有同内容的一份，原图目录这份是重复的。
    // 删之前再比一次哈希：这中间要是被换过内容，就留着不动。
    for h in &p.redundant {
        let keep = p.arc_dir.join(name_raw(h));
        if !same_content(h, &keep) {
            errors += 1;
            println!("{}{}: 删除重复份失败：内容已变化，未删除", ui::C_ERR, name_raw(h));
            continue;
        }
        match std::fs::remove_file(h) {
            Ok(()) => {
                removed += 1;
                let _ = writeln!(
                    log,
                    "{}\t{}\t{}\t{}\t{}\t归档里已有同内容，删掉原图目录里的重复份",
                    stamp,
                    p.batch,
                    name_raw(h),
                    h.display(),
                    keep.display()
                );
            }
            Err(e) => {
                errors += 1;
                println!("{}{}: 删除重复份失败：{}", ui::C_ERR, name_raw(h), e);
            }
        }
    }

    let ok = archived;
    println!(
        "\n完成：归档 {} 个，已在归档中 {} 个，内容冲突 {} 个，失败 {} 个。",
        ok,
        p.already,
        p.conflicts.len(),
        errors
    );
    if removed > 0 {
        println!("（另有 {removed} 个在归档目录里已有同内容的一份，原图目录里的重复份已删除）");
    }
    if cross_vol > 0 {
        println!("（其中 {cross_vol} 个不在同一个卷上，是复制 + 哈希校验 + 删源，不是原子改名）");
    }
    if skipped > 0 {
        println!("（执行期间另有 {skipped} 个目标已被占用，未计入归档）");
    }
    println!("日志：{}", log_path.display());
    if p.untouched > 0 {
        println!(
            "原图目录里还剩 {} 个未修原图，继续在相册里可见。",
            p.untouched
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_ext_gate() {
        assert!(is_image_ext("jpg"));
        assert!(is_image_ext("heic"));
        assert!(is_image_ext("psd"));
        assert!(!is_image_ext("xmp"));
        assert!(!is_image_ext("aae"));
        assert!(!is_image_ext("dop"));
    }
}
