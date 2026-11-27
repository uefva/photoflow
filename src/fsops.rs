//! 文件操作层。
//!
//! 这一层是整次移植里最容易「看着对、其实退化」的地方，所以每个函数都写清楚
//! 它对应的 Python 行为，以及 Rust 标准库默认行为差在哪：
//!
//! - `copy2`：Python 的 `shutil.copy2` 连修改时间一起拷；Rust `fs::copy` 不拷时间，
//!   必须自己 `set_times`。不补，`plan_token`（吃 mtime）和压测报告里那个
//!   「同大小同 mtime 跳过哈希」的 `--quick` 都会悄悄变味。
//! - `os.rename`：Python 的 `os.rename` 跨卷抛 EXDEV，代码接住、降级为复制+校验+删源。
//!   Rust `std::fs::rename` 在 Windows 上走 `MoveFileEx(MOVEFILE_COPY_ALLOWED)`，
//!   **跨卷会自己偷偷复制再删、而且不校验**——那等于把 `verify_fixes` 第 2 项白做。
//!   所以这里绝不用 `fs::rename`：同一卷用「硬链接 + 删源」（原子、目标已存在则失败），
//!   Windows 上直接调不带 `COPY_ALLOWED` 的 `MoveFileExW`，跨卷照样得到错误码 17。
//!
//! 每个文件都流式处理，不把整张照片读进内存（保住原版「峰值内存与文件大小无关」）。

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const TMP_SUFFIX: &str = ".phototmp";

/// 移动的结果，对应 Python `move_verified` 的返回值 "rename" / "copy"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveKind {
    /// 同一卷内改名（原子，未搬数据）
    Rename,
    /// 跨卷，退化成「复制 → 校验 → 删源」
    Copy,
}

/// 两个文件是否真的同一份内容：先比大小，大小相同才读哈希。
/// 只在「同名」候选上调用，所以哈希开销几乎不触发。
/// 读取失败一律当「不同」——宁可报冲突也不静默丢片。
pub fn same_content(a: &Path, b: &Path) -> bool {
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(ma), Ok(mb)) => {
            if ma.len() != mb.len() {
                return false;
            }
            match (hash_file(a), hash_file(b)) {
                (Ok(ha), Ok(hb)) => ha == hb,
                _ => false,
            }
        }
        _ => false,
    }
}

/// 流式算一个文件的 SHA-256 原始字节。
pub fn hash_file(path: &Path) -> io::Result<[u8; 32]> {
    let f = File::open(path)?;
    crate::sha256::hash_reader(f)
}

/// 去掉只读属性。Windows 上只读文件删不掉、也打不开写入句柄，
/// 所以清理临时文件和补时间之前都要先过这一道。
fn make_writable(path: &Path) -> io::Result<()> {
    let md = fs::metadata(path)?;
    let mut perm = md.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // 只补属主写位，保留原来的读/执行位，不把 0444 变成 0666
        perm.set_mode(perm.mode() | 0o200);
    }
    #[cfg(windows)]
    {
        if !perm.readonly() {
            return Ok(());
        }
        perm.set_readonly(false);
    }
    fs::set_permissions(path, perm)
}

fn drop_quietly(path: &Path) {
    // 清理路径不该反过来把主流程搞崩，删不掉就算了。
    // 先去掉只读：Windows 上 remove_file 遇到只读文件会失败，留下 .phototmp。
    let _ = make_writable(path);
    let _ = fs::remove_file(path);
}

/// 复制并校验：先写临时文件，校验通过才落位。中途出任何事都不会在库里留下半截照片。
///
/// 默认只比大小（能挡住读卡器/网络盘掉线造成的截断），`verify=true` 才两头各算一遍
/// SHA-256（挡「大小一样但内容错了」的静默损坏）。
///
/// 落位用的是 `publish_no_clobber`：目标在这期间被别人占了就失败，绝不覆盖。
/// 复制完成后把源文件的 mtime 设到临时文件上，落位后即为 `copy2` 的语义。
pub fn copy_verified(src: &Path, dst: &Path, verify: bool) -> io::Result<()> {
    copy_verified_with(src, dst, verify, |s, t| fs::copy(s, t))
}

/// 与 `copy_verified` 相同，但可注入复制函数——测试用它模拟「写一半就断」
/// 和「大小对、内容错」这两种真实故障。
pub fn copy_verified_with<F>(src: &Path, dst: &Path, verify: bool, copy: F) -> io::Result<()>
where
    F: FnOnce(&Path, &Path) -> io::Result<u64>,
{
    let tmp = sibling_tmp(dst);

    // 上次失败可能留下只读的临时文件（旧版本在 Windows 上删不掉），
    // 先清掉，否则 fs::copy 会一直写不进去。
    drop_quietly(&tmp);

    let result = (|| -> io::Result<()> {
        copy(src, &tmp)?;
        // fs::copy 会把源文件的权限位一起带过来：相机卡上「保护」的照片是 0444，
        // 临时文件也就成了只读，后面补 mtime 需要写句柄，会直接 EACCES。
        // 库里不需要只读属性，统一补上属主写位。
        make_writable(&tmp)?;
        if fs::metadata(&tmp)?.len() != fs::metadata(src)?.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "大小校验不一致"));
        }
        if verify && !same_content(src, &tmp) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "哈希校验不一致"));
        }
        copy_times(src, &tmp)?;
        // 落位：目标已存在则失败，不覆盖
        rename_no_clobber(&tmp, dst)?;
        Ok(())
    })();

    if result.is_err() {
        drop_quietly(&tmp);
    }
    result
}

