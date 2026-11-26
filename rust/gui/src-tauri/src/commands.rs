//! photoflow 的桌面界面后端。
//!
//! 设计上和 Python 版一样：**界面不自己实现任何文件操作**，只是把参数拼成一条
//! 命令行交给自己（`__cli` 隐藏模式），再把输出流回界面。这样界面和 CLI 永远
//! 走同一套逻辑，不会出现「界面上能跑、命令行跑出来不一样」。
//!
//! 为什么不用进程内直接调库：库函数是往 stdout 打印的，界面要的是结构化事件流。
//! 复用现有的 `@@PROGRESS@@` / `@@PLAN@@` 协议，比把八个模块全改成「接受一个输出
//! 接收器」改动小得多，也保住了「同一套代码、两个入口」这条性质。

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use photoflow_rs::{config, naming};

/// 界面启动时需要的全部状态。
#[derive(Serialize)]
pub struct UiState {
    pub root: String,
    pub batch: String,
    pub dirs: BTreeMap<String, String>,
    /// 四个目录的显示名，免得界面上再写一遍中文
    pub area_names: BTreeMap<String, String>,
    /// 「恢复默认」要用的出厂目录名
    pub default_dirs: BTreeMap<String, String>,
    /// 配置文件位置（「设置」页显示、以及打开所在目录用）
    pub config_path: String,
}

#[tauri::command]
pub fn get_state() -> UiState {
    config::load();
    let mut dirs = BTreeMap::new();
    let mut area_names = BTreeMap::new();
    for key in naming::AREA_KEYS {
        dirs.insert(key.to_string(), config::dir_name(key));
        area_names.insert(key.to_string(), naming::area_name(key).to_string());
    }
    let mut default_dirs = BTreeMap::new();
    for (key, value) in [
        ("raw", naming::DEFAULT_RAW),
        ("jpg", naming::DEFAULT_JPG),
        ("ed", naming::DEFAULT_ED),
        ("arc", naming::DEFAULT_ARC),
    ] {
        default_dirs.insert(key.to_string(), value.to_string());
    }

    UiState {
        root: config::root().unwrap_or_default(),
        batch: config::batch(),
        dirs,
        area_names,
        default_dirs,
        config_path: config::config_path().to_string_lossy().to_string(),
    }
}

/// 保存「设置」页：根目录、当前批次、四个目录名。
#[tauri::command]
pub fn save_settings(
    root: String,
    batch: String,
    dirs: BTreeMap<String, String>,
) -> Result<(), String> {
    for (key, value) in &dirs {
        if let Some(err) = config::check_dir_setting(value) {
            return Err(format!("{}：{err}", naming::area_name(key)));
        }
    }
    let distinct: HashSet<String> = dirs.values().map(|v| v.trim().to_lowercase()).collect();
    if distinct.len() != dirs.len() {
        return Err("四个目录不能重名，否则文件会互相覆盖。".into());
    }

    config::set_dirs(dirs.into_iter().map(|(k, v)| (k, v.trim().to_string())).collect());
    config::set_root(if root.trim().is_empty() {
        None
    } else {
        Some(root.trim().to_string())
    });
    config::set_batch(batch.trim().to_string());
    config::save().map_err(|e| format!("保存失败：{e}"))
}

/// 刷新批次下拉框：四个业务目录里出现过的批次名，去重后倒序（新的在前）。
#[tauri::command]
pub fn list_batches(root: String) -> Vec<String> {
    let root = PathBuf::from(root.trim());
    if root.as_os_str().is_empty() {
        return Vec::new();
    }
    let d = naming::dirs(&root);
    let mut names = BTreeSet::new();
    for folder in [&d.jpg, &d.raw, &d.ed] {
        for n in naming::batch_name_list(folder) {
            names.insert(n);
        }
    }
    let mut v: Vec<String> = names.into_iter().collect();
    v.reverse(); // BTreeSet 是升序，倒过来让最近的在最上面
    v
}

#[derive(Deserialize)]
pub struct RunRequest {
    /// new / ingest / archive / doctor
    pub cmd: String,
    /// 子命令自己的位置参数（批次名、源目录…）
    pub args: Vec<String>,
    pub root: String,
    pub apply: bool,
    /// 预演时拿到的清单指纹；执行时带回去核对
    pub expect_plan: Option<String>,
}

