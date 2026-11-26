#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
验证三处加固是否真的生效（对应之前那轮评审的第 1、2、3 条）：

  1. 分发中途失败，不能留下半截文件；
  2. 归档跨卷复制出错，原图不能丢，也不能悄悄存一份错的；
  3. 预演确认的清单要能锁住，清单变了就拒绝执行。

全部在临时目录里造数据，不碰真实照片库。为了制造故障，会在进程内临时替换
shutil.copy2 / os.rename —— 这是故障注入，跑完一定还原。

跑法：python verify_fixes.py
"""

import os
import shutil
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import photoflow as pf

BATCH = "2026-11-26-测试"
HR = "=" * 68


def fresh_lib(base):
    root = Path(base) / "lib"
    for key in ("raw", "jpg", "ed", "arc"):
        (root / pf.DEFAULT_DIRS[key]).mkdir(parents=True, exist_ok=True)
    return root


def batch_dirs(lib):
    d = {}
    for key in ("raw", "jpg", "ed", "arc"):
        d[key] = lib / pf.DEFAULT_DIRS[key] / BATCH
        d[key].mkdir(parents=True, exist_ok=True)
    return d


def run(argv):
    """走真正的命令行入口，stdout 和 stderr 一起收（die 的报错走 stderr）。"""
    import contextlib
    import io

    old = sys.argv
    sys.argv = ["photoflow"] + [str(a) for a in argv]
    buf = io.StringIO()
    try:
        with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(buf):
            pf.main()
    except SystemExit as e:
        return e.code or 0, buf.getvalue()
    finally:
        sys.argv = old
    return 0, buf.getvalue()


def check_1(base):
    print(HR)
    print("1. 分发中途失败，不留半截文件")
    print(HR)
    lib = fresh_lib(base / "c1")
    card = base / "c1" / "card"
    card.mkdir(parents=True)
    (card / "IMG_0001.JPG").write_bytes(b"A" * 400_000)
    (card / "IMG_0002.JPG").write_bytes(b"B" * 300_000)

    real_copy2 = shutil.copy2

    def flaky_copy2(src, dst, **kw):
        data = Path(src).read_bytes()
        Path(dst).write_bytes(data[: len(data) // 2])
        raise OSError("模拟：传输中断（已写入一半）")

    shutil.copy2 = flaky_copy2
    try:
        run(["ingest", card, BATCH, "--root", lib, "--apply"])
    finally:
        shutil.copy2 = real_copy2

    jpg_dir = lib / pf.DEFAULT_DIRS["jpg"] / BATCH
    left = sorted(p.name for p in jpg_dir.iterdir()) if jpg_dir.is_dir() else []
    print(f"  失败后原图目录里剩下：{left or '（空）'}")

    rc, out = run(["ingest", card, BATCH, "--root", lib])
    conflict = "冲突：目标已存在但内容不同" in out
    print(f"  恢复正常后重新预演，还报不报冲突：{conflict}")

    ok = (left == [] and not conflict)
    print(f"  → {'通过：没有残留，重跑能正常导入' if ok else '不通过'}")
    return ok


def check_2(base):
    print()
    print(HR)
    print("2. 归档跨卷复制出错：原图不能丢，也不能存错的")
    print(HR)
    lib = fresh_lib(base / "c2")
    d = batch_dirs(lib)
    size = 300_000
    src = d["jpg"] / "IMG_0001.JPG"
    src.write_bytes(b"O" * size)
    (d["ed"] / "IMG_0001-ed.JPG").write_bytes(b"e" * 1000)

    real_rename = os.rename
    real_copy2 = shutil.copy2

    def cross_device(*a, **kw):
        raise OSError(18, "Invalid cross-device link")

    def same_size_wrong_bytes(s, t, **kw):
        n = Path(s).stat().st_size
        Path(t).write_bytes(b"X" * n)
        shutil.copystat(s, t)

    os.rename = cross_device
    shutil.copy2 = same_size_wrong_bytes
    try:
        rc, out = run(["archive", BATCH, "--root", lib, "--apply"])
    finally:
        os.rename = real_rename
        shutil.copy2 = real_copy2

    kept = src.exists()
    arc = sorted(p.name for p in d["arc"].iterdir())
    failed = "失败 1 个" in out
    print(f"  原图还在：{kept}")
    print(f"  归档目录：{arc or '（空）'}")
    print(f"  报告里有「失败 1 个」：{failed}")

    ok = kept and arc == [] and failed
    print(f"  → {'通过：校验没过就停手，原图保住' if ok else '不通过'}")
    return ok


def check_3(base):
    print()
    print(HR)
    print("3. 预演清单锁定：清单变了就拒绝执行")
    print(HR)
    lib = fresh_lib(base / "c3")
    d = batch_dirs(lib)
    for i in (1, 2):
        (d["jpg"] / f"IMG_{i:04d}.JPG").write_bytes(b"o" * 1000)
        (d["ed"] / f"IMG_{i:04d}-ed.JPG").write_bytes(b"e" * 500)

    rc, out = run(["archive", BATCH, "--root", lib, "--progress"])
    tokens = [l.split("\t")[1] for l in out.splitlines() if l.startswith("@@PLAN@@")]
    token = tokens[0] if tokens else None
    print(f"  预演拿到指纹：{token}")

    (d["jpg"] / "IMG_0003.JPG").write_bytes(b"o" * 1000)
    (d["ed"] / "IMG_0003-ed.JPG").write_bytes(b"e" * 500)
    print("  预演之后又放进去一组 IMG_0003")

    rc, out = run(["archive", BATCH, "--root", lib,
                   "--expect-plan", token, "--apply"])
    refused = rc != 0 and "计划已经变了" in out
    moved = sorted(p.name for p in d["arc"].iterdir())
    print(f"  带旧指纹执行：被拒绝={refused}，实际移动了 {moved or '（没有）'}")

    rc, out = run(["archive", BATCH, "--root", lib, "--progress"])
    token2 = [l.split("\t")[1] for l in out.splitlines() if l.startswith("@@PLAN@@")][0]
    rc, out = run(["archive", BATCH, "--root", lib,
                   "--expect-plan", token2, "--apply"])
    moved2 = sorted(p.name for p in d["arc"].iterdir())
    print(f"  重新预演后再执行：{len(moved2)} 个 {moved2}")

    ok = refused and moved == [] and len(moved2) == 3
    print(f"  → {'通过：旧指纹被拦下，新指纹放行' if ok else '不通过'}")
    return ok


def main():
    base = Path(tempfile.mkdtemp(prefix="photoflow-verify-"))
    os.environ["PHOTOFLOW_CONFIG"] = str(base / "cfg.json")
    pf.CONFIG_PATH = Path(os.environ["PHOTOFLOW_CONFIG"])
    pf._PROGRESS["on"] = False
    print(f"临时目录：{base}\n")
    try:
        results = [check_1(base), check_2(base), check_3(base)]
        print()
        print(HR)
        print(f"结果：1={'通过' if results[0] else '不通过'}  "
              f"2={'通过' if results[1] else '不通过'}  "
              f"3={'通过' if results[2] else '不通过'}")
        print(HR)
        return 0 if all(results) else 1
    finally:
        shutil.rmtree(base, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
