#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
photoflow — 极空间摄影工作流助手

目录约定（与现有结构完全一致，不新增、不改名）：
  <root>/04_RAW原图/<批次>/           相机 RAW 底片
  <root>/05_JPG图/<批次>/             待处理 JPG 工作区
  <root>/03_缓存-调色ed/<批次>/       修图成品，文件名带 -ed
  <root>/99_图库/原图JPG归档/<批次>/  原图 JPG 永久归档

所有会改动文件的命令默认只做预演（dry-run），确认清单无误后加 --apply 才真正执行。

用法示例：
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
import hashlib
import os
import re
import shutil
import sys
from pathlib import Path

# 重定向到文件时用 utf-8，直接输出到控制台时保留 Windows 的 Unicode 控制台，避免中文乱码
for _stream in (sys.stdout, sys.stderr):
    try:
        if not _stream.isatty():
            _stream.reconfigure(encoding="utf-8")
    except Exception:
        pass

DEFAULT_ROOT = r"J:\sata11-17665442547\05_相册\my摄影照片-合集"

DIR_RAW = "04_RAW原图"
DIR_JPG = "05_JPG图"
DIR_ED = "03_缓存-调色ed"
DIR_ARC = os.path.join("99_图库", "原图JPG归档")

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


# ---------------------------------------------------------------- 基础设施

def paths(root):
    r = Path(root)
    return {
        "raw": r / DIR_RAW,
        "jpg": r / DIR_JPG,
        "ed": r / DIR_ED,
        "arc": r / DIR_ARC,
        "logs": r / DIR_ARC / "_logs",
    }


def check_root(root):
    p = paths(root)
    if not p["raw"].is_dir():
        die(f"找不到目录：{p['raw']}\n"
            f"J 盘没挂载？或者用 --root 指定正确的根目录。")
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

    confirm_apply(args.apply, f"为批次 {name} 建 4 个同名目录")
    created = existed = 0
    for label, path in targets:
        if path.is_dir():
            print(f"{C_INFO}{label:<4} 已存在  {path}")
            existed += 1
        else:
            print(f"{C_OK}{label:<4} 新建    {path}")
            if args.apply:
                path.mkdir(parents=True, exist_ok=True)
            created += 1

    print(f"\n新建 {created} 个，已存在 {existed} 个。")
    if not args.apply and created:
        print("加 --apply 真正创建。")


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
            "dup_same": [], "dup_diff": [], "renamed": []}
    families = {}       # (kind, 文件名小写) -> [[源文件, 目标路径], ...]，同名文件的全部内容版本
    used_names = set()  # (kind, 目标文件名小写)，避免 _2/_3 撞名

    def alt_name(kind, dst_dir, f):
        """卡内同名但内容不同：加 _2/_3 后缀，避免覆盖或丢片。"""
        n = 2
        while True:
            cand = dst_dir / f"{f.stem}_{n}{f.suffix}"
            if not cand.exists() and (kind, cand.name.lower()) not in used_names:
                return cand
            n += 1

    for f in sorted(iter_files(src), key=lambda p: str(p).lower()):
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

    total_bytes = sum(f.stat().st_size for f, _ in plan["raw"] + plan["jpg"])

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

    if not args.apply:
        print("\n加 --apply 真正复制。")
        return

    errors = 0
    for kind, label, dst_dir in (("raw", "RAW", dst_raw), ("jpg", "JPG", dst_jpg)):
        dst_dir.mkdir(parents=True, exist_ok=True)
        for i, (s, d) in enumerate(plan[kind], 1):
            try:
                shutil.copy2(s, d)
                if d.stat().st_size != s.stat().st_size:
                    d.unlink()
                    raise OSError("大小校验不一致")
            except OSError as e:
                errors += 1
                print(f"{C_ERR}{s.name}: {e}")
                continue
            if i % 50 == 0:
                print(f"       {label} {i}/{len(plan[kind])} ...")
    print(f"\n完成：RAW {len(plan['raw'])} 个，JPG {len(plan['jpg'])} 个，失败 {errors} 个。")
    if errors:
        print("失败项未复制，请检查网络连接后重跑（已复制的会自动跳过）。")