#[derive(Serialize)]
pub struct RunResult {
    pub code: i32,
    /// 过滤掉哨兵行之后的完整输出
    pub text: String,
    /// 预演产生的清单指纹（执行时为 None）
    pub token: Option<String>,
}

#[derive(Serialize, Clone)]
struct ProgressEvent {
    done: u64,
    total: u64,
    label: String,
}

/// 跑一次 CLI：把自己当成子进程启动（`__cli` 模式），逐行读输出。
#[tauri::command]
pub async fn run_command(app: AppHandle, req: RunRequest) -> Result<RunResult, String> {
    let exe = std::env::current_exe().map_err(|e| format!("定位自身失败：{e}"))?;

    let mut argv: Vec<String> = vec!["__cli".into(), req.cmd];
    argv.extend(req.args.iter().cloned());
    argv.push("--root".into());
    argv.push(req.root.clone());
    argv.push("--progress".into());
    if let Some(t) = &req.expect_plan {
        argv.push("--expect-plan".into());
        argv.push(t.clone());
    }
    if req.apply {
        argv.push("--apply".into());
    }

    tauri::async_runtime::spawn_blocking(move || run_blocking(app, exe, argv))
        .await
        .map_err(|e| format!("任务异常：{e}"))?
}

fn run_blocking(app: AppHandle, exe: PathBuf, argv: Vec<String>) -> Result<RunResult, String> {
    let mut child = Command::new(&exe)
        .args(&argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("启动失败：{e}"))?;

    // stderr 单独一个线程收：正常路径它基本是空的，只有 die 才会写，
    // 但不能不读——管道写满子进程会卡住。
    let stderr = child.stderr.take();
    let err_handle = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut e) = stderr {
            let _ = e.read_to_string(&mut s);
        }
        s
    });

    let stdout = child.stdout.take().ok_or("拿不到子进程输出")?;
    let mut text = String::new();
    let mut token = None;

    for line in BufReader::new(stdout).lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if let Some(rest) = line.strip_prefix("@@PROGRESS@@\t") {
            let bits: Vec<&str> = rest.split('\t').collect();
            if bits.len() >= 3 {
                if let (Ok(done), Ok(total)) = (bits[0].parse::<u64>(), bits[1].parse::<u64>()) {
                    let _ = app.emit(
                        "pf-progress",
                        ProgressEvent {
                            done,
                            total,
                            label: bits[2].to_string(),
                        },
                    );
                }
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("@@PLAN@@\t") {
            let t = rest.trim().to_string();
            token = Some(t.clone());
            let _ = app.emit("pf-plan", t);
            continue;
        }
        let _ = app.emit("pf-line", line.clone());
        text.push_str(&line);
        text.push('\n');
    }

    let status = child.wait().map_err(|e| format!("等待子进程失败：{e}"))?;
    let err = err_handle.join().unwrap_or_default();
    if !err.trim().is_empty() {
        let _ = app.emit("pf-line", err.clone());
        text.push_str(&err);
        if !err.ends_with('\n') {
            text.push('\n');
        }
    }

    Ok(RunResult {
        code: status.code().unwrap_or(-1),
        text,
        token,
    })
}

/// 选一个目录（相机卡或照片库根目录）。
#[tauri::command]
pub fn pick_directory(app: AppHandle, title: String) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = app.dialog().file().set_title(&title).blocking_pick_folder();
    picked.and_then(|p| p.into_path().ok()).map(|p: PathBuf| p.to_string_lossy().to_string())
}

/// 在文件管理器里打开一个路径。
#[tauri::command]
pub fn open_path(path: String) -> Result<(), String> {
    let p = Path::new(&path);
    if p.as_os_str().is_empty() {
        return Err("路径是空的".into());
    }
    #[cfg(windows)]
    let spawned = Command::new("explorer").arg(p).spawn();
    #[cfg(target_os = "macos")]
    let spawned = Command::new("open").arg(p).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let spawned = Command::new("xdg-open").arg(p).spawn();

    spawned.map(|_| ()).map_err(|e| format!("打不开：{e}"))
}
