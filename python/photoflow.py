#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
photoflow — 照片库工作流助手

目录约定（与现有结构完全一致，不新增、不改名）：
  <root>/04_RAW原图/<批次>/           相机 RAW 底片
  <root>/05_JPG图/<批次>/             待处理 JPG 工作区
  <root>/03_缓存-调色ed/<批次>/       修图成品，文件名带 -ed
  <root>/99_图库/原图JPG归档/<批次>/  原图 JPG 永久归档

所有会改动文件的命令默认只做预演（dry-run），确认清单无误后加 --apply 才真正执行。

用法示例：
  python photoflow.py                      不带参数直接开图形界面（双击 photoflow.bat 同效）
  python photoflow.py gui                  显式打开图形界面

  python photoflow.py new 2026-11-25-香炉石
  python photoflow.py new 2026-11-25-香炉石 --apply

  python photoflow.py ingest F:\\DCIM 2026-11-25-香炉石
  python photoflow.py ingest F:\\DCIM 2026-11-25-香炉石 --apply

  python photoflow.py archive 2026-11-25-香炉石
  python photoflow.py archive 2026-11-25-香炉石 --apply

  python photoflow.py doctor
"""

import argparse
import datetime as _dt
import errno
import hashlib
import json
import os
import re
import shutil
import sys
import traceback
from pathlib import Path

# 重定向到文件时用 utf-8，直接输出到控制台时保留 Windows 的 Unicode 控制台，避免中文乱码
for _stream in (sys.stdout, sys.stderr):
    try:
        if not _stream.isatty():
            _stream.reconfigure(encoding="utf-8")
    except Exception:
        pass

DEFAULT_ROOT = r"E:\TMP\outputs"

# 四个业务目录的默认名字。用户可以在配置文件或界面的「设置」里改，
# 改完 CLI 和图形界面都跟着走，不需要动代码。
DEFAULT_DIRS = {
    "raw": "04_RAW原图",
    "jpg": "05_JPG图",
    "ed": "03_缓存-调色ed",
    "arc": os.path.join("99_图库", "原图JPG归档"),
}

# 兼容旧代码：这两组名字作为默认值保留，实际路径一律走 _CONFIG。
DIR_RAW = DEFAULT_DIRS["raw"]
DIR_JPG = DEFAULT_DIRS["jpg"]
DIR_ED = DEFAULT_DIRS["ed"]
DIR_ARC = DEFAULT_DIRS["arc"]

# 配置文件位置。测试或多套照片库可以用环境变量 PHOTOFLOW_CONFIG 指到别处。
CONFIG_PATH = Path(os.environ.get("PHOTOFLOW_CONFIG") or (Path.home() / ".photoflow.json"))

_CONFIG = {"root": None, "batch": "", "dirs": dict(DEFAULT_DIRS)}

# 输出里给用户看的名字。一律用「RAW 目录 / 原图目录 / 成品目录 / 归档目录」这种说法，
# 不写 03/04/05/99 这类编号——换个不熟悉目录结构的人看，编号等于天书。
AREA_NAME = {
    "raw": "RAW 目录",
    "jpg": "原图目录",
    "ed": "成品目录",
    "arc": "归档目录",
}

RAW_EXT = {".cr2", ".cr3", ".nef", ".nrw", ".arw", ".srf", ".sr2", ".raf",
           ".dng", ".orf", ".rw2", ".pef", ".srw", ".raw", ".3fr", ".x3f"}
JPG_EXT = {".jpg", ".jpeg", ".jpe", ".png", ".heic", ".heif", ".tif", ".tiff"}

SKIP_DIR_NAMES = {"system volume information", "$recycle.bin", ".trashes",
                  ".spotlight-v100", ".fseventsd", "dcim_bkp"}

NAME_RE = re.compile(r"^(\d{4})-(\d{2})-(\d{2})-(.+)$")
LOOSE_NAME_RE = re.compile(r"^(\d{4})-(\d{1,2})-(\d{1,2})-(.+)$")
# DSC01234-ed.jpg / DSC01234-ed2.jpg / DSC01234-ed-2.jpg / DSC01234-ed-Edit.jpg
ED_RE = re.compile(r"^(?P<base>.+?)-ed(?:\d+|[-_][\w]+)*$", re.IGNORECASE)

C_OK = "  OK  "
C_WARN = " WARN "
C_ERR = " ERR  "
C_INFO = " INFO "

# 图形界面的进度条靠这一行前缀识别。只有加了 --progress 才会输出，
# 普通命令行用法永远看不到这些行。
PROGRESS_TAG = "@@PROGRESS@@"
PLAN_TAG = "@@PLAN@@"
_PROGRESS = {"on": False}


def progress(done, total, label=""):
    """
    打一行机器可读的进度。节流到最多 200 次，避免几万个文件把管道刷爆。

    进度打到 stdout 而不是 stderr，是为了和普通输出保持同一顺序：
    每次 print 带 flush，前面的文字不会压在缓冲区里迟迟不到界面。
    """
    if not _PROGRESS["on"]:
        return
    total = int(total)
    if total <= 0:          # 没事可做，不输出，界面那边由收尾统一置 100%
        return
    done = int(done)
    step = max(1, total // 200)
    if done < total and done % step:
        return
    print(f"{PROGRESS_TAG}\t{done}\t{total}\t{label}", flush=True)


# ---------------------------------------------------------------- 基础设施

def check_dir_setting(value):
    """校验一个目录设置项。返回错误说明，None 表示可以用。"""
    if not value or not value.strip():
        return "不能为空"
    v = value.strip()
    if os.path.isabs(v) or re.match(r"^[A-Za-z]:", v):
        return "要写相对路径，不要写盘符或绝对路径"
    parts = re.split(r"[\\/]", v)
    if any(seg in ("", ".", "..") for seg in parts):
        return "不能包含空段、. 或 .."
    if any(ch in v for ch in '<>:"|?*') or any(ord(ch) < 32 for ch in v):
        return "含有不能用于文件名的字符"
    return None


def load_config(path=None):
    """读配置文件。读不到、或某一项不合法，就退回默认值，不报错。"""
    cfg = Path(path) if path else CONFIG_PATH
    try:
        data = json.loads(cfg.read_text(encoding="utf-8"))
    except Exception:                                        # noqa: BLE001
        return _CONFIG
    if not isinstance(data, dict):
        return _CONFIG

    root = data.get("root")
    if isinstance(root, str) and root.strip():
        _CONFIG["root"] = root.strip()

    batch = data.get("batch")
    if isinstance(batch, str):
        _CONFIG["batch"] = batch.strip()

    dirs = data.get("dirs")
    if isinstance(dirs, dict):
        for key in DEFAULT_DIRS:
            val = dirs.get(key)
            if isinstance(val, str) and check_dir_setting(val) is None:
                _CONFIG["dirs"][key] = os.path.normpath(val.strip())
    return _CONFIG


def save_config(path=None):
    cfg = Path(path) if path else CONFIG_PATH
    cfg.parent.mkdir(parents=True, exist_ok=True)
    cfg.write_text(json.dumps({"root": _CONFIG["root"], "batch": _CONFIG["batch"],
                               "dirs": dict(_CONFIG["dirs"])},
                              ensure_ascii=False, indent=2), encoding="utf-8")


def dir_name(key):
    """当前生效的目录名（相对照片库根目录）。"""
    return _CONFIG["dirs"][key]


def paths(root):
    r = Path(root)
    return {
        "raw": r / dir_name("raw"),
        "jpg": r / dir_name("jpg"),
        "ed": r / dir_name("ed"),
        "arc": r / dir_name("arc"),
        "logs": r / dir_name("arc") / "_logs",
    }


def check_root(root):
    p = paths(root)
    if not p["raw"].is_dir():
        die(f"找不到 RAW 目录：{p['raw']}\n"
            f"照片库所在的盘没挂载，或者路径填错了。可以用 --root 指定正确的根目录。")
    return p


def die(msg, code=2):
    print(f"\n{C_ERR}{msg}\n", file=sys.stderr)
    sys.exit(code)


def human(n):
    for unit in ("B", "KB", "MB", "GB", "TB"):
        if n < 1024 or unit == "TB":
            return f"{n:.1f}{unit}" if unit != "B" else f"{n}B"
        n /= 1024


def file_hash(path, chunk=1 << 20):
    """
    SHA-256。

    刻意不做缓存：同一次运行期间文件内容可能被别的程序改掉，按路径缓存会把
    「已经改过」判断成「还是同一份」，进而在复制/移动时做出错误决定。
    哈希只在「同名且同大小」的候选上触发，重复计算的代价可以忽略。
    """
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for block in iter(lambda: fh.read(chunk), b""):
            h.update(block)
    return h.hexdigest()


def same_content(a, b):
    """
    判断两个文件是否真的是同一份内容：先比大小，大小相同才读哈希。
    只在「同名」的候选上调用，所以哈希开销几乎不会触发。
    读取失败一律当作「不同」，宁可报冲突也不静默丢片。
    """
    try:
        if a.stat().st_size != b.stat().st_size:
            return False
        return file_hash(a) == file_hash(b)
    except OSError:
        return False


TMP_SUFFIX = ".phototmp"


def _drop(path):
    """删掉一个文件，删不掉就算了 —— 清理路径不该反过来把主流程搞崩。"""
    try:
        if path.exists():
            path.unlink()
    except OSError:
        pass


def copy_verified(src, dst, verify=False):
    """
    先写临时文件，校验通过才落位。中途出任何事都不会在库里留下半截照片。

    默认只比大小：能挡住截断（网络盘、读卡器掉线的主要表现），几乎不花时间。
    加 verify 才两头各算一遍 SHA-256，能挡「大小一样但内容错了」的静默损坏，
    代价是把源文件再读一遍。
    """
    tmp = dst.with_name(dst.name + TMP_SUFFIX)
    try:
        shutil.copy2(str(src), str(tmp))
        if tmp.stat().st_size != src.stat().st_size:
            raise OSError("大小校验不一致")
        if verify and not same_content(src, tmp):
            raise OSError("哈希校验不一致")
        os.replace(str(tmp), str(dst))
    except BaseException:
        _drop(tmp)          # Ctrl-C 也要清干净
        raise


def move_verified(src, dst, verify=True):
    """
    同卷改名是原子的，直接做；只有跨卷才退化成「复制 → 校验 → 删源」。

    shutil.move 在跨卷时复制完就删源，从不检查复制得对不对。归档目录放的是
    长期保存的原图，这一步不能只信它。返回 "rename" 或 "copy"。
    """
    try:
        os.rename(str(src), str(dst))
        return "rename"
    except OSError as e:
        # 只有「不是同一个卷」才该退化成复制；权限之类的错误原样抛出去，
        # 否则会被误当成跨卷，绕一大圈再报一个看不懂的错。
        if not (getattr(e, "errno", None) == errno.EXDEV
                or getattr(e, "winerror", None) == 17):
            raise
    copy_verified(src, dst, verify=verify)
    os.unlink(str(src))
    return "copy"


def stat_sig(path):
    """(大小, 修改时间)。文件不在了就返回 (None, None)，交给指纹去体现差异。"""
    try:
        st = path.stat()
        return st.st_size, st.st_mtime_ns
    except OSError:
        return None, None


def plan_token(rows):
    """
    把「这次要动哪些文件」压成一个短指纹。

    只吃路径、大小、修改时间：内容不变、位置不变，指纹就不变。预演时算一次，
    执行时再算一次，对不上就说明中间被动过。
    """
    h = hashlib.sha256()
    for row in rows:
        h.update("\x1f".join("" if x is None else str(x) for x in row)
                 .encode("utf-8", "replace"))
        h.update(b"\x1e")
    return h.hexdigest()[:16]


def announce_plan(token):
    """把计划指纹交给图形界面。只有 --progress 时才输出，命令行用户看不到。"""
    if _PROGRESS["on"]:
        print(f"{PLAN_TAG}\t{token}", flush=True)


def check_plan(expected, actual):
    """执行前确认：现在这份计划，和用户预演时确认的那份还是不是同一份。"""
    if expected and expected != actual:
        die(f"计划已经变了，不能直接执行。\n"
            f"  预演时：{expected}\n"
            f"  现在　：{actual}\n"
            f"这中间照片库里多了、少了、或者有文件被改动过。\n"
            f"请重新预演，看清新的清单再执行。")


def iter_files(folder):
    """递归列文件，跳过系统目录。"""
    if not folder.is_dir():
        return
    for dirpath, dirnames, filenames in os.walk(folder):
        dirnames[:] = [d for d in dirnames if d.lower() not in SKIP_DIR_NAMES]
        for fn in filenames:
            yield Path(dirpath) / fn


def check_batch_name(name):
    """返回 (是否合规, 提示)。"""
    m = NAME_RE.match(name)
    if m:
        y, mo, d = int(m.group(1)), int(m.group(2)), int(m.group(3))
        try:
            _dt.date(y, mo, d)
        except ValueError:
            return False, "日期不存在"
        if " " in m.group(4):
            return False, "地点含空格，建议用 - 或 _"
        return True, ""
    m2 = LOOSE_NAME_RE.match(name)
    if m2:
        y, mo, d, rest = m2.groups()
        fix = f"{y}-{int(mo):02d}-{int(d):02d}-{rest}"
        return False, f"月/日未补零，建议改为 {fix}"
    return False, "不符合 YYYY-MM-DD-地点 格式"


# 批次名里绝不能出现的字符：路径分隔符 + Windows 保留字符 + 控制字符
UNSAFE_CHARS = set('/\\:*?"<>|')


def batch_name_error(name):
    """
    只判断「安全性」，不判断日期格式。返回错误说明，None 表示安全。

    用黑名单而不是「只允许中文/字母/数字/-/_」的白名单，是因为现有批次名里
    有 `2025-10-01-梧桐岛+西湾红树林` 这类带 + 的目录，白名单会直接把老批次挡在门外。
    斜杠、反斜杠、冒号、控制字符全部禁掉之后，路径穿越已经不可能发生，
    下面 batch_dir() 还会再校验一次最终路径是否仍在库内。
    """
    if not name or name in (".", ".."):
        return "批次名为空，或为 . / .."
    if len(name) > 120:
        return "批次名超过 120 个字符"
    bad = sorted({ch for ch in name if ch in UNSAFE_CHARS or ord(ch) < 32})
    if bad:
        shown = " ".join(repr(c) for c in bad)
        return f"含路径分隔符或控制字符：{shown}"
    if name != name.strip():
        return "首尾不能有空格"
    if name.endswith("."):
        return "结尾不能是点（Windows 会悄悄去掉）"
    return None


def batch_dir(base, name):
    """把批次名拼到 base 下，并确认最终路径没有跑出 base。"""
    err = batch_name_error(name)
    if err:
        die(f"批次名不安全：{name}\n  {err}\n"
            f"  这一项不受 --force 影响，请先改成安全的名称。")
    base_p = Path(base)
    target = base_p / name

    # 这里刻意不用 resolve()：在极空间这类网络卷上，Windows 的 realpath 会抛
    # WinError 1005（此卷不包含可识别的文件系统），连存在的目录都解析不了。
    # 改成纯字符串规范化比较，完全不碰文件系统。
    base_n = os.path.normpath(str(base_p))
    target_n = os.path.normpath(str(target))
    try:
        inside = os.path.commonpath([base_n, target_n]) == base_n
    except ValueError:          # 不同盘符之类，commonpath 会抛
        inside = False
    if not inside:
        die(f"批次路径越界，已拒绝执行：{target}")
    return target


def batch_name_list(folder):
    """列出批次目录，跳过 _logs 这类下划线/点开头的辅助目录。"""
    if not folder.is_dir():
        return []
    return sorted(d.name for d in folder.iterdir()
                  if d.is_dir() and not d.name.startswith(("_", ".")))


def confirm_apply(apply_flag, what):
    """统一输出「预演 / 执行」抬头。"""
    if apply_flag:
        print(f"== 执行：{what} ==\n")
    else:
        print(f"== 预演：{what} ==\n"
              f"（下面只是清单，没有动任何文件。确认无误后加 --apply 再跑一次）\n")


# ---------------------------------------------------------------- new

def cmd_new(args):
    name = args.batch
    ok, hint = check_batch_name(name)
    if not ok and not args.force:
        die(f"批次名不合规：{name}\n  原因：{hint}\n"
            f"  如确实要用这个名字，加 --force。")

    p = paths(args.root)
    targets = [
        ("RAW", batch_dir(p["raw"], name)),
        ("JPG", batch_dir(p["jpg"], name)),
        ("成品", batch_dir(p["ed"], name)),
        ("归档", batch_dir(p["arc"], name)),
    ]

    # 目标路径被同名文件占住时，mkdir 会抛 FileExistsError —— 先查出来，给一句人话
    blocked = [path for _, path in targets if path.exists() and not path.is_dir()]
    if blocked:
        die("以下路径已被同名文件占用，无法建目录：\n"
            + "\n".join(f"  {b}" for b in blocked)
            + "\n请先把这些文件移走或改名，再重新执行。")

    # 预演确认的是「这 4 个路径现在各自是什么状态」，执行前再核对一次
    token = plan_token([(label, str(path), path.is_dir()) for label, path in targets])
    check_plan(getattr(args, "expect_plan", None), token)
    announce_plan(token)

    confirm_apply(args.apply, f"为批次 {name} 建 4 个同名目录")
    created = existed = 0
    progress(0, len(targets), "建目录")
    for i, (label, path) in enumerate(targets, 1):
        if path.is_dir():
            print(f"{C_INFO}{label:<4} 已存在  {path}")
            existed += 1
        else:
            print(f"{C_OK}{label:<4} 新建    {path}")
            if args.apply:
                path.mkdir(parents=True, exist_ok=True)
            created += 1
        progress(i, len(targets), "建目录")

    print(f"\n新建 {created} 个，已存在 {existed} 个。")
    if not args.apply and created:
        print(f"加 --apply 真正创建。想锁定这份清单，就加 --expect-plan {token}")


# ---------------------------------------------------------------- ingest

def cmd_ingest(args):
    src = Path(args.src)
    if not src.is_dir():
        die(f"源目录不存在：{src}")
    p = check_root(args.root)

    name = args.batch
    ok, hint = check_batch_name(name)
    if not ok:
        print(f"{C_WARN}批次名不合规：{hint}")

    dst_raw = batch_dir(p["raw"], name)
    dst_jpg = batch_dir(p["jpg"], name)

    plan = {"raw": [], "jpg": [], "other": [], "skip": [], "conflict": [],
            "dup_same": [], "dup_diff": [], "renamed": [], "target_clash": []}
    families = {}       # (kind, 文件名小写) -> [[源文件, 目标路径], ...]，同名文件的全部内容版本
    used_names = set()  # (kind, 目标文件名小写)，避免 _2/_3 撞名

    files = sorted(iter_files(src), key=lambda p: str(p).lower())

    # 卡里所有会被导入的文件名，先收齐。
    # 改名时必须绕开它们：卡里本来就有一张 DSC1_2.jpg 的时候，改名改出来的
    # DSC1_2.jpg 会在执行阶段把它顶掉 —— 报告说「3 个成功」，实际只落 2 个。
    reserved = set()
    for f in files:
        ext = f.suffix.lower()
        if ext in RAW_EXT:
            reserved.add(("raw", f.name.lower()))
        elif ext in JPG_EXT:
            reserved.add(("jpg", f.name.lower()))

    def alt_name(kind, dst_dir, f):
        """卡内同名但内容不同：加 _2/_3 后缀，避免覆盖或丢片。"""
        n = 2
        while True:
            cand = dst_dir / f"{f.stem}_{n}{f.suffix}"
            key = (kind, cand.name.lower())
            # 三重回避：目标已存在、本批已经分配过、卡里本来就叫这个名字
            if (not cand.exists()
                    and key not in used_names
                    and (key not in reserved or cand.name.lower() == f.name.lower())):
                return cand
            n += 1

    def find_twin(dst_dir, f):
        """
        上一次 --dedupe 把这张存成了 DSC1_2.jpg，这次要认出来。
        否则每次重跑都报「冲突」，逼着人一个个看。
        """
        n = 2
        while True:
            cand = dst_dir / f"{f.stem}_{n}{f.suffix}"
            if not cand.exists():
                return None
            if same_content(f, cand):
                return cand
            n += 1
    progress(0, len(files), "扫描")
    for i, f in enumerate(files, 1):
        # 重复分发时最慢的是这里（同名文件要两头各算一遍哈希），进度放在最前面，
        # 这样卡在大文件上也能看出还在走
        progress(i, len(files), "扫描")
        ext = f.suffix.lower()
        if ext in RAW_EXT:
            kind, dst_dir = "raw", dst_raw
        elif ext in JPG_EXT:
            kind, dst_dir = "jpg", dst_jpg
        else:
            plan["other"].append(f)
            continue

        key = (kind, f.name.lower())
        dst = dst_dir / f.name
        used_names.add((kind, dst.name.lower()))

        if dst.exists():
            if same_content(f, dst):
                plan["skip"].append(dst)
                continue
            twin = find_twin(dst_dir, f)
            if twin is not None:
                plan["dup_same"].append((f, twin))
            else:
                try:
                    same_size = dst.stat().st_size == f.stat().st_size
                except OSError:
                    same_size = False
                plan["conflict"].append(
                    (f, dst, "大小相同但内容不同" if same_size else "大小不同"))
            continue

        versions = families.setdefault(key, [])

        # 跟这个文件名下的每一个已知版本逐一比对，而不是只跟第一个比。
        # 否则 a=AAAA、b=BBBB、c=BBBB 会把 c 也当成新照片，多出 IMG_3。
        match = next((v for v in versions if same_content(f, v[0])), None)
        if match is not None:
            plan["dup_same"].append((f, match[0]))
            continue

        if versions and not args.dedupe:
            plan["dup_diff"].append((f, versions[0][0]))
            continue

        if not versions:
            target = dst
        else:
            target = alt_name(kind, dst_dir, f)
            used_names.add((kind, target.name.lower()))
            plan["renamed"].append((f, target))

        versions.append([f, target])
        plan[kind].append((f, target))

    # 收尾再验一遍：计划里不能有两个源文件落到同一个目标名上。
    # 真出现了就是会静默覆盖丢片，宁可拦下来报冲突。
    seen_target = {}
    for kind in ("raw", "jpg"):
        keep = []
        for s, target in plan[kind]:
            key = (kind, target.name.lower())
            if key in seen_target:
                plan["target_clash"].append((s, target, seen_target[key]))
                continue
            seen_target[key] = s
            keep.append((s, target))
        plan[kind] = keep

    total_bytes = sum(f.stat().st_size for f, _ in plan["raw"] + plan["jpg"])

    # 清单 = 真正要复制的那些 (类型, 源, 目标, 大小, 修改时间)
    token = plan_token([(kind, str(s), str(d)) + stat_sig(s)
                        for kind in ("raw", "jpg") for s, d in plan[kind]])
    check_plan(getattr(args, "expect_plan", None), token)
    announce_plan(token)

    confirm_apply(args.apply, f"从 {src} 分发到批次 {name}")
    print(f"RAW  待复制 {len(plan['raw']):>4} 个")
    print(f"JPG  待复制 {len(plan['jpg']):>4} 个")
    print(f"已存在跳过 {len(plan['skip']):>4} 个")
    print(f"合计待复制 {human(total_bytes)}")
    if plan["other"]:
        print(f"{C_INFO}非 RAW/JPG 跳过 {len(plan['other'])} 个"
              f"（如 {plan['other'][0].name}）")

    if plan["dup_same"]:
        print(f"{C_INFO}卡内同名且内容相同 {len(plan['dup_same'])} 个 —— 已校验哈希，跳过不重复导入")
    if plan["renamed"]:
        print(f"{C_WARN}卡内同名但内容不同 {len(plan['renamed'])} 个 —— 已改用 _2/_3 后缀，避免丢片：")
        for s, d in plan["renamed"][:20]:
            print(f"       {s}  ->  {d.name}")
    if plan["dup_diff"]:
        print(f"\n{C_WARN}冲突：卡内同名但内容不同 {len(plan['dup_diff'])} 个 —— 判定为不同照片，未复制：")
        for s, first in plan["dup_diff"][:20]:
            print(f"       {s}  ({human(s.stat().st_size)})")
            print(f"       与 {first}  ({human(first.stat().st_size)}) 撞名")
        print("       加 --dedupe 自动加 _2 后缀；或手动改名后重跑。")

    if plan["conflict"]:
        print(f"\n{C_WARN}冲突：目标已存在但内容不同 {len(plan['conflict'])} 个 —— 不覆盖，需你人工判断：")
        for s, d, reason in plan["conflict"][:20]:
            print(f"       源   {s}  ({human(s.stat().st_size)})")
            print(f"       已有 {d}  ({human(d.stat().st_size)}，{reason})")

    if plan["target_clash"]:
        print(f"\n{C_WARN}两个源文件算出了同一个目标名 {len(plan['target_clash'])} 个 —— 已拦下，不复制：")
        for s, d, first in plan["target_clash"][:20]:
            print(f"       {s}")
            print(f"       会和 {first} 一起落到 {d.name}")
        print("       这是脚本分配文件名时出的错，不是你的问题。加 --dedupe 重跑，或先手动改名。")

    if not args.apply:
        print(f"\n加 --apply 真正复制。想锁定这份清单，就加 --expect-plan {token}")
        return

    errors = raced = 0
    copied = {"raw": 0, "jpg": 0}
    plan_total = len(plan["raw"]) + len(plan["jpg"])
    done = 0
    progress(0, plan_total, "复制")
    for kind, label, dst_dir in (("raw", "RAW", dst_raw), ("jpg", "JPG", dst_jpg)):
        dst_dir.mkdir(parents=True, exist_ok=True)
        for i, (s, d) in enumerate(plan[kind], 1):
            done += 1
            # 计划之后、落盘之前，目标位置被别的程序占了：报出来，绝不覆盖
            if d.exists():
                raced += 1
                print(f"{C_ERR}{s.name}: 目标位置在执行期间被占用，跳过（不覆盖）")
                progress(done, plan_total, "复制")
                continue
            try:
                copy_verified(s, d, verify=args.verify)
            except OSError as e:
                errors += 1
                print(f"{C_ERR}{s.name}: {e}")
            else:
                copied[kind] += 1
                if i % 50 == 0:
                    print(f"       {label} {i}/{len(plan[kind])} ...")
            progress(done, plan_total, "复制")
    print(f"\n完成：RAW {copied['raw']} 个，JPG {copied['jpg']} 个，失败 {errors} 个。")
    if raced:
        print(f"（另有 {raced} 个的目标位置在执行期间被别的程序占了，已跳过，未覆盖）")
    if errors:
        print("失败项未复制，请检查网络连接后重跑（已复制的会自动跳过）。")


# ---------------------------------------------------------------- archive

# Lightroom 的 AI 降噪会派生 `原名-已增强-降噪.dng`（英文界面是 -Enhanced-NR），
# 之后从这张 DNG 导出的 JPG 也会带着这个中缀，匹配原图前要先把它剥掉，
# 否则 DSC07287-已增强-降噪-ed.JPG 永远对不上 DSC07287.JPG。
DENOISE_SUFFIXES = ("-已增强-降噪", "-Enhanced-NR")
# LR 里「在 Photoshop 中编辑」再导出，文件名会带上 -Edit：DSC1-Edit-ed.jpg。
# 不剥掉这一层，就永远对不上 DSC1.JPG。
EDIT_SUFFIXES = ("-Edit",)


def strip_ed(stem):
    m = ED_RE.match(stem)
    if not m:
        return None
    base = m.group("base")
    # 一层层剥：DSC1-Edit-已增强-降噪-ed 要先去掉降噪，再去掉 -Edit
    changed = True
    while changed:
        changed = False
        low = base.lower()
        for suf in DENOISE_SUFFIXES + EDIT_SUFFIXES:
            if low.endswith(suf.lower()) and len(base) > len(suf):
                base = base[: -len(suf)]
                changed = True
                break
    return base


def cmd_archive(args):
    p = check_root(args.root)
    name = args.batch
    ed_dir = batch_dir(p["ed"], name)
    jpg_dir = batch_dir(p["jpg"], name)
    arc_dir = batch_dir(p["arc"], name)

    if not ed_dir.is_dir():
        die(f"找不到成品目录：{ed_dir}\n"
            f"（这个批次还没有修好的照片。先在 Lightroom 里导出，文件名带 -ed，"
            f"放进这个目录之后再归档。）")

    # 05 里的原图：主名 -> 扩展名 -> [文件]。扩展名参与匹配，避免同主名不同格式被一锅端。
    index = {}
    if jpg_dir.is_dir():
        for f in iter_files(jpg_dir):
            index.setdefault(f.stem.lower(), {}).setdefault(f.suffix.lower(), []).append(f)

    # 99 里已归档的，按「主名 + 扩展名」索引：IMG.png 不能证明 IMG.jpg 已归档。
    # 另外按主名再收一份，用来认出「已归档，只是扩展名不同」这种情况。
    arc_index = set()
    arc_by_stem = {}
    if arc_dir.is_dir():
        for f in iter_files(arc_dir):
            arc_index.add((f.stem.lower(), f.suffix.lower()))
            arc_by_stem.setdefault(f.stem.lower(), set()).add(f.suffix.lower())

    moved, already, missing, orphan = [], [], [], []
    archived_other, redundant, siblings_left = [], [], []
    ambiguous, shared, conflicts = [], [], []
    claimed = {}       # 原图 -> 认领它的成品，保证一张原图只归档一次
    referenced = set() # 被任何成品指向过的原图，用于算「未修」

    def logged_names():
        """
        归档日志里记过的原图文件名。

        用来区分两种「99 里有个同名不同格式的文件」：一种是我们自己上一趟移过去的
        （HEIC 原图配 JPG 成品），另一种来路不明 —— 后者仍然要按「找不到原图」报警。
        """
        names = set()
        try:
            for log_file in p["logs"].glob("archive-*.tsv"):
                with log_file.open(encoding="utf-8", errors="replace") as fh:
                    for line in fh:
                        bits = line.rstrip("\n").split("\t")
                        if len(bits) >= 3:
                            names.add(bits[2].lower())
        except OSError:
            pass
        return names

    logged = logged_names()
    ed_files = sorted(x for x in iter_files(ed_dir) if x.is_file())
    progress(0, len(ed_files), "匹配")
    for i, f in enumerate(ed_files, 1):
        progress(i, len(ed_files), "匹配")
        base = strip_ed(f.stem)
        if base is None:
            orphan.append(f)          # 03 里不带 -ed 的文件
            continue

        groups = index.get(base.lower(), {})
        ext = f.suffix.lower()

        if ext in groups:
            cand = groups[ext]
        else:
            others = [x for lst in groups.values() for x in lst]
            if len(others) > 1:
                referenced.update(others)
                ambiguous.append((f, others, "扩展名不一致，且有多个同名候选"))
                continue
            cand = others             # 只有一个候选，扩展名不同，下面会标注

        if len(cand) > 1:
            referenced.update(cand)
            ambiguous.append((f, cand, "同名同扩展名，但原图目录里有多个文件"))
            continue

        if not cand:
            if (base.lower(), ext) in arc_index:
                already.append((f, base))
            elif base.lower() in arc_by_stem and any(
                    f"{base}{e}".lower() in logged for e in arc_by_stem[base.lower()]):
                # 原图目录里没了，归档里有一份同名不同格式的，而且日志证明确实是我们
                # 自己上一趟移过去的 —— 那是「已归档，只是换了格式」，不该报找不到。
                archived_other.append((f, base, sorted(arc_by_stem[base.lower()])))
            else:
                missing.append((f, base))
            continue

        h = cand[0]
        referenced.add(h)

        # 同一张照片可能同时有 .jpg 和 .heic 两份原图。按「匹配不唯一就不动手」的规矩，
        # 这里只报出来，不自动搬：多格式的导出件也可能是有意留着的。
        # 但要标成「被指向过」，免得它们被算进「未修」里 —— 这些照片明明是修过的。
        for sib in [x for lst in groups.values() for x in lst if x is not h]:
            referenced.add(sib)
            if any(sib == seen for _, seen in siblings_left):
                continue
            siblings_left.append((f, sib))

        if h in claimed:
            shared.append((f, h, claimed[h]))
            continue

        d = arc_dir / h.name
        if d.exists():
            # 已经归档过：内容一致才算完成，不一致说明 99 里那份不是这张原图
            if same_content(h, d):
                claimed[h] = f
                already.append((f, base))
                redundant.append(h)        # 原图目录里这份是重复的，执行时删掉
            else:
                conflicts.append((f, h, d))
            continue

        claimed[h] = f
        note = "" if h.suffix.lower() == ext else f"扩展名不同（原图 {h.suffix} / 成品 {f.suffix}）"
        moved.append((f, h, d, note))

    # 05 里没被任何成品指向的
    untouched = [f for f in iter_files(jpg_dir) if f not in referenced] if jpg_dir.is_dir() else []

    # 清单 = 真正要移动的那些 (源, 目标, 大小, 修改时间)
    token = plan_token([(str(h), str(d)) + stat_sig(h) for f, h, d, note in moved])
    check_plan(getattr(args, "expect_plan", None), token)
    announce_plan(token)

    confirm_apply(args.apply, f"归档批次 {name} 的原图 JPG")
    print(f"待归档 {len(moved):>4} 个")
    print(f"原图目录保留（未修） {len(untouched):>4} 个")
    if already:
        print(f"{C_INFO}已在归档中 {len(already)} 个（重复跑命令不会出错）")
    if redundant:
        print(f"{C_INFO}归档目录里已经有同内容的一份 {len(redundant)} 个"
              f" —— 执行时会删掉原图目录里的重复份：")
        for h in redundant[:20]:
            print(f"       {h.name}")
    if archived_other:
        print(f"\n{C_INFO}原图目录里已经没有了，归档目录里有一份同名的其它格式 "
              f"{len(archived_other)} 个（日志里记着是之前归档过去的）：")
        for f, base, exts in archived_other[:20]:
            print(f"       {f.name}  -> 归档里是 {base}{'、'.join(exts)}")
    if siblings_left:
        print(f"\n{C_WARN}同一张照片在原图目录里还有其它格式的原图 "
              f"{len(siblings_left)} 个 —— 没动，需你确认：")
        for f, sib in siblings_left[:20]:
            print(f"       {sib.name}（成品 {f.name} 对应的那张已经归档，这份还在原图目录里）")
        print("       是同一张照片的另一种格式（比如 HEIC 原图 + JPG 成品）就手动挪进归档目录；")
        print("       是有意留着的导出件就不用管。脚本不替你判断。")
    if shared:
        print(f"{C_INFO}多张成品对应同一张原图 {len(shared)} 组 —— 只归档一次：")
        for f, h, first in shared[:20]:
            print(f"       {f.name} 与 {first.name} 都指向 {h.name}")
    if ambiguous:
        print(f"\n{C_WARN}匹配不唯一 {len(ambiguous)} 个 —— 不自动移动，需你人工确认：")
        for f, cand, why in ambiguous[:20]:
            print(f"       {f.name}：{why}")
            for c in cand[:5]:
                print(f"           候选 {c}")
    if conflicts:
        print(f"\n{C_WARN}冲突：归档目录里已有同名但内容不同的文件 {len(conflicts)} 个 —— 不覆盖：")
        for f, h, d in conflicts[:20]:
            print(f"       原图 {h}")
            print(f"       归档 {d}  （内容不同）")
    if missing:
        print(f"\n{C_WARN}有成品，但原图目录和归档目录里都找不到对应原图 {len(missing)} 个 —— 需你人工确认：")
        for f, base in missing[:30]:
            print(f"       {f.name}  -> 找不到 {base}.*")
    if orphan:
        print(f"\n{C_WARN}成品目录里没有 -ed 后缀的文件 {len(orphan)} 个 —— 不在归档判定范围内：")
        for f in orphan[:30]:
            print(f"       {f}")

    if moved:
        print(f"\n将移动（原图目录 → 归档目录）：")
        for f, h, d, note in moved[:40]:
            extra = f"  [{note}]" if note else ""
            print(f"       {h.name:<32} {human(h.stat().st_size):>9}{extra}")
        if len(moved) > 40:
            print(f"       …… 另有 {len(moved) - 40} 个")

    if not args.apply:
        print(f"\n加 --apply 真正移动。想锁定这份清单，就加 --expect-plan {token}")
        return

    arc_dir.mkdir(parents=True, exist_ok=True)
    log_dir = paths(args.root)["logs"]
    log_dir.mkdir(parents=True, exist_ok=True)
    log_path = log_dir / f"archive-{_dt.date.today():%Y-%m}.tsv"

    errors = skipped = cross_vol = removed = 0
    progress(0, len(moved), "归档")
    with log_path.open("a", encoding="utf-8") as log:
        stamp = _dt.datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        for i, (f, h, d, note) in enumerate(moved, 1):
            if d.exists():            # 规划后被别的进程抢先放了文件
                skipped += 1
                print(f"{C_INFO}目标已存在，跳过 {d.name}")
            else:
                try:
                    how = move_verified(h, d)
                    log.write(f"{stamp}\t{name}\t{h.name}\t{h}\t{d}\n")
                    if how == "copy":
                        cross_vol += 1
                except OSError as e:
                    errors += 1
                    print(f"{C_ERR}{h.name}: {e}")
            progress(i, len(moved), "归档")

        # 归档里已经有同内容的一份，原图目录这份是重复的。
        # 删之前再比一次哈希：这中间要是被换过内容，就留着不动。
        for h in redundant:
            keep = arc_dir / h.name
            try:
                if not same_content(h, keep):
                    raise OSError("内容已变化，未删除")
                h.unlink()
                removed += 1
                log.write(f"{stamp}\t{name}\t{h.name}\t{h}\t{keep}\t"
                          f"归档里已有同内容，删掉原图目录里的重复份\n")
            except OSError as e:
                errors += 1
                print(f"{C_ERR}{h.name}: 删除重复份失败：{e}")

    ok = len(moved) - errors - skipped
    print(f"\n完成：归档 {ok} 个，已在归档中 {len(already)} 个，"
          f"内容冲突 {len(conflicts)} 个，失败 {errors} 个。")
    if removed:
        print(f"（另有 {removed} 个在归档目录里已有同内容的一份，原图目录里的重复份已删除）")
    if cross_vol:
        print(f"（其中 {cross_vol} 个不在同一个卷上，是复制 + 哈希校验 + 删源，不是原子改名）")
    if skipped:
        print(f"（执行期间另有 {skipped} 个目标已被占用，未计入归档）")
    print(f"日志：{log_path}")
    if untouched:
        print(f"原图目录里还剩 {len(untouched)} 个未修原图，继续在相册里可见。")


# ---------------------------------------------------------------- doctor

def cmd_doctor(args):
    p = check_root(args.root)
    issues = 0

    def issue(level, msg):
        nonlocal issues
        issues += 1
        print(f"{level}{msg}")

    print("== 体检：目录一致性与命名 ==\n")

    sets = {k: set(batch_name_list(p[k])) for k in ("raw", "jpg", "ed", "arc")}
    all_names = set().union(*sets.values())

    print(f"批次数量：RAW={len(sets['raw'])}  原图={len(sets['jpg'])}  "
          f"成品={len(sets['ed'])}  归档={len(sets['arc'])}\n")

    # 0. 顶层目录缺失（后面的检查都依赖这些目录，先说明白）
    progress(0, 7, "体检")
    labels = {k: dir_name(k) for k in ("raw", "jpg", "ed", "arc")}
    gone = [k for k in ("jpg", "ed", "arc") if not p[k].is_dir()]
    if gone:
        print("-- 顶层目录缺失 --")
        for k in gone:
            issue(C_WARN, f"{AREA_NAME[k]}（{labels[k]}）不存在，相关检查已跳过")
        print()

    progress(1, 7, "体检")
    # 1. 命名合规
    bad_names = sorted(n for n in all_names if not check_batch_name(n)[0])
    if bad_names:
        print("-- 批次命名 --")
        for n in bad_names:
            _, hint = check_batch_name(n)
            issue(C_WARN, f"{n}：{hint}")
        print()

    progress(2, 7, "体检")
    # 2. 四处不一致
    disagree = []
    for n in sorted(all_names):
        have = [k for k in ("raw", "jpg", "ed", "arc") if n in sets[k]]
        lack = [k for k in ("raw", "jpg", "ed", "arc") if n not in sets[k]]
        # 04/05 应该都有；03/99 只在该批次有修图时才出现，缺失属正常
        must = [k for k in ("raw", "jpg") if k in lack]
        if must:
            disagree.append((n, have, must))
    if disagree:
        print("-- 批次目录不齐 --")
        for n, have, lack in disagree:
            issue(C_WARN, f"{n}：有 {'、'.join(AREA_NAME[k] for k in have) or '无'}，"
                          f"缺 {'、'.join(AREA_NAME[k] for k in lack)}")
        print()

    progress(3, 7, "体检")
    # 3. 03 根目录散落文件
    stray = (sorted(f for f in p["ed"].iterdir() if f.is_file())
             if p["ed"].is_dir() else [])
    if stray:
        print("-- 成品目录根部散落的文件 --")
        for f in stray:
            issue(C_WARN, f"{f.name}  ({human(f.stat().st_size)})  {_dt.datetime.fromtimestamp(f.stat().st_mtime):%Y-%m-%d}")
        print()

    progress(4, 7, "体检")
    # 4. 05 空目录
    empty = []
    if p["jpg"].is_dir():
        for d in sorted(x for x in p["jpg"].iterdir() if x.is_dir()):
            if not any(iter_files(d)):
                empty.append(d)
    if empty:
        print("-- 原图目录里的空目录（可删） --")
        for d in empty:
            issue(C_INFO, str(d))
        print()

    progress(5, 7, "体检")
    # 5. 该归档但没归档的（极相册里会重复的根源）
    pending, only_other = [], []
    for n in sorted(sets["ed"]):
        ed_dir = p["ed"] / n
        jpg_dir = p["jpg"] / n
        if not jpg_dir.is_dir():
            continue
        index = {}
        for f in iter_files(jpg_dir):
            index.setdefault(f.stem.lower(), []).append(f)
        cnt = other = 0
        for f in iter_files(ed_dir):
            base = strip_ed(f.stem)
            cands = index.get(base.lower()) if base else None
            if not cands:
                continue
            # 扩展名对得上才算「这张原图还没归档」；只剩别的格式的，
            # 说明对应格式已经归档过了，只是同名文件还有另一份。
            if any(c.suffix.lower() == f.suffix.lower() for c in cands):
                cnt += 1
            else:
                other += 1
        if cnt:
            pending.append((n, cnt))
        if other:
            only_other.append((n, other))
    if pending:
        print("-- 已修但原图还没归档（相册里会重复显示） --")
        for n, cnt in pending:
            issue(C_WARN, f"{n}：{cnt} 张  ->  python photoflow.py archive {n} --apply")
        print()
    if only_other:
        print("-- 成品对应的原图已归档，但原图目录里还留着别的格式 --")
        for n, cnt in only_other:
            issue(C_INFO, f"{n}：{cnt} 张（同名的 HEIC / TIF 之类还在原图目录里，脚本不自动搬）")
        print()

    progress(6, 7, "体检")
    # 6. 03 里不带 -ed 的文件（按批次）
    odd = []
    for n in sorted(sets["ed"]):
        for f in iter_files(p["ed"] / n):
            if strip_ed(f.stem) is None:
                odd.append(f)
    if odd:
        print("-- 成品目录里不带 -ed 的文件 --")
        for f in odd[:30]:
            issue(C_INFO, str(f))
        print()

    progress(7, 7, "体检")
    if issues == 0:
        print(f"{C_OK}没发现问题。")
    else:
        print(f"共 {issues} 项需要关注。WARN 建议处理，INFO 仅供参考。")


# ---------------------------------------------------------------- GUI

PALETTE = {
    "ink": "#16181D",
    "ink_soft": "#3A3F47",
    "muted": "#6B7280",
    "faint": "#9CA3AF",
    "line": "#E2E4E8",
    "canvas": "#F4F5F7",
    "surface": "#FFFFFF",
    "console": "#FAFAFB",
    "ok": "#1F7A4D",
    "warn": "#A15C00",
    "err": "#A3242B",
}


def _first_font(root, tkfont, candidates):
    try:
        families = set(tkfont.families(root))
    except Exception:                                        # noqa: BLE001
        families = set()
    for name in candidates:
        if name in families:
            return name
    return "TkDefaultFont"


def _apply_theme(tk, ttk, root):
    """
    一套克制的单色主题：白纸 + 墨色，像相机机身。

    只有「执行」这类主操作是实心墨色，其余都是白底描边；颜色只用来表达状态
    （可用/警告/错误），不做装饰。
    """
    from tkinter import font as tkfont

    ui = _first_font(root, tkfont, ["Microsoft YaHei UI", "Microsoft YaHei",
                                    "Segoe UI", "Noto Sans CJK SC",
                                    "PingFang SC", "DejaVu Sans"])
    mono = _first_font(root, tkfont, ["Consolas", "Cascadia Mono",
                                      "DejaVu Sans Mono", "Courier New"])

    st = ttk.Style(root)
    try:
        st.theme_use("clam")
    except Exception:                                        # noqa: BLE001
        pass

    ink, soft, muted, faint = (PALETTE["ink"], PALETTE["ink_soft"],
                               PALETTE["muted"], PALETTE["faint"])
    line, canvas, surface = PALETTE["line"], PALETTE["canvas"], PALETTE["surface"]

    st.configure(".", background=canvas, foreground=ink, font=(ui, 10))
    st.configure("TFrame", background=canvas)
    st.configure("Canvas.TFrame", background=canvas)
    st.configure("Sheet.TFrame", background=surface)

    st.configure("TLabel", background=surface, foreground=ink)
    st.configure("Wordmark.TLabel", background=surface, foreground=ink,
                 font=(ui, 16, "bold"))
    st.configure("Tagline.TLabel", background=surface, foreground=faint,
                 font=(ui, 9))
    st.configure("Title.TLabel", background=surface, foreground=ink,
                 font=(ui, 13, "bold"))
    st.configure("Desc.TLabel", background=surface, foreground=muted,
                 font=(ui, 10))
    st.configure("Field.TLabel", background=surface, foreground=muted,
                 font=(ui, 9))
    st.configure("Hint.TLabel", background=surface, foreground=muted,
                 font=(ui, 9))
    st.configure("Status.TLabel", background=canvas, foreground=faint,
                 font=(ui, 9))

    st.configure("TEntry", fieldbackground=surface, foreground=ink,
                 bordercolor=line, lightcolor=line, darkcolor=line,
                 padding=6, insertcolor=ink)
    st.map("TEntry", bordercolor=[("focus", ink)])

    st.configure("TCombobox", fieldbackground=surface, background=surface,
                 foreground=ink, bordercolor=line, lightcolor=line,
                 darkcolor=line, arrowcolor=soft, padding=5)
    st.map("TCombobox",
           fieldbackground=[("readonly", surface), ("!disabled", surface)],
           bordercolor=[("focus", ink)])
    root.option_add("*TCombobox*Listbox.background", surface)
    root.option_add("*TCombobox*Listbox.foreground", ink)
    root.option_add("*TCombobox*Listbox.selectBackground", ink)
    root.option_add("*TCombobox*Listbox.selectForeground", "#FFFFFF")

    st.configure("Field.TCheckbutton", background=surface, foreground=ink,
                 focuscolor=surface)
    st.map("Field.TCheckbutton", background=[("active", surface)])

    st.configure("Primary.TButton", background=ink, foreground="#FFFFFF",
                 bordercolor=ink, lightcolor=ink, darkcolor=ink,
                 focusthickness=0, relief="flat", padding=(18, 6),
                 font=(ui, 10, "bold"))
    st.map("Primary.TButton",
           background=[("disabled", "#D3D6DB"), ("pressed", "#000000"),
                       ("active", "#2C3038")],
           bordercolor=[("disabled", "#D3D6DB")],
           foreground=[("disabled", "#FFFFFF")])

    # 次要按钮：浅灰底 + 更浅的描边；悬停/按下逐级加深，但始终远浅于主按钮的墨色
    st.configure("Small.TButton", background="#F2F3F5", foreground=ink,
                 bordercolor="#DFE1E5", lightcolor="#F2F3F5", darkcolor="#F2F3F5",
                 focusthickness=0, relief="flat", padding=(8, 3), font=(ui, 9))
    st.map("Small.TButton",
           background=[("pressed", "#D8DBE0"), ("active", "#E6E9ED"),
                       ("disabled", "#FAFAFB")],
           bordercolor=[("active", "#C9CDD4")],
           foreground=[("disabled", faint)])

    st.configure("Secondary.TButton", background="#F2F3F5", foreground=ink,
                 bordercolor="#DFE1E5", lightcolor="#F2F3F5", darkcolor="#F2F3F5",
                 focusthickness=0, relief="flat", padding=(14, 6))
    st.map("Secondary.TButton",
           background=[("pressed", "#D8DBE0"), ("active", "#E6E9ED"),
                       ("disabled", "#FAFAFB")],
           bordercolor=[("active", "#C9CDD4")],
           foreground=[("disabled", faint)])

    st.configure("Card.TNotebook", background=canvas, borderwidth=0,
                 tabmargins=(0, 0, 0, 0))
    st.configure("Card.TNotebook.Tab", background=canvas, foreground=muted,
                 borderwidth=0, padding=(18, 9), font=(ui, 10))
    st.map("Card.TNotebook.Tab",
           background=[("selected", surface)],
           foreground=[("selected", ink)],
           expand=[("selected", (0, 0, 0, 0))])

    for orient in ("Vertical", "Horizontal"):
        st.configure(f"{orient}.TScrollbar", background=canvas, troughcolor=PALETTE["console"],
                     bordercolor=PALETTE["console"], arrowcolor=muted, borderwidth=0)

    # 进度条：极细一条，填充用墨色，底槽接近页签底色，静止时几乎看不见
    st.configure("Thin.Horizontal.TProgressbar",
                 background=ink, troughcolor="#EDEFF2",
                 bordercolor="#EDEFF2", lightcolor=ink, darkcolor=ink,
                 borderwidth=0, thickness=6)
    st.map("Thin.Horizontal.TProgressbar", background=[("disabled", faint)])

    return {"ui": ui, "mono": mono}


def _run_gui(tk, ttk, filedialog, messagebox, initial_root):
    """
    tkinter 界面。

    界面本身不做任何文件操作，只负责把参数拼成一条命令行、调用同一个 photoflow.py，
    再把输出原样显示出来。这样 GUI 和命令行永远走同一套逻辑，不会各有一套实现。
    每个操作都保持「先预演、再执行」：没预演过、或预演后改了输入，「执行」按钮是灰的。
    """
    import queue
    import subprocess
    import threading
    from pathlib import Path

    script = Path(__file__).resolve()

    class App:
        def __init__(self, master, fonts):
            self.master = master
            self.fonts = fonts
            master.title("photoflow · 照片库工作流")
            master.geometry("1000x780")
            master.minsize(900, 680)
            master.columnconfigure(0, weight=1)
            master.rowconfigure(1, weight=1)

            self.busy = False
            self.previewed = {}
            self.apply_buttons = {}
            self._preview_btns = {}
            self.batch_labels = []
            self.titles = {}
            self.bars = {}          # 页签名 -> 进度条
            self.plan_tokens = {}   # 页签名 -> 预演时算出的清单指纹
            self.events = queue.Queue()
            self._pending_refresh = None

            self._build_header()
            self._build_tabs()
            self._build_status()
            self._load_config()
            self.refresh_batches()
            self._poll()

        # ---------------------------------------------------- 顶部

        def _build_header(self):
            wrap = tk.Frame(self.master, background=PALETTE["surface"])
            wrap.grid(row=0, column=0, sticky="ew")

            head = ttk.Frame(wrap, style="Sheet.TFrame", padding=(20, 12, 20, 10))
            head.pack(fill="x")
            head.columnconfigure(1, weight=1)

            ttk.Label(head, text="photoflow", style="Wordmark.TLabel").grid(
                row=0, column=0, sticky="w")

            # 目录状态直接跟在标题后面，不再单独占一行
            self.root_hint = ttk.Label(head, text="", style="Hint.TLabel")
            self.root_hint.grid(row=0, column=1, columnspan=3, sticky="w", padx=(12, 0))

            ttk.Label(head, text="照片库根目录", style="Field.TLabel", width=11).grid(
                row=1, column=0, sticky="w", pady=(10, 0))
            self.root_var = tk.StringVar(value=initial_root)
            ttk.Entry(head, textvariable=self.root_var).grid(
                row=1, column=1, columnspan=3, sticky="ew", pady=(10, 0))
            ttk.Button(head, text="浏览…", style="Small.TButton", width=0,
                       command=self._pick_root).grid(row=1, column=4, padx=(8, 0), pady=(10, 0))
            ttk.Button(head, text="打开", style="Small.TButton", width=0,
                       command=self._open_root).grid(row=1, column=5, padx=(8, 0), pady=(10, 0))

            # 当前批次：整个窗口只有这一处能改，三个页签都读它，
            # 避免在每个页签各选一次、选串了。
            ttk.Label(head, text="当前批次", style="Field.TLabel", width=11).grid(
                row=2, column=0, sticky="w", pady=(6, 0))
            self.batch_var = tk.StringVar()
            self.batch_box = ttk.Combobox(head, textvariable=self.batch_var,
                                          postcommand=self.refresh_batches)
            self.batch_box.grid(row=2, column=1, columnspan=3, sticky="ew", pady=(6, 0))
            self.batch_var.trace_add("write", lambda *a: self._on_batch_changed())
            ttk.Button(head, text="刷新", style="Small.TButton", width=0,
                       command=self.refresh_batches).grid(row=2, column=4, padx=(8, 0), pady=(6, 0))

            self.root_var.trace_add("write", lambda *a: self._on_root_changed())

            tk.Frame(wrap, height=1, background=PALETTE["line"]).pack(fill="x")

        def _on_root_changed(self):
            for name in ("new", "ingest", "archive"):
                self._invalidate(name)
            raw = self.root_var.get().strip().strip('"')
            if not raw:
                self.root_hint.configure(text="")
                return
            if (Path(raw) / dir_name("raw")).is_dir():
                self.root_hint.configure(text="✓ 目录可用", foreground=PALETTE["ok"])
                if self._pending_refresh:
                    self.master.after_cancel(self._pending_refresh)
                self._pending_refresh = self.master.after(500, self.refresh_batches)
            else:
                self.root_hint.configure(
                    text=f"✗ 找不到 RAW 目录（{dir_name('raw')}），检查路径或网络盘是否连上",
                    foreground=PALETTE["err"])

        def _on_batch_changed(self):
            for name in ("new", "ingest", "archive"):
                self._invalidate(name)
            cur = self.batch_var.get().strip()
            shown = cur or "（还没填）"
            for lbl, prefix in self.batch_labels:
                lbl.configure(text=f"{prefix}：{shown}")

        def _batch_label(self, parent, prefix):
            """页签里只读显示当前批次，改要去上方那一行改。"""
            lbl = ttk.Label(parent, text="", style="Desc.TLabel")
            lbl.pack(anchor="w", pady=(6, 12))
            self.batch_labels.append((lbl, prefix))
            self._on_batch_changed()
            return lbl

        def _current_batch(self):
            name = self.batch_var.get().strip()
            if not name:
                messagebox.showwarning(
                    "缺少批次", "先去上方「当前批次」填一个，例如 2026-11-26-梧桐山。")
                return None
            return name

        def _pick_root(self):
            cur = self.root_var.get().strip()
            chosen = filedialog.askdirectory(
                title="选择照片库根目录", initialdir=cur if Path(cur).is_dir() else "")
            if chosen:
                self.root_var.set(chosen)

        def _open_root(self):
            raw = self.root_var.get().strip().strip('"')
            if not Path(raw).is_dir():
                messagebox.showwarning("打不开", "这个路径不存在。")
                return
            try:
                os.startfile(raw)          # 仅 Windows
            except AttributeError:
                messagebox.showinfo("提示", "当前系统不支持在文件管理器里直接打开。")
            except OSError as e:
                messagebox.showerror("打不开", str(e))

        # ---------------------------------------------------- 四个页签

        def _build_tabs(self):
            nb = ttk.Notebook(self.master, style="Card.TNotebook")
            nb.grid(row=1, column=0, sticky="nsew", padx=20, pady=10)
            self._tab_new(nb)
            self._tab_ingest(nb)
            self._tab_archive(nb)
            self._tab_doctor(nb)
            self._tab_settings(nb)

        def _title(self, parent, text, sub=None):
            ttk.Label(parent, text=text, style="Title.TLabel").pack(anchor="w")
            if sub:
                ttk.Label(parent, text=sub, style="Desc.TLabel",
                          wraplength=820, justify="left").pack(anchor="w", pady=(6, 18))
            else:
                ttk.Frame(parent, style="Sheet.TFrame", height=14).pack()

        def _output(self, parent):
            box = ttk.Frame(parent, style="Sheet.TFrame")
            box.pack(fill="both", expand=True, pady=(16, 0))
            box.columnconfigure(0, weight=1)
            box.rowconfigure(0, weight=1)
            txt = tk.Text(box, height=16, wrap="none",
                          font=(self.fonts["mono"], 10),
                          background=PALETTE["console"], foreground=PALETTE["ink_soft"],
                          relief="flat", borderwidth=0,
                          highlightthickness=1, highlightbackground=PALETTE["line"],
                          highlightcolor=PALETTE["line"],
                          padx=14, pady=12, insertbackground=PALETTE["ink"])
            ysb = ttk.Scrollbar(box, orient="vertical", command=txt.yview)
            xsb = ttk.Scrollbar(box, orient="horizontal", command=txt.xview)
            txt.configure(yscrollcommand=ysb.set, xscrollcommand=xsb.set)
            txt.grid(row=0, column=0, sticky="nsew")
            ysb.grid(row=0, column=1, sticky="ns")
            xsb.grid(row=1, column=0, sticky="ew")
            txt.insert("1.0", "先点「预演」看清单，确认无误后再点「执行」。")
            txt.configure(state="disabled")
            return txt

        def _progressbar(self, name, parent):
            """
            每个页签最底下的一条细进度条，静止时几乎看不见。

            必须在 _buttons 之前 pack：pack 的 bottom 是谁先占谁更靠下，
            先放的这条才贴在页签最底部，按钮落在它上面。

            高度靠固定高度的外框压住：clam 主题下进度条的 thickness 改了也没用，
            控件请求高度永远是 18px，只有 pack_propagate(False) 才压得下来。

            而且不能压得太狠：clam 的填充条上下各留 3px 描边，外框 6px 时中间的
            墨色刚好被裁没，看着像一直没动。10px 才剩下 4px 实心，既细又看得见。
            """
            wrap = tk.Frame(parent, background=PALETTE["surface"], height=10)
            wrap.pack(side="bottom", fill="x", pady=(12, 0))
            wrap.pack_propagate(False)
            bar = ttk.Progressbar(wrap, style="Thin.Horizontal.TProgressbar",
                                  mode="determinate", maximum=100, value=0)
            bar.pack(fill="both", expand=True)
            self.bars[name] = bar
            return bar

        def _buttons(self, parent, name, with_apply=True):
            """
            先在底部占好位置，再让上面的输出框去填剩下的空间。

            顺序反过来（先放输出框再放按钮）时，输出框的 expand 会把底部挤没，
            按钮就跑到窗口外面去了。
            """
            row = ttk.Frame(parent, style="Sheet.TFrame")
            row.pack(side="bottom", fill="x", pady=(18, 0))
            prev_style = "Secondary.TButton" if with_apply else "Primary.TButton"
            prev = ttk.Button(row, text="预演" if with_apply else "运行", style=prev_style)
            prev.pack(side="left")
            self._preview_btns[name] = prev
            if with_apply:
                btn = ttk.Button(row, text="执行", style="Primary.TButton")
                btn.state(["disabled"])
                btn.pack(side="left", padx=10)
                self.apply_buttons[name] = btn

        def _bind_actions(self, name, out, args_fn):
            self._preview_btns[name].configure(
                command=lambda: self._do_preview(name, args_fn(), out))
            if name in self.apply_buttons:
                self.apply_buttons[name].configure(
                    command=lambda: self._do_apply(name, args_fn(), out))

        def _tab_new(self, nb):
            f = ttk.Frame(nb, padding=22, style="Sheet.TFrame")
            nb.add(f, text="   建批次   ")
            self.titles["new"] = "建批次"
            self._title(f, "建一个批次",
                        "批次名在上方「当前批次」里填，格式 YYYY-MM-DD-地点，例如 2026-11-26-梧桐山。"
                        "这里只负责按这个名字一次建好 RAW、原图、成品、归档四个同名目录。")

            self._batch_label(f, "将创建的批次")

            self._progressbar("new", f)
            self._buttons(f, "new")
            out = self._output(f)
            self._bind_actions("new", out, self._args_new)

        def _tab_ingest(self, nb):
            f = ttk.Frame(nb, padding=22, style="Sheet.TFrame")
            nb.add(f, text="   从卡分发   ")
            self.titles["ingest"] = "从卡分发"
            self._title(f, "把卡里的照片分发进照片库",
                        "相机 RAW 进 RAW 目录，JPG / HEIC / PNG / TIFF 进原图目录，视频等其余文件跳过。"
                        "同名文件会先比大小、再比 SHA-256，内容不同就报冲突，不会覆盖。")

            ttk.Label(f, text="源目录", style="Field.TLabel").pack(anchor="w")
            row1 = ttk.Frame(f, style="Sheet.TFrame")
            row1.pack(fill="x", pady=(4, 12))
            self.ingest_src = tk.StringVar()
            self.ingest_src.trace_add("write", lambda *a: self._invalidate("ingest"))
            ttk.Entry(row1, textvariable=self.ingest_src).pack(
                side="left", fill="x", expand=True)
            ttk.Button(row1, text="浏览…", width=8,
                       command=self._pick_src).pack(side="left", padx=6)

            self._batch_label(f, "目标批次（跟随上方「当前批次」）")

            self.ingest_dedupe = tk.BooleanVar(value=False)
            self.ingest_dedupe.trace_add("write", lambda *a: self._invalidate("ingest"))
            ttk.Checkbutton(
                f, variable=self.ingest_dedupe, style="Field.TCheckbutton",
                text="卡内同名但内容不同时自动加 _2 后缀（不勾选则报冲突、不导入）"
            ).pack(anchor="w", pady=(4, 0))

            self._progressbar("ingest", f)
            self._buttons(f, "ingest")
            out = self._output(f)
            self._bind_actions("ingest", out, self._args_ingest)

        def _tab_archive(self, nb):
            f = ttk.Frame(nb, padding=22, style="Sheet.TFrame")
            nb.add(f, text="   归档   ")
            self.titles["archive"] = "归档"
            self._title(f, "把已修照片的原图移进归档",
                        "按成品目录里的 -ed 成品，反查原图目录里的同名原图，移进归档目录。"
                        "移完之后这张照片在相册里只剩成品一份，原图退出时间线。没修的原图不动。")

            self._batch_label(f, "要归档的批次（跟随上方「当前批次」）")

            self._progressbar("archive", f)
            self._buttons(f, "archive")
            out = self._output(f)
            self._bind_actions("archive", out, self._args_archive)

        def _tab_doctor(self, nb):
            f = ttk.Frame(nb, padding=22, style="Sheet.TFrame")
            nb.add(f, text="   体检   ")
            self.titles["doctor"] = "体检"
            self._title(f, "全库体检",
                        "只读，不改任何文件。检查批次命名、四个目录是否对齐、成品目录里散落的文件、"
                        "原图目录里的空目录，以及最关键的「已修但原图还没归档」。")

            self._progressbar("doctor", f)
            self._buttons(f, "doctor", with_apply=False)
            out = self._output(f)
            self._bind_actions("doctor", out, self._args_doctor)

        def _tab_settings(self, nb):
            f = ttk.Frame(nb, padding=22, style="Sheet.TFrame")
            nb.add(f, text="   设置   ")
            self.titles["settings"] = "设置"
            self._title(f, "目录设置",
                        "这里改的是脚本去认哪四个文件夹，不会重命名或移动已有的文件夹。"
                        "改完必须保证照片库里确实叫这些名字，否则所有命令都会找不到目录。")

            grid = ttk.Frame(f, style="Sheet.TFrame")
            grid.pack(fill="x", pady=(0, 8))
            grid.columnconfigure(1, weight=1)

            self.dir_vars = {}
            for row, key in enumerate(("raw", "jpg", "ed", "arc")):
                ttk.Label(grid, text=AREA_NAME[key], style="Field.TLabel",
                          width=10).grid(row=row, column=0, sticky="w", pady=5)
                var = tk.StringVar(value=dir_name(key))
                self.dir_vars[key] = var
                ttk.Entry(grid, textvariable=var, font=(self.fonts["mono"], 10)).grid(
                    row=row, column=1, sticky="ew", pady=5)

            self.settings_hint = ttk.Label(f, text="", style="Desc.TLabel",
                                           wraplength=820, justify="left")
            self.settings_hint.pack(anchor="w", pady=(4, 14))

            row = ttk.Frame(f, style="Sheet.TFrame")
            row.pack(fill="x")
            # 保存是这一页唯一会写文件的操作，单独推到最右边；另两个次要按钮留在左边。
            ttk.Button(row, text="保存", style="Primary.TButton",
                       command=self._save_dirs).pack(side="right")
            ttk.Button(row, text="恢复默认", style="Secondary.TButton",
                       command=self._reset_dirs).pack(side="left")
            ttk.Button(row, text="打开配置文件", style="Secondary.TButton",
                       command=self._open_config).pack(side="left", padx=(10, 0))

            ttk.Label(f, text=f"配置文件：{CONFIG_PATH}", style="Tagline.TLabel"
                      ).pack(anchor="w", pady=(18, 0))

        def _save_dirs(self):
            new = {}
            for key, var in self.dir_vars.items():
                val = var.get().strip()
                err = check_dir_setting(val)
                if err:
                    self.settings_hint.configure(
                        text=f"{AREA_NAME[key]}：{err}", foreground=PALETTE["err"])
                    return
                new[key] = os.path.normpath(val)

            if len({v.lower() for v in new.values()}) != len(new):
                self.settings_hint.configure(
                    text="四个目录不能重名，否则文件会互相覆盖。", foreground=PALETTE["err"])
                return

            _CONFIG["dirs"].update(new)
            _CONFIG["root"] = self.root_var.get().strip()
            _CONFIG["batch"] = self.batch_var.get().strip()
            try:
                save_config()
            except OSError as e:
                self.settings_hint.configure(text=f"保存失败：{e}", foreground=PALETTE["err"])
                return

            self.settings_hint.configure(text="已保存。", foreground=PALETTE["ok"])
            self._on_root_changed()
            self.refresh_batches()
            self.set_status("目录设置已保存")

        def _reset_dirs(self):
            for key, var in self.dir_vars.items():
                var.set(DEFAULT_DIRS[key])
            self.settings_hint.configure(text="已恢复默认值，点「保存」才会生效。",
                                         foreground=PALETTE["warn"])

        def _open_config(self):
            try:
                os.startfile(str(CONFIG_PATH))
            except AttributeError:
                messagebox.showinfo("配置文件", str(CONFIG_PATH))
            except OSError as e:
                messagebox.showerror("打不开", str(e))

        # ---------------------------------------------------- 参数拼装

        def _args_new(self):
            name = self._current_batch()
            if name is None:
                return None
            return ["new", name]

        def _args_ingest(self):
            src = self.ingest_src.get().strip().strip('"')
            batch = self._current_batch()
            if batch is None:
                return None
            if not src:
                messagebox.showwarning("信息不全", "源目录要填。")
                return None
            if not Path(src).is_dir():
                messagebox.showwarning("源目录不存在", f"找不到：{src}")
                return None
            args = ["ingest", src, batch]
            if self.ingest_dedupe.get():
                args.append("--dedupe")
            return args

        def _args_archive(self):
            batch = self._current_batch()
            if batch is None:
                return None
            return ["archive", batch]

        def _args_doctor(self):
            return ["doctor"]

        def _pick_src(self):
            cur = self.ingest_src.get().strip().strip('"')
            chosen = filedialog.askdirectory(
                title="选择相机存储卡或已拷到本机的目录",
                initialdir=cur if Path(cur).is_dir() else "")
            if chosen:
                self.ingest_src.set(chosen)

        # ---------------------------------------------------- 预演 / 执行

        def _invalidate(self, name):
            self.previewed[name] = None
            btn = self.apply_buttons.get(name)
            if btn is not None:
                btn.state(["disabled"])

        def _do_preview(self, name, args, out):
            if args is None:
                return
            self.plan_tokens.pop(name, None)

            def done(code):
                btn = self.apply_buttons.get(name)
                if code == 0:
                    self.previewed[name] = tuple(args)
                    if btn is not None:
                        btn.state(["!disabled"])
                else:
                    self.previewed[name] = None
                    if btn is not None:
                        btn.state(["disabled"])

            self.run_cli(name, args, out, done)

        def _do_apply(self, name, args, out):
            if args is None:
                return
            if self.previewed.get(name) != tuple(args):
                messagebox.showwarning(
                    "请先预演", "输入改过，或者还没有预演过。\n请先点「预演」看清清单。")
                return

            token = self.plan_tokens.get(name)
            if not token:
                messagebox.showwarning(
                    "请先预演",
                    "没拿到这次的清单指纹，无法确认要动哪些文件。\n请重新点「预演」。")
                return

            if not messagebox.askyesno(
                    "确认执行",
                    f"即将真正执行「{self.titles[name]}」，这会改动照片库里的文件。\n\n"
                    f"只执行刚才预演确认过的那份清单；如果这期间目录里多了、少了或\n"
                    f"有文件被改动，程序会停下来要求重新预演。\n\n确定继续？"):
                return
            self._invalidate(name)
            self.run_cli(name, args, out, None, apply=True, expect_plan=token)

        # ---------------------------------------------------- 跑命令

        def run_cli(self, name, args, out, on_done=None, apply=False,
                    expect_plan=None):
            if self.busy:
                messagebox.showinfo("请稍候", "上一个操作还没跑完。")
                return
            root = self.root_var.get().strip().strip('"')
            if not root:
                messagebox.showwarning("缺少根目录", "请先填写照片库根目录。")
                return

            cmd = [sys.executable, "-I", str(script), *args,
                   "--root", root, "--progress"]
            if expect_plan:
                cmd += ["--expect-plan", expect_plan]
            if apply:
                cmd.append("--apply")

            bar = self.bars[name]
            bar.configure(value=0)
            self.busy = True
            self.set_status("运行中…")
            header = "photoflow " + " ".join(args) + ("  --apply" if apply else "")
            out.configure(state="normal")
            out.delete("1.0", "end")
            out.insert("end", "> " + header + "\n\n")
            out.configure(state="disabled")

            def worker():
                """
                逐行读子进程输出：普通文字实时进控制台，进度行只走事件队列。

                Tk 控件只能在主线程碰，所以这里一个控件都不动，全部塞进队列，
                由主线程的 _poll 去更新界面。
                """
                try:
                    proc = subprocess.Popen(
                        cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                        text=True, encoding="utf-8", errors="replace", bufsize=1)
                except Exception as e:                      # noqa: BLE001
                    self.events.put(("done", out, bar, f"启动失败：{e}\n", -1, on_done))
                    return
                try:
                    for line in proc.stdout:
                        if line.startswith(PLAN_TAG):
                            bits = line.rstrip("\r\n").split("\t")
                            if len(bits) > 1:
                                self.events.put(("plan", name, bits[1].strip()))
                            continue
                        if line.startswith(PROGRESS_TAG):
                            bits = line.rstrip("\r\n").split("\t")
                            if len(bits) >= 3:
                                try:
                                    done, total = int(bits[1]), int(bits[2])
                                except ValueError:
                                    continue
                                label = bits[3] if len(bits) > 3 else ""
                                self.events.put(("prog", bar, done, total, label))
                            continue
                        self.events.put(("text", out, line))
                except Exception as e:                      # noqa: BLE001
                    self.events.put(("text", out, f"\n读取输出出错：{e}\n"))
                finally:
                    try:
                        proc.stdout.close()
                    except Exception:                       # noqa: BLE001
                        pass
                self.events.put(("done", out, bar, "", proc.wait(), on_done))

            threading.Thread(target=worker, daemon=True).start()

        def _poll(self):
            # 一次最多处理这么多事件：几万行输出时不能把界面卡住
            for _ in range(400):
                try:
                    ev = self.events.get_nowait()
                except queue.Empty:
                    break

                if ev[0] == "plan":
                    _, pname, token = ev
                    self.plan_tokens[pname] = token
                    continue

                if ev[0] == "prog":
                    _, bar, done, total, label = ev
                    pct = 100.0 * done / total if total else 0.0
                    bar.configure(value=100.0 if pct > 100 else pct)
                    if label:
                        self.set_status(f"{label} {done}/{total}（{pct:.0f}%）")
                    continue

                if ev[0] == "text":
                    _, w, line = ev
                    w.configure(state="normal")
                    w.insert("end", line)
                    w.see("end")
                    w.configure(state="disabled")
                    continue

                _, w, bar, tail, code, on_done = ev
                if tail:
                    w.configure(state="normal")
                    w.insert("end", tail)
                    w.see("end")
                    w.configure(state="disabled")
                self.busy = False
                if code == 0:
                    bar.configure(value=100)
                    self.set_status("完成")
                else:
                    self.set_status(f"结束，退出码 {code}")
                if on_done:
                    on_done(code)
            self.master.after(80, self._poll)

        # ---------------------------------------------------- 杂项

        def refresh_batches(self):
            self._pending_refresh = None
            raw = self.root_var.get().strip().strip('"')
            names = set()
            if raw:
                p = paths(raw)
                for key in ("jpg", "raw", "ed"):
                    names.update(batch_name_list(p[key]))
            values = sorted(names, reverse=True)
            self.batch_box["values"] = values

        def set_status(self, text):
            self.status_var.set(text)

        def _build_status(self):
            self.status_var = tk.StringVar(value="就绪")
            bar = tk.Frame(self.master, background=PALETTE["canvas"])
            bar.grid(row=2, column=0, sticky="ew")
            tk.Frame(bar, height=1, background=PALETTE["line"]).pack(fill="x")
            inner = ttk.Frame(bar, style="Canvas.TFrame", padding=(24, 7))
            inner.pack(fill="x")
            ttk.Label(inner, textvariable=self.status_var,
                      style="Status.TLabel").pack(side="left")

        def _load_config(self):
            saved = _CONFIG.get("root")
            if saved and Path(saved).is_dir():
                self.root_var.set(saved)
            else:
                self._on_root_changed()
            if _CONFIG.get("batch"):
                self.batch_var.set(_CONFIG["batch"])

        def _save_config(self):
            try:
                _CONFIG["root"] = self.root_var.get().strip()
                _CONFIG["batch"] = self.batch_var.get().strip()
                save_config()
            except Exception:                                # noqa: BLE001
                pass

    try:
        root = tk.Tk()
    except Exception as e:                                   # noqa: BLE001
        die(f"打不开窗口：{e}\n"
            "（没有图形环境的话，请改用命令行：photoflow new / ingest / archive / doctor）")
    try:
        # 高 DPI 屏上别让界面糊成一片；失败就算了
        import ctypes
        ctypes.windll.shcore.SetProcessDpiAwareness(1)
    except Exception:                                        # noqa: BLE001
        pass
    fonts = _apply_theme(tk, ttk, root)
    root.configure(background=PALETTE["canvas"])

    app = App(root, fonts)
    root.protocol("WM_DELETE_WINDOW", lambda: (app._save_config(), root.destroy()))
    root.mainloop()


def cmd_gui(args):
    try:
        import tkinter as tk
        from tkinter import ttk, filedialog, messagebox
    except Exception as e:                                   # noqa: BLE001
        die("打不开图形界面：这台机器的 Python 没有可用的 tkinter。\n"
            f"  {e}\n"
            "命令行用法完全不受影响：photoflow new / ingest / archive / doctor")
    _run_gui(tk, ttk, filedialog, messagebox, args.root)


# ---------------------------------------------------------------- CLI

def build_parser():
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--root", default=None,
                        help="照片库根目录（默认读配置文件，其次 "
                             f"{DEFAULT_ROOT}）")
    common.add_argument("--progress", action="store_true",
                        help="输出机器可读的进度行（图形界面用，命令行不需要）")
    common.add_argument("--expect-plan", default=None, metavar="指纹",
                        help="只有图形界面会传：把预演时的清单指纹带过来，"
                             "执行前核对清单有没有变过")

    ap = argparse.ArgumentParser(
        prog="photoflow",
        description="照片库工作流助手：建批次 / 分发 / 归档 / 体检 / 图形界面",
        epilog=("会改动文件的命令默认只预演，确认清单后加 --apply 执行。\n"
                "当前目录设置：RAW 目录 = {}，原图目录 = {}，成品目录 = {}，归档目录 = {}。"
                "可以在 `photoflow gui` 的「设置」里改。").format(
                    dir_name("raw"), dir_name("jpg"), dir_name("ed"), dir_name("arc")),
    )
    sub = ap.add_subparsers(dest="cmd", required=False)

    s = sub.add_parser("gui", parents=[common], help="打开图形界面")
    s.set_defaults(func=cmd_gui)

    s = sub.add_parser("new", parents=[common], help="为一个批次建好 RAW / 原图 / 成品 / 归档四个同名目录")
    s.add_argument("batch", help="批次名，如 2026-11-25-香炉石")
    s.add_argument("--force", action="store_true", help="批次名不合规也照建")
    s.add_argument("--apply", action="store_true",
                   help="确认清单无误后真正执行（不加只预演）")
    s.set_defaults(func=cmd_new)

    s = sub.add_parser("ingest", parents=[common], help="把卡里的照片按 RAW/JPG 分发到 RAW 目录和原图目录")
    s.add_argument("src", help="源目录，如 F:\\DCIM")
    s.add_argument("batch", help="批次名")
    s.add_argument("--apply", action="store_true",
                   help="确认清单无误后真正执行（不加只预演）")
    s.add_argument("--dedupe", action="store_true",
                   help="卡内同名但内容不同时，自动加 _2/_3 后缀而不是跳过")
    s.add_argument("--verify", action="store_true",
                   help="复制完再逐个比 SHA-256（默认只比大小）；能把卡重读一遍，会明显变慢")
    s.set_defaults(func=cmd_ingest)

    s = sub.add_parser("archive", parents=[common], help="把已修照片的 JPG 原图移进归档目录")
    s.add_argument("batch", help="批次名")
    s.add_argument("--apply", action="store_true",
                   help="确认清单无误后真正执行（不加只预演）")
    s.set_defaults(func=cmd_archive)

    s = sub.add_parser("doctor", parents=[common], help="体检：命名、目录对齐、漏归档、空目录")
    s.set_defaults(func=cmd_doctor)

    return ap


def _report_crash(text):
    """
    没有控制台时（bat 用 pythonw 启动），异常的 traceback 会彻底消失。

    所以先写一份日志，再尽量弹个窗告诉用户日志在哪。
    """
    path = CONFIG_PATH.parent / "photoflow-error.log"
    try:
        with open(path, "a", encoding="utf-8") as fh:
            fh.write(f"\n===== {_dt.datetime.now():%Y-%m-%d %H:%M:%S} =====\n{text}\n")
    except OSError:
        return
    try:
        import tkinter as tk
        from tkinter import messagebox
        r = tk.Tk()
        r.withdraw()
        messagebox.showerror("photoflow 出错了", f"详细报错已写入：\n{path}")
        r.destroy()
    except Exception:                                    # noqa: BLE001
        pass


def main():
    load_config()
    ap = build_parser()
    args = ap.parse_args()
    if not getattr(args, "root", None):
        args.root = _CONFIG["root"] or DEFAULT_ROOT
    if not getattr(args, "cmd", None):
        # 不带参数就是开界面：双击 bat、双击 py、命令行敲 photoflow 都走这里
        args.func = cmd_gui
    _PROGRESS["on"] = bool(getattr(args, "progress", False))
    try:
        args.func(args)
    except Exception:                                    # noqa: BLE001
        if sys.stderr is None:                           # pythonw：没有控制台可打
            _report_crash(traceback.format_exc())
        raise


if __name__ == "__main__":
    main()
