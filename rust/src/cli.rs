//! 命令行逻辑：参数解析、根目录解析、四个子命令的分发。
//!
//! 放在库里而不是二进制里，是因为图形界面也要用它——Tauri 那侧用一个隐藏的
//! `__cli` 模式直接调用 `cli::run()`，保证「界面和命令行永远走同一套逻辑」，
//! 不会出现两边行为不一致。
//!
//! 返回值就是进程退出码：0 成功，2 出错（和 Python 版的 die 一致）。

use std::path::PathBuf;

use crate::{archive, config, doctor, ingest, newcmd, progress, ui};

const DEFAULT_ROOT: &str = r"E:\TMP\outputs";

/// Python 版在没有 --root、配置里也没有时用的默认值，这里保持一致。
fn default_root() -> PathBuf {
    if let Ok(v) = std::env::var("PHOTOFLOW_ROOT") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    PathBuf::from(DEFAULT_ROOT)
}

fn usage() -> String {
    format!(
        "photoflow {}（Rust 版）\n\
         \n\
         用法：\n  \
         photoflow new     <批次> [--apply] [--force]\n  \
         photoflow ingest  <源目录> <批次> [--apply] [--dedupe] [--verify]\n  \
         photoflow archive <批次> [--apply]\n  \
         photoflow doctor\n\
         \n\
         通用参数：\n  \
         --root PATH       照片库根目录（默认读配置文件 config，其次 {DEFAULT_ROOT}）\n  \
         --apply           确认清单后真正执行（不加只预演）\n  \
         --progress        输出机器可读的进度行和计划指纹（图形界面用）\n  \
         --expect-plan X   带上预演时的清单指纹，执行前核对\n\
         \n\
         会改动文件的命令默认只预演。\n\
         当前目录设置：RAW={} 原图={} 成品={} 归档={}\n",
        env!("CARGO_PKG_VERSION"),
        config::dir_name("raw"),
        config::dir_name("jpg"),
        config::dir_name("ed"),
        config::dir_name("arc"),
    )
}

struct Args {
    cmd: String,
    positional: Vec<String>,
    root: Option<PathBuf>,
    apply: bool,
    progress: bool,
    expect_plan: Option<String>,
    verify: bool,
    dedupe: bool,
    force: bool,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    if argv.is_empty() {
        return Err(usage());
    }
    if matches!(argv[0].as_str(), "-h" | "--help" | "help") {
        return Err(usage());
    }

    let mut a = Args {
        cmd: argv[0].clone(),
        positional: Vec::new(),
        root: None,
        apply: false,
        progress: false,
        expect_plan: None,
        verify: false,
        dedupe: false,
        force: false,
    };

    let mut i = 1;
    while i < argv.len() {
        let arg = &argv[i];
        if let Some(rest) = arg.strip_prefix("--root=") {
            a.root = Some(PathBuf::from(rest));
        } else if let Some(rest) = arg.strip_prefix("--expect-plan=") {
            a.expect_plan = Some(rest.to_string());
        } else {
            match arg.as_str() {
                "--apply" => a.apply = true,
                "--progress" => a.progress = true,
                "--verify" => a.verify = true,
                "--dedupe" => a.dedupe = true,
                "--force" => a.force = true,
                "--root" => {
                    i += 1;
                    a.root = Some(PathBuf::from(
                        argv.get(i).ok_or("--root 后面要跟路径")?.clone(),
                    ));
                }
                "--expect-plan" => {
                    i += 1;
                    a.expect_plan = Some(argv.get(i).ok_or("--expect-plan 后面要跟指纹")?.clone());
                }
                other if other.starts_with('-') => {
                    return Err(format!("未知参数：{other}\n\n{}", usage()));
                }
                _ => a.positional.push(arg.clone()),
            }
        }
        i += 1;
    }
    Ok(a)
}

fn resolve_root(a: &Args) -> PathBuf {
    if let Some(r) = &a.root {
        return r.clone();
    }
    if let Some(r) = config::root() {
        return PathBuf::from(r);
    }
    default_root()
}

/// 跑一次命令行。`argv` 是去掉程序名之后的参数。
pub fn run(argv: Vec<String>) -> i32 {
    config::load();

    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            println!("{e}");
            return 2;
        }
    };

    // 配置层面的硬约束：四个目录必须互不相同。会写文件的三个命令一律拒绝执行；
    // doctor 是只读的，留着让人自查。
    if matches!(args.cmd.as_str(), "new" | "ingest" | "archive") {
        if let Some(msg) = config::dirs_conflict() {
            eprintln!("\n{}{}\n", ui::C_ERR, msg);
            return 2;
        }
    }

    progress::enable(args.progress);
    let root = resolve_root(&args);

    let result = match args.cmd.as_str() {
        "new" => match args.positional.as_slice() {
            [batch] => newcmd::run(
                &root,
                batch,
                args.apply,
                args.force,
                args.expect_plan.as_deref(),
            ),
            _ => Err(format!("用法：photoflow new <批次> [--apply] [--force]\n\n{}", usage())),
        },
        "ingest" => match args.positional.as_slice() {
            [src, batch] => ingest::run(
                &root,
                &PathBuf::from(src),
                batch,
                args.apply,
                args.dedupe,
                args.verify,
                args.expect_plan.as_deref(),
            ),
            _ => Err(format!(
                "用法：photoflow ingest <源目录> <批次> [--apply] [--dedupe] [--verify]\n\n{}",
                usage()
            )),
        },
        "archive" => match args.positional.as_slice() {
            [batch] => archive::run(&root, batch, args.apply, args.expect_plan.as_deref()),
            _ => Err(format!("用法：photoflow archive <批次> [--apply]\n\n{}", usage())),
        },
        "doctor" => doctor::run(&root),
        "gui" => Err("这是命令行版。图形界面请用 photoflow-gui（Tauri 那支）。".to_string()),
        other => Err(format!("未知命令：{other}\n\n{}", usage())),
    };

    match result {
        Ok(()) => 0,
        Err(msg) => {
            // 前后各空一行：和 Python 版 die() 的 f"\n{C_ERR}{msg}\n" 对齐
            eprintln!("\n{}{}\n", ui::C_ERR, msg);
            2
        }
    }
}