# ---------------------------------------------------------------- archive

# Lightroom 的 AI 降噪会派生 `原名-已增强-降噪.dng`（英文界面是 -Enhanced-NR），
# 之后从这张 DNG 导出的 JPG 也会带着这个中缀，匹配原图前要先把它剥掉，
# 否则 DSC07287-已增强-降噪-ed.JPG 永远对不上 DSC07287.JPG。
DENOISE_SUFFIXES = ("-已增强-降噪", "-Enhanced-NR")


def strip_ed(stem):
    m = ED_RE.match(stem)
    if not m:
        return None
    base = m.group("base")
    low = base.lower()
    for suf in DENOISE_SUFFIXES:
        if low.endswith(suf.lower()):
            return base[: -len(suf)]
    return base


def cmd_archive(args):
    p = check_root(args.root)
    name = args.batch
    ed_dir = batch_dir(p["ed"], name)
    jpg_dir = batch_dir(p["jpg"], name)
    arc_dir = batch_dir(p["arc"], name)

    if not ed_dir.is_dir():
        die(f"找不到成品目录：{ed_dir}\n"
            f"（03 里还没有这个批次？先导出成品再归档。）")

    # 05 里的原图：主名 -> 扩展名 -> [文件]。扩展名参与匹配，避免同主名不同格式被一锅端。
    index = {}
    if jpg_dir.is_dir():
        for f in iter_files(jpg_dir):
            index.setdefault(f.stem.lower(), {}).setdefault(f.suffix.lower(), []).append(f)

    # 99 里已归档的，按「主名 + 扩展名」索引：IMG.png 不能证明 IMG.jpg 已归档
    arc_index = set()
    if arc_dir.is_dir():
        for f in iter_files(arc_dir):
            arc_index.add((f.stem.lower(), f.suffix.lower()))

    moved, already, missing, orphan = [], [], [], []
    ambiguous, shared, conflicts = [], [], []
    claimed = {}       # 原图 -> 认领它的成品，保证一张原图只归档一次
    referenced = set() # 被任何成品指向过的原图，用于算「未修」

    for f in sorted(x for x in iter_files(ed_dir) if x.is_file()):
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
            ambiguous.append((f, cand, "同名同扩展名，但 05 里有多个文件"))
            continue

        if not cand:
            if (base.lower(), ext) in arc_index:
                already.append((f, base))
            else:
                missing.append((f, base))
            continue

        h = cand[0]
        referenced.add(h)

        if h in claimed:
            shared.append((f, h, claimed[h]))
            continue

        d = arc_dir / h.name
        if d.exists():
            # 已经归档过：内容一致才算完成，不一致说明 99 里那份不是这张原图
            if same_content(h, d):
                claimed[h] = f
                already.append((f, base))
            else:
                conflicts.append((f, h, d))
            continue

        claimed[h] = f
        note = "" if h.suffix.lower() == ext else f"扩展名不同（原图 {h.suffix} / 成品 {f.suffix}）"
        moved.append((f, h, d, note))

    # 05 里没被任何成品指向的
    untouched = [f for f in iter_files(jpg_dir) if f not in referenced] if jpg_dir.is_dir() else []

    confirm_apply(args.apply, f"归档批次 {name} 的原图 JPG")
    print(f"待归档 {len(moved):>4} 个")
    print(f"05 保留（未修） {len(untouched):>4} 个")
    if already:
        print(f"{C_INFO}已在归档中 {len(already)} 个（重复跑命令不会出错）")
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
        print(f"\n{C_WARN}冲突：99 里已有同名但内容不同的文件 {len(conflicts)} 个 —— 不覆盖：")
        for f, h, d in conflicts[:20]:
            print(f"       05 {h}")
            print(f"       99 {d}  （内容不同）")
    if missing:
        print(f"\n{C_WARN}03 有成品但 05 和 99 都找不到原图 {len(missing)} 个 —— 需你人工确认：")
        for f, base in missing[:30]:
            print(f"       {f.name}  -> 找不到 {base}.*")
    if orphan:
        print(f"\n{C_WARN}03 里没有 -ed 后缀的文件 {len(orphan)} 个 —— 不在归档判定范围内：")
        for f in orphan[:30]:
            print(f"       {f}")

    if moved:
        print(f"\n将移动（05 -> 99）：")
        for f, h, d, note in moved[:40]:
            extra = f"  [{note}]" if note else ""
            print(f"       {h.name:<32} {human(h.stat().st_size):>9}{extra}")
        if len(moved) > 40:
            print(f"       …… 另有 {len(moved) - 40} 个")

    if not args.apply:
        print("\n加 --apply 真正移动。")
        return

    arc_dir.mkdir(parents=True, exist_ok=True)
    log_dir = paths(args.root)["logs"]
    log_dir.mkdir(parents=True, exist_ok=True)
    log_path = log_dir / f"archive-{_dt.date.today():%Y-%m}.tsv"

    errors = skipped = 0
    with log_path.open("a", encoding="utf-8") as log:
        stamp = _dt.datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        for f, h, d, note in moved:
            try:
                if d.exists():            # 规划后被别的进程抢先放了文件
                    skipped += 1
                    print(f"{C_INFO}目标已存在，跳过 {d.name}")
                    continue
                shutil.move(str(h), str(d))
                log.write(f"{stamp}\t{name}\t{h.name}\t{h}\t{d}\n")
            except OSError as e:
                errors += 1
                print(f"{C_ERR}{h.name}: {e}")

    ok = len(moved) - errors - skipped
    print(f"\n完成：归档 {ok} 个，已在归档中 {len(already)} 个，"
          f"内容冲突 {len(conflicts)} 个，失败 {errors} 个。")
    if skipped:
        print(f"（执行期间另有 {skipped} 个目标已被占用，未计入归档）")
    print(f"日志：{log_path}")
    if untouched:
        print(f"05 里还剩 {len(untouched)} 个未修原图，继续在极相册可见。")


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

    print(f"批次数量：04={len(sets['raw'])}  05={len(sets['jpg'])}  "
          f"03={len(sets['ed'])}  99={len(sets['arc'])}\n")

    # 0. 顶层目录缺失（后面的检查都依赖这些目录，先说明白）
    labels = {"raw": DIR_RAW, "jpg": DIR_JPG, "ed": DIR_ED, "arc": DIR_ARC}
    gone = [k for k in ("jpg", "ed", "arc") if not p[k].is_dir()]
    if gone:
        print("-- 顶层目录缺失 --")
        for k in gone:
            issue(C_WARN, f"{labels[k]} 不存在，相关检查已跳过")
        print()

    # 1. 命名合规
    bad_names = sorted(n for n in all_names if not check_batch_name(n)[0])
    if bad_names:
        print("-- 批次命名 --")
        for n in bad_names:
            _, hint = check_batch_name(n)
            issue(C_WARN, f"{n}：{hint}")
        print()

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
        print("-- 04/05 缺目录 --")
        for n, have, lack in disagree:
            issue(C_WARN, f"{n}：有 {'/'.join(have) or '无'}，缺 {'/'.join(lack)}")
        print()

    # 3. 03 根目录散落文件
    stray = (sorted(f for f in p["ed"].iterdir() if f.is_file())
             if p["ed"].is_dir() else [])
    if stray:
        print("-- 03 根目录散落文件 --")
        for f in stray:
            issue(C_WARN, f"{f.name}  ({human(f.stat().st_size)})  {_dt.datetime.fromtimestamp(f.stat().st_mtime):%Y-%m-%d}")
        print()

    # 4. 05 空目录
    empty = []
    if p["jpg"].is_dir():
        for d in sorted(x for x in p["jpg"].iterdir() if x.is_dir()):
            if not any(iter_files(d)):
                empty.append(d)
    if empty:
        print("-- 05 空目录（可删） --")
        for d in empty:
            issue(C_INFO, str(d))
        print()

    # 5. 该归档但没归档的（极相册里会重复的根源）
    pending = []
    for n in sorted(sets["ed"]):
        ed_dir = p["ed"] / n
        jpg_dir = p["jpg"] / n
        if not jpg_dir.is_dir():
            continue
        index = {}
        for f in iter_files(jpg_dir):
            index.setdefault(f.stem.lower(), []).append(f)
        cnt = 0
        for f in iter_files(ed_dir):
            base = strip_ed(f.stem)
            if base and index.get(base.lower()):
                cnt += 1
        if cnt:
            pending.append((n, cnt))
    if pending:
        print("-- 已修但原图还没归档（极相册会重复显示） --")
        for n, cnt in pending:
            issue(C_WARN, f"{n}：{cnt} 张  ->  python photoflow.py archive {n} --apply")
        print()

    # 6. 03 里不带 -ed 的文件（按批次）
    odd = []
    for n in sorted(sets["ed"]):
        for f in iter_files(p["ed"] / n):
            if strip_ed(f.stem) is None:
                odd.append(f)
    if odd:
        print("-- 03 里不带 -ed 的成品 --")
        for f in odd[:30]:
            issue(C_INFO, str(f))
        print()

    if issues == 0:
        print(f"{C_OK}没发现问题。")
    else:
        print(f"共 {issues} 项需要关注。WARN 建议处理，INFO 仅供参考。")


