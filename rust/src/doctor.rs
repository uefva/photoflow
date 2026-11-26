//! `doctor`：全库体检，只读不写。
//!
//! 七项检查，对应 `photoflow.py` 的 `cmd_doctor`：
//!   0 顶层目录缺失  1 命名合规  2 四处目录不齐  3 成品目录根部散落
//!   4 原图目录空目录  5 已修但没归档  6 成品目录里不带 -ed 的文件
//!
//! 第 5 项是最关键的一项：漏归档就是相册里出现重复照片的唯一原因。

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use crate::config::dir_name;
use crate::naming::{
    self, area_name, batch_name_list, check_batch_name, ext_lower, human, iter_files, stem_lower,
    stem_raw, strip_ed, AREA_KEYS,
};
use crate::{progress, timefmt, ui};

pub fn run(root: &Path) -> Result<(), String> {
    let d = naming::check_root(root)?;
    let mut issues = 0usize;

    println!("== 体检：目录一致性与命名 ==\n");

    let sets: HashMap<&str, BTreeSet<String>> = AREA_KEYS
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

    let mut all_names: BTreeSet<String> = BTreeSet::new();
    for s in sets.values() {
        all_names.extend(s.iter().cloned());
    }

    println!(
        "批次数量：RAW={}  原图={}  成品={}  归档={}\n",
        sets["raw"].len(),
        sets["jpg"].len(),
        sets["ed"].len(),
        sets["arc"].len()
    );

    // 0. 顶层目录缺失
    progress::progress(0, 7, "体检");
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
    progress::progress(1, 7, "体检");
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
    progress::progress(2, 7, "体检");
    let mut disagree: Vec<(String, Vec<&str>, Vec<&str>)> = Vec::new();
    for n in &all_names {
        let have: Vec<&str> = AREA_KEYS
            .iter()
            .filter(|k| sets[*k].contains(n))
            .copied()
            .collect();
        let lack: Vec<&str> = AREA_KEYS
            .iter()
            .filter(|k| !sets[*k].contains(n))
            .copied()
            .collect();
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
    progress::progress(3, 7, "体检");
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
    progress::progress(4, 7, "体检");
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
    progress::progress(5, 7, "体检");
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
            index.entry(stem_lower(&f)).or_default().push(f);
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
    progress::progress(6, 7, "体检");
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

    progress::progress(7, 7, "体检");
    if issues == 0 {
        println!("{}没发现问题。", ui::C_OK);
    } else {
        println!("共 {issues} 项需要关注。WARN 建议处理，INFO 仅供参考。");
    }
    Ok(())
}
