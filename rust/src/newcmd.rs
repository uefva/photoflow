//! `new`：为一个批次一次建好 RAW / 原图 / 成品 / 归档四个同名目录。
//!
//! 对应 `photoflow.py` 的 `cmd_new`。两个要点：
//!   - 批次名先过日期格式校验（`--force` 可放过），再过路径安全校验（不可放过）；
//!   - 预演确认的是「这四个路径现在各自是什么状态」，执行前用指纹核对一次。

use std::path::{Path, PathBuf};

use crate::naming::{self, batch_dir, check_batch_name, Dirs};
use crate::{plan, progress, ui};

pub fn run(
    root: &Path,
    name: &str,
    apply: bool,
    force: bool,
    expect_plan: Option<&str>,
) -> Result<(), String> {
    if let Err(hint) = check_batch_name(name) {
        if !force {
            return Err(format!(
                "批次名不合规：{name}\n  原因：{hint}\n  如确实要用这个名字，加 --force。"
            ));
        }
    }

    let d: Dirs = naming::dirs(root);
    let targets: Vec<(&str, PathBuf)> = vec![
        ("RAW", batch_dir(&d.raw, name)?),
        ("JPG", batch_dir(&d.jpg, name)?),
        ("成品", batch_dir(&d.ed, name)?),
        ("归档", batch_dir(&d.arc, name)?),
    ];

    // 目标路径被同名文件占住时，mkdir 会失败——先查出来，给一句人话
    let blocked: Vec<&PathBuf> = targets
        .iter()
        .filter(|(_, p)| p.exists() && !p.is_dir())
        .map(|(_, p)| p)
        .collect();
    if !blocked.is_empty() {
        let mut msg = String::from("以下路径已被同名文件占用，无法建目录：\n");
        for b in &blocked {
            msg.push_str(&format!("  {}\n", b.display()));
        }
        msg.push_str("请先把这些文件移走或改名，再重新执行。");
        return Err(msg);
    }

    // 预演确认的是「这 4 个路径现在各自是什么状态」，执行前再核对一次
    let rows: Vec<Vec<String>> = targets
        .iter()
        .map(|(label, p)| {
            vec![
                label.to_string(),
                p.display().to_string(),
                p.is_dir().to_string(),
            ]
        })
        .collect();
    let token = plan::token(&rows);
    plan::check_plan(expect_plan, &token)?;
    progress::announce_plan(&token);

    ui::confirm_apply(apply, &format!("为批次 {name} 建 4 个同名目录"));

    let (mut created, mut existed) = (0usize, 0usize);
    progress::progress(0, targets.len(), "建目录");
    for (i, (label, path)) in targets.iter().enumerate() {
        if path.is_dir() {
            println!("{}{:<4} 已存在  {}", ui::C_INFO, label, path.display());
            existed += 1;
        } else {
            println!("{}{:<4} 新建    {}", ui::C_OK, label, path.display());
            if apply {
                std::fs::create_dir_all(path)
                    .map_err(|e| format!("建目录失败 {}：{e}", path.display()))?;
            }
            created += 1;
        }
        progress::progress(i + 1, targets.len(), "建目录");
    }

    println!("\n新建 {created} 个，已存在 {existed} 个。");
    if !apply && created > 0 {
        println!("加 --apply 真正创建。想锁定这份清单，就加 --expect-plan {token}");
    }
    Ok(())
}