# ---------------------------------------------------------------- CLI

def build_parser():
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--root", default=DEFAULT_ROOT,
                        help=f"照片库根目录（默认 {DEFAULT_ROOT}）")

    ap = argparse.ArgumentParser(
        prog="photoflow",
        description="极空间摄影工作流助手：建批次 / 分发 / 归档 / 体检",
        epilog="会改动文件的命令默认只预演，确认清单后加 --apply 执行。",
    )
    sub = ap.add_subparsers(dest="cmd", required=True)

    s = sub.add_parser("new", parents=[common], help="为一个批次建好 04/05/03/99 四个同名目录")
    s.add_argument("batch", help="批次名，如 2026-11-25-香炉石")
    s.add_argument("--force", action="store_true", help="批次名不合规也照建")
    s.add_argument("--apply", action="store_true",
                   help="确认清单无误后真正执行（不加只预演）")
    s.set_defaults(func=cmd_new)

    s = sub.add_parser("ingest", parents=[common], help="把卡里的照片按 RAW/JPG 分发到 04/05")
    s.add_argument("src", help="源目录，如 F:\\DCIM")
    s.add_argument("batch", help="批次名")
    s.add_argument("--apply", action="store_true",
                   help="确认清单无误后真正执行（不加只预演）")
    s.add_argument("--dedupe", action="store_true",
                   help="卡内同名但内容不同时，自动加 _2/_3 后缀而不是跳过")
    s.set_defaults(func=cmd_ingest)

    s = sub.add_parser("archive", parents=[common], help="把已修照片的 JPG 原图从 05 移到 99")
    s.add_argument("batch", help="批次名")
    s.add_argument("--apply", action="store_true",
                   help="确认清单无误后真正执行（不加只预演）")
    s.set_defaults(func=cmd_archive)

    s = sub.add_parser("doctor", parents=[common], help="体检：命名、目录对齐、漏归档、空目录")
    s.set_defaults(func=cmd_doctor)

    return ap


def main():
    ap = build_parser()
    args = ap.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