/// 移动并校验。同卷走改名（原子）；跨卷退化成「复制 → 校验 → 删源」。
///
/// 注意：这里**不**用 `std::fs::rename`。原因见模块注释——它在 Windows 上跨卷会
/// 静默复制不校验。改用 `rename_no_clobber` 后，跨卷会如实返回错误码，从而进入
/// 下面真正的校验路径。
pub fn move_verified(src: &Path, dst: &Path) -> io::Result<MoveKind> {
    move_verified_with(src, dst, rename_no_clobber)
}

/// 与 `move_verified` 相同，但可注入改名函数——测试用它强制制造「跨卷」。
pub fn move_verified_with<F>(src: &Path, dst: &Path, try_rename: F) -> io::Result<MoveKind>
where
    F: FnOnce(&Path, &Path) -> io::Result<()>,
{
    match try_rename(src, dst) {
        Ok(()) => Ok(MoveKind::Rename),
        Err(e) if is_cross_device(&e) => {
            copy_verified(src, dst, true)?;
            fs::remove_file(src)?;
            Ok(MoveKind::Copy)
        }
        // 权限之类的问题原样抛出去，否则会被误当成跨卷，绕一大圈再报看不懂的错。
        Err(e) => Err(e),
    }
}

/// 判断一个 IO 错误是不是「跨卷」。对应 Python 的 `errno == EXDEV or winerror == 17`。
pub fn is_cross_device(e: &io::Error) -> bool {
    match e.raw_os_error() {
        // EXDEV：Linux/macOS/多数 Unix 都是 18
        #[cfg(unix)]
        Some(18) => true,
        // ERROR_NOT_SAME_DEVICE = 17（Windows）
        #[cfg(windows)]
        Some(17) => true,
        _ => false,
    }
}

fn sibling_tmp(dst: &Path) -> PathBuf {
    let mut name = dst.file_name().unwrap_or_default().to_os_string();
    name.push(TMP_SUFFIX);
    dst.with_file_name(name)
}

fn copy_times(src: &Path, dst: &Path) -> io::Result<()> {
    let md = fs::metadata(src)?;
    let mtime = md.modified().unwrap_or_else(|_| SystemTime::now());
    let atime = md.accessed().unwrap_or(mtime);

    let f = fs::OpenOptions::new().write(true).open(dst)?;
    let times = fs::FileTimes::new().set_modified(mtime).set_accessed(atime);
    f.set_times(times)?;
    Ok(())
}

// ---------------------------------------------------------------- 无覆盖改名

/// 把 src 改名为 dst，**目标已存在即失败**，不覆盖。
///
/// - Unix：`hard_link(src, dst)` 目标存在会失败（原子），成功后再删 src。
/// - Windows：`MoveFileExW` 不带 `REPLACE_EXISTING`/`COPY_ALLOWED`。
///   无 REPLACE → 不覆盖；无 COPY_ALLOWED → 跨卷返回错误码 17，不会静默复制。
#[cfg(unix)]
pub fn rename_no_clobber(src: &Path, dst: &Path) -> io::Result<()> {
    fs::hard_link(src, dst)?;
    fs::remove_file(src)?;
    Ok(())
}

