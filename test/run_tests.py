#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
photoflow 回归 / bug 复现测试。
用法：把本文件和 photoflow.py 放同目录，python run_tests.py
"""
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

PHOTOFLOW = Path(__file__).resolve().parent / "photoflow.py"
PY = sys.executable

DIR_RAW = "04_RAW原图"
DIR_JPG = "05_JPG图"
DIR_ED = "03_缓存-调色ed"
DIR_ARC = Path("99_图库") / "原图JPG归档"


def run(root, *args, apply=False):
    cmd = [PY, str(PHOTOFLOW), *map(str, args), "--root", str(root)]
    if apply:
        cmd.append("--apply")
    return subprocess.run(cmd, capture_output=True, text=True,
                          encoding="utf-8", errors="replace")


def fresh_root():
    """每个用例独立临时库，已建 04 目录（check_root 要求）。"""
    d = Path(tempfile.mkdtemp(prefix="pf_test_"))
    (d / DIR_RAW).mkdir()
    return d


def touch(p, data=b"x"):
    p = Path(p)
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_bytes(data)
    return p


# ---------------------------------------------------------------- 用例

def t1_doctor_missing_ed_dir(root):
    """BUG#1：03 目录不存在时 doctor 应正常运行，而非崩溃。"""
    r = run(root, "doctor")
    if "Traceback" in (r.stdout + r.stderr):
        return False, f"doctor 抛未捕获异常\n{r.stderr.strip()[-500:]}"
    return True, "doctor 正常"


def t2_archive_reports_skipped_as_done(root):
    """BUG#2：目标已存在被跳过时，不应计入'归档'成功数。"""
    name = "b"
    touch(root / DIR_JPG / name / "DSC001.jpg", b"orig")
    touch(root / DIR_ED / name / "DSC001-ed.jpg", b"edited")
    touch(root / DIR_ARC / name / "DSC001.jpg", b"already")  # 制造 skip
    r = run(root, "archive", name, apply=True)
    m = re.search(r"完成：归档 (\d+) 个", r.stdout)
    reported = int(m.group(1)) if m else -1
    actual_moved = 0 if (root / DIR_JPG / name / "DSC001.jpg").exists() else 1
    if reported != actual_moved:
        return False, (f"统计错误：实际移动 {actual_moved} 个，"
                       f"但报告'归档 {reported} 个'")
    return True, f"统计正确（归档 {reported} 个）"


def t3_new_target_is_file(root):
    """BUG#3：目标路径已存在同名文件时，new 应优雅报错而非 Traceback。"""
    name = "2026-11-25-测试"
    touch(root / DIR_RAW / name, b"blocker")  # 放个同名文件
    r = run(root, "new", name, apply=True)
    if "Traceback" in (r.stdout + r.stderr):
        return False, f"new 抛未捕获异常\n{r.stderr.strip()[-500:]}"
    return True, "new 优雅报错"


def t4_archive_same_name_diff_subdir(root):
    """BUG#4：05 不同子目录的同名原图，应都被归档。"""
    name = "b"
    touch(root / DIR_JPG / name / "A" / "DSC001.jpg", b"orig_A")
    touch(root / DIR_JPG / name / "B" / "DSC001.jpg", b"orig_B")
    touch(root / DIR_ED / name / "DSC001-ed.jpg", b"edited")
    r = run(root, "archive", name, apply=True)
    arc = root / DIR_ARC / name
    files = list(arc.iterdir()) if arc.is_dir() else []
    if len(files) != 2:
        return False, (f"期望归档 2 个，实际 {len(files)} 个 "
                       f"{[f.name for f in files]}\n{r.stdout.strip()[-400:]}")
    return True, "同名原图均归档"


# ---------------------------------------------------------------- 回归

def t6_new_creates_four_dirs(root):
    """回归：new --apply 应建齐四目录。"""
    name = "2026-11-25-回归"
    run(root, "new", name, apply=True)
    for d in (DIR_RAW, DIR_JPG, DIR_ED, str(DIR_ARC)):
        if not (root / d / name).is_dir():
            return False, f"缺目录 {d}/{name}"
    return True, "四目录齐全"


def t7_ingest_splits_raw_jpg(root):
    """回归：ingest 应按扩展名分发 RAW 与 JPG。"""
    src = root / "_src"
    name = "2026-11-25-回归"
    touch(src / "F100" / "DSC001.CR3", b"r" * 20)
    touch(src / "F100" / "DSC001.JPG", b"j" * 20)
    r = run(root, "ingest", str(src), name, apply=True)
    ok = ((root / DIR_RAW / name / "DSC001.CR3").exists()
          and (root / DIR_JPG / name / "DSC001.JPG").exists())
    if not ok:
        return False, f"分发失败\n{r.stdout.strip()[-400:]}"
    return True, "RAW/JPG 分发正确"


def t8_archive_basic(root):
    """回归：archive 正常归档 1 个并移动原图。"""
    name = "2026-11-25-回归"
    touch(root / DIR_JPG / name / "DSC001.jpg", b"orig")
    touch(root / DIR_ED / name / "DSC001-ed.jpg", b"edit")
    r = run(root, "archive", name, apply=True)
    if (root / DIR_JPG / name / "DSC001.jpg").exists():
        return False, "原图未从 05 移走"
    if not (root / DIR_ARC / name / "DSC001.jpg").exists():
        return False, "原图未到 99"
    if "完成：归档 1 个" not in r.stdout:
        return False, f"统计不对\n{r.stdout.strip()[-200:]}"
    return True, "正常归档"


# ---------------------------------------------------------------- runner

TESTS = [
    t1_doctor_missing_ed_dir,
    t2_archive_reports_skipped_as_done,
    t3_new_target_is_file,
    t4_archive_same_name_diff_subdir,
    t6_new_creates_four_dirs,
    t7_ingest_splits_raw_jpg,
    t8_archive_basic,
]


def main():
    if not PHOTOFLOW.exists():
        sys.exit(f"找不到 {PHOTOFLOW}，请把本脚本和 photoflow.py 放同目录。")

    passed = failed = 0
    print("=" * 60)
    for fn in TESTS:
        root = fresh_root()
        try:
            ok, msg = fn(root)
        except Exception as e:
            ok, msg = False, f"测试自身异常：{e!r}"
        finally:
            shutil.rmtree(root, ignore_errors=True)
        mark = "PASS" if ok else "FAIL"
        print(f"[{mark}] {fn.__name__}\n        {msg}")
        if ok:
            passed += 1
        else:
            failed += 1
    print("=" * 60)
    print(f"合计 {passed + failed}：PASS {passed}，FAIL {failed}")
    sys.exit(0 if failed == 0 else 1)


if __name__ == "__main__":
    main()
