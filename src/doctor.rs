//! `doctor`：全库体检，只读不写。
//!
//! 十项检查：
//!   0 顶层目录缺失  1 命名合规  2 四处目录不齐  3 成品目录根部散落
//!   4 原图目录空目录  5 已修但没归档  6 成品目录里不带 -ed 的文件
//!   7 视频批次对不上照片批次 / 视频空目录
//!   8 视频目录里内容重复
//!   9 照片目录里混入的视频（只报告，不参与照片流程）
//!
//! 第 5 项是最关键的一项：漏归档就是相册里出现重复照片的唯一原因。

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use crate::config::dir_name;
use crate::naming::{
    self, area_name, batch_name_list, check_batch_name, ext_lower, human, iter_files, stem_lower,
    stem_raw, strip_ed,
};
use crate::{progress, sha256, timefmt, ui};

pub fn run(root: &Path) -> Result<(), String> {
    let d = naming::check_root(root)?;
    let mut issues = 0usize;

    println!("== 体检：目录一致性与命名 ==\n");

    // 只拿四个照片目录做一致性检查；视频目录按批次单独统计，不参与「缺目录」判定。
    let photo_keys: [&str; 4] = ["raw", "jpg", "ed", "arc"];
    let sets: HashMap<&str, BTreeSet<String>> = photo_keys
        .iter()
        .map(|k| {
            let folder = match *k {
                "raw" => &d.raw,
                "jpg" => &d.jpg,
                "ed" => &d.ed,
                _ => &d.arc,
            };
            (*k, batch_name_list(folder).into_iter().collect())
        })
        .collect();

    let mut photo_names: BTreeSet<String> = BTreeSet::new();
    for s in sets.values() {
        photo_names.extend(s.iter().cloned());
    }
    let vid_names: BTreeSet<String> = batch_name_list(&d.vid).into_iter().collect();
    // 命名检查把视频批次也算进来，免得同一个不合规的名字漏报
    let mut all_names = photo_names.clone();
    all_names.extend(vid_names.iter().cloned());

    println!(
        "批次数量：RAW={}  原图={}  成品={}  归档={}  视频={}\n",
        sets["raw"].len(),
        sets["jpg"].len(),
        sets["ed"].len(),
        sets["arc"].len(),
        vid_names.len()
    );

    // 0. 顶层目录缺失
    progress::progress(0, 10, "体检");
    let gone: Vec<&str> = ["jpg", "ed", "arc"]
        .into_iter()
        .filter(|k| {
            let folder = match *k {
                "jpg" => &d.jpg,
                "ed" => &d.ed,
                _ => &d.arc,
            };
            !folder.is_dir()
        })
        .collect();
    if !gone.is_empty() {
        println!("-- 顶层目录缺失 --");
        for k in &gone {
            println!(
                "{}{}（{}）不存在，相关检查已跳过",
                ui::C_WARN,
                area_name(k),
                dir_name(k)
            );
            issues += 1;
        }
        println!();
    }

    // 1. 命名合规
    progress::progress(1, 10, "体检");
    let bad_names: Vec<(String, String)> = all_names
        .iter()
        .filter_map(|n| check_batch_name(n).err().map(|hint| (n.clone(), hint)))
        .collect();
    if !bad_names.is_empty() {
        println!("-- 批次命名 --");
        for (n, hint) in &bad_names {
            println!("{}{}：{}", ui::C_WARN, n, hint);
            issues += 1;
        }
        println!();
    }

    // 2. 四处目录不齐
    progress::progress(2, 10, "体检");
    let mut disagree: Vec<(String, Vec<&str>, Vec<&str>)> = Vec::new();
    for n in &photo_names {
        let mut have: Vec<&str> = Vec::new();
        let mut lack: Vec<&str> = Vec::new();
        for k in photo_keys {
            if sets[k].contains(n) {
                have.push(k);
            } else {
                lack.push(k);
            }
        }
        // 04/05 应该都有；03/99 只在该批次有修图时才出现，缺失属正常
        let must: Vec<&str> = lack
            .iter()
            .filter(|k| **k == "raw" || **k == "jpg")
            .copied()
            .collect();
        if !must.is_empty() {
            disagree.push((n.clone(), have, lack));
        }
    }
    if !disagree.is_empty() {
        println!("-- 批次目录不齐 --");
        for (n, have, lack) in &disagree {
            let have_s = if have.is_empty() {
                "无".to_string()
            } else {
                have.iter().map(|k| area_name(k)).collect::<Vec<_>>().join("、")
            };
            let lack_s = lack
                .iter()
                .map(|k| area_name(k))
                .collect::<Vec<_>>()
                .join("、");
            println!("{}{}：有 {}，缺 {}", ui::C_WARN, n, have_s, lack_s);
            issues += 1;
        }
        println!();
    }

    // 3. 成品目录根部散落的文件
    progress::progress(3, 10, "体检");
    let mut stray: Vec<std::path::PathBuf> = Vec::new();
    if d.ed.is_dir() {
        if let Ok(rd) = std::fs::read_dir(&d.ed) {
            for ent in rd.flatten() {
                let p = ent.path();
                if p.is_file() {
                    stray.push(p);
                }
            }
        }
    }
    stray.sort();
    if !stray.is_empty() {
        println!("-- 成品目录根部散落的文件 --");
        for f in &stray {
            let md = std::fs::metadata(f).ok();
            let size = md.as_ref().map(|m| m.len()).unwrap_or(0);
            let day = md
                .and_then(|m| m.modified().ok())
                .map(timefmt::date_of)
                .unwrap_or_else(|| "????-??-??".into());
            println!(
                "{}{}  ({})  {}",
                ui::C_WARN,
                naming::name_raw(f),
                human(size),
                day
            );
            issues += 1;
        }
        println!();
    }

    // 4. 原图目录里的空目录
    progress::progress(4, 10, "体检");
    let mut empty: Vec<std::path::PathBuf> = Vec::new();
    if d.jpg.is_dir() {
        if let Ok(rd) = std::fs::read_dir(&d.jpg) {
            let mut dirs: Vec<std::path::PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            dirs.sort();
            for dir in dirs {
                if iter_files(&dir).is_empty() {
                    empty.push(dir);
                }
            }
        }
    }
    if !empty.is_empty() {
        println!("-- 原图目录里的空目录（可删） --");
        for dir in &empty {
            println!("{}{}", ui::C_INFO, dir.display());
            issues += 1;
        }
        println!();
    }

    // 5. 已修但原图还没归档
    progress::progress(5, 10, "体检");
    let mut pending: Vec<(String, usize)> = Vec::new();
    let mut only_other: Vec<(String, usize)> = Vec::new();
    for n in &sets["ed"] {
        let ed_dir = d.ed.join(n);
        let jpg_dir = d.jpg.join(n);
        if !jpg_dir.is_dir() {
            continue;
        }
        let mut index: HashMap<String, Vec<std::path::PathBuf>> = HashMap::new();
        for f in iter_files(&jpg_dir) {
            // 视频不是原图，不能算进「还有同名文件」里
            if naming::is_photo_ext(&ext_lower(&f)) {
                index.entry(stem_lower(&f)).or_default().push(f);
            }
        }
        let (mut cnt, mut other) = (0usize, 0usize);
        for f in iter_files(&ed_dir) {
            let base = match strip_ed(&stem_raw(&f)) {
                Some(b) => b,
                None => continue,
            };
            let cands = match index.get(&base.to_lowercase()) {
                Some(v) if !v.is_empty() => v,
                _ => continue,
            };
            // 扩展名对得上才算「这张原图还没归档」；只剩别的格式的，
            // 说明对应格式已经归档过了，只是同名文件还有另一份。
            let want = ext_lower(&f);
            if cands.iter().any(|c| ext_lower(c) == want) {
                cnt += 1;
            } else {
                other += 1;
            }
        }
        if cnt > 0 {
            pending.push((n.clone(), cnt));
        }
        if other > 0 {
            only_other.push((n.clone(), other));
        }
    }
    if !pending.is_empty() {
        println!("-- 已修但原图还没归档（相册里会重复显示） --");
        for (n, cnt) in &pending {
            println!(
                "{}{}：{} 张  ->  photoflow archive {} --apply",
                ui::C_WARN,
                n,
                cnt,
                n
            );
            issues += 1;
        }
        println!();
    }
    if !only_other.is_empty() {
        println!("-- 成品对应的原图已归档，但原图目录里还留着别的格式 --");
        for (n, cnt) in &only_other {
            println!(
                "{}{}：{} 张（同名的 HEIC / TIF 之类还在原图目录里，脚本不自动搬）",
                ui::C_INFO,
                n,
                cnt
            );
            issues += 1;
        }
        println!();
    }

    // 6. 成品目录里不带 -ed 的文件
    progress::progress(6, 10, "体检");
    let mut odd: Vec<std::path::PathBuf> = Vec::new();
    for n in &sets["ed"] {
        for f in iter_files(&d.ed.join(n)) {
            if strip_ed(&stem_raw(&f)).is_none() {
                odd.push(f);
            }
        }
    }
    if !odd.is_empty() {
        println!("-- 成品目录里不带 -ed 的文件 --");
        for f in odd.iter().take(30) {
            println!("{}{}", ui::C_INFO, f.display());
            issues += 1;
        }
        println!();
    }

    // 7. 视频目录：批次对不对得上、有没有空目录
    progress::progress(7, 10, "体检");
    let mut orphan_vid: Vec<&String> = Vec::new();
    let mut empty_vid: Vec<String> = Vec::new();
    for n in &vid_names {
        if iter_files(&d.vid.join(n)).is_empty() {
            // 空目录只报「可删」，不再重复报「对不上」
            empty_vid.push(n.clone());
        } else if !sets["raw"].contains(n) && !sets["jpg"].contains(n) {
            orphan_vid.push(n);
        }
    }
    if !orphan_vid.is_empty() || !empty_vid.is_empty() {
        println!("-- 视频目录 --");
        for n in &orphan_vid {
            println!(
                "{}视频目录里有 {}，但 RAW / 原图目录里没有同名批次",
                ui::C_WARN, n
            );
            issues += 1;
        }
        for n in &empty_vid {
            println!("{}{}：视频批次目录是空的（可删）", ui::C_INFO, n);
            issues += 1;
        }
        println!();
    }

    // 8. 视频目录里内容相同的重复文件。
    // 先按大小分组，只有大小撞了才去算 SHA-256——视频动辄几个 GB，
    // 不能对每个文件都全量读一遍。
    progress::progress(8, 10, "体检");
    let mut vid_files: Vec<std::path::PathBuf> = Vec::new();
    for n in &vid_names {
        for f in iter_files(&d.vid.join(n)) {
            if naming::is_video_ext(&ext_lower(&f)) {
                vid_files.push(f);
            }
        }
    }
    let mut by_size: HashMap<u64, Vec<std::path::PathBuf>> = HashMap::new();
    for f in vid_files {
        let size = std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0);
        by_size.entry(size).or_default().push(f);
    }
    let mut dup_groups: Vec<Vec<std::path::PathBuf>> = Vec::new();
    for (_, files) in by_size {
        if files.len() < 2 {
            continue;
        }
        let mut by_hash: HashMap<[u8; 32], Vec<std::path::PathBuf>> = HashMap::new();
        for f in files {
            if let Ok(fh) = std::fs::File::open(&f) {
                if let Ok(h) = sha256::hash_reader(fh) {
                    by_hash.entry(h).or_default().push(f);
                }
            }
        }
        for (_, group) in by_hash {
            if group.len() > 1 {
                dup_groups.push(group);
            }
        }
    }
    dup_groups.sort_by(|a, b| a[0].to_string_lossy().cmp(&b[0].to_string_lossy()));
    if !dup_groups.is_empty() {
        println!("-- 视频目录里内容相同的重复文件 --");
        for group in dup_groups.iter().take(20) {
            println!("{}同一内容 {} 份：", ui::C_INFO, group.len());
            for f in group {
                println!("       {}", f.display());
            }
        }
        if dup_groups.len() > 20 {
            println!("       …… 另有 {} 组", dup_groups.len() - 20);
        }
        issues += dup_groups.len();
        println!();
    }

    // 9. 照片目录里混入的视频：只报告，不参与照片流程（不计未修、不进归档匹配）
    progress::progress(9, 10, "体检");
    let mut vids: Vec<std::path::PathBuf> = Vec::new();
    for folder in [&d.jpg, &d.ed] {
        if !folder.is_dir() {
            continue;
        }
        for n in batch_name_list(folder) {
            for f in iter_files(&folder.join(&n)) {
                if naming::is_video_ext(&ext_lower(&f)) {
                    vids.push(f);
                }
            }
        }
    }
    if !vids.is_empty() {
        println!("-- 视频文件（不在照片流程内，未计入未修与归档） --");
        for f in vids.iter().take(30) {
            println!("{}{}", ui::C_INFO, f.display());
        }
        if vids.len() > 30 {
            println!("       …… 另有 {} 个", vids.len() - 30);
        }
        println!(
            "       视频请放到视频目录（{}）下；脚本不会移动它们。",
            dir_name("vid")
        );
        println!();
        issues += 1;
    }

    progress::progress(10, 10, "体检");
    if issues == 0 {
        println!("{}没发现问题。", ui::C_OK);
    } else {
        println!("共 {issues} 项需要关注。WARN 建议处理，INFO 仅供参考。");
    }
    Ok(())
}