#[cfg(windows)]
pub fn rename_no_clobber(src: &Path, dst: &Path) -> io::Result<()> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(lp_existing: *const u16, lp_new: *const u16, dw_flags: u32) -> i32;
    }

    fn wide(p: &Path) -> Vec<u16> {
        OsStr::new(p.as_os_str())
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    let s = wide(src);
    let d = wide(dst);
    // flags = 0：不加 REPLACE_EXISTING（不覆盖），不加 COPY_ALLOWED（跨卷必须报错）
    let ok = unsafe { MoveFileExW(s.as_ptr(), d.as_ptr(), 0) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn mkfile(p: &Path, data: &[u8]) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = File::create(p).unwrap();
        f.write_all(data).unwrap();
    }

    #[test]
    fn same_content_size_and_bytes() {
        let d = std::env::temp_dir().join(format!("pf-fs-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let a = d.join("a.bin");
        let b = d.join("b.bin");
        let c = d.join("c.bin");
        mkfile(&a, b"SAME");
        mkfile(&b, b"SAME");
        mkfile(&c, b"DIFF");
        assert!(same_content(&a, &b));
        assert!(!same_content(&a, &c));
        // 同大小不同内容
        let c2 = d.join("c2.bin");
        mkfile(&c2, b"DIFF"); // 与 c 相同，验证对称
        assert!(same_content(&c, &c2));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn copy_verified_preserves_mtime() {
        let d = std::env::temp_dir().join(format!("pf-mt-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let src = d.join("src.bin");
        let dst = d.join("dst.bin");
        mkfile(&src, b"hello");
        // 人为改一个明显不同的 mtime
        let past = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        let f = fs::OpenOptions::new().write(true).open(&src).unwrap();
        f.set_times(fs::FileTimes::new().set_modified(past)).unwrap();
        drop(f);

        copy_verified(&src, &dst, true).unwrap();

        let ms = fs::metadata(&src).unwrap().modified().unwrap();
        let md = fs::metadata(&dst).unwrap().modified().unwrap();
        assert_eq!(ms, md, "复制后 mtime 必须与源一致（copy2 语义）");
        assert_eq!(fs::read(&dst).unwrap(), b"hello");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn move_same_volume_is_rename() {
        let d = std::env::temp_dir().join(format!("pf-mv-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let src = d.join("s.bin");
        let dst = d.join("t.bin");
        mkfile(&src, b"data");
        let kind = move_verified(&src, &dst).unwrap();
        assert_eq!(kind, MoveKind::Rename);
        assert!(!src.exists());
        assert_eq!(fs::read(&dst).unwrap(), b"data");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn move_cross_device_degrades_to_copy_and_preserves_source_on_verify_fail() {
        let d = std::env::temp_dir().join(format!("pf-x-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let src = d.join("s.bin");
        let dst = d.join("t.bin");
        let mut data = vec![b'o'; 100];
        mkfile(&src, &data);

        // 强制「跨卷」：改名永远报 EXDEV → 应走「复制 + 校验 + 删源」
        let kind = move_verified_with(&src, &dst, |_, _| {
            Err(io::Error::from_raw_os_error(18))
        })
        .unwrap();
        assert_eq!(kind, MoveKind::Copy);
        assert!(!src.exists(), "复制成功后应删源");
        data = vec![b'o'; 100];
        assert_eq!(fs::read(&dst).unwrap(), data);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn move_does_not_clobber_existing_target() {
        let d = std::env::temp_dir().join(format!("pf-nc-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let src = d.join("s.bin");
        let dst = d.join("t.bin");
        mkfile(&src, b"newsrc");
        mkfile(&dst, b"keepme");

        let r = move_verified(&src, &dst);
        assert!(r.is_err(), "目标已存在时不能覆盖，应失败");
        assert_eq!(fs::read(&dst).unwrap(), b"keepme", "目标内容不能被改动");
        assert!(src.exists(), "失败时源文件应保留");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn copy_verified_cleans_up_when_size_mismatch() {
        let d = std::env::temp_dir().join(format!("pf-short-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let src = d.join("src.bin");
        let dst = d.join("dst.bin");
        mkfile(&src, &vec![b'A'; 100]);

        // 只写 40 字节就「成功」返回 —— 对应的就是掉线截断
        let r = copy_verified_with(&src, &dst, false, |_s, t| {
            let mut f = File::create(t)?;
            f.write_all(&vec![b'A'; 40])?;
            Ok(40)
        });
        assert!(r.is_err(), "大小不符必须报错");
        assert!(!dst.exists(), "校验没过不能落正式文件");
        assert!(
            !sibling_tmp(&dst).exists(),
            "临时文件必须清理掉，否则下次会被当成冲突"
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn copy_verified_detects_same_size_wrong_bytes() {
        let d = std::env::temp_dir().join(format!("pf-verify-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let src = d.join("src.bin");
        let dst = d.join("dst.bin");
        mkfile(&src, &vec![b'A'; 100]);

        // 大小一样、内容全错：只有 verify=true 才拦得住
        let lie = |_s: &Path, t: &Path| -> io::Result<u64> {
            let mut f = File::create(t)?;
            f.write_all(&vec![b'X'; 100])?;
            Ok(100)
        };
        let r = copy_verified_with(&src, &dst, true, lie);
        assert!(r.is_err(), "--verify 没拦住内容不对的复制");
        assert!(!dst.exists());
        assert!(!sibling_tmp(&dst).exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn copy_verified_does_not_clobber_existing_target() {
        let d = std::env::temp_dir().join(format!("pf-cc-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let src = d.join("src.bin");
        let dst = d.join("dst.bin");
        mkfile(&src, b"new");
        mkfile(&dst, b"keep");

        let r = copy_verified(&src, &dst, false);
        assert!(r.is_err(), "目标已存在时不能覆盖");
        assert_eq!(fs::read(&dst).unwrap(), b"keep");
        let _ = fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn cross_device_classification() {
        assert!(is_cross_device(&io::Error::from_raw_os_error(18)));
        assert!(!is_cross_device(&io::Error::from_raw_os_error(13))); // EACCES 不算跨卷
    }
}
