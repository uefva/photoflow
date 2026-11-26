#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
photoflow 压力测试。

造一批假照片，走完整流程（建批次 → 分发 → 重复分发 → 归档 → 体检），
量每一步的耗时、吞吐和子进程峰值内存，并校验文件数量与字节数没有出错。

用法：
  python stress_test.py --workdir D:\\stress --size-gb 10 --files 200
  python stress_test.py --workdir D:\\stress --size-gb 10 --files 20000   # 小文件海量
  python stress_test.py --workdir D:\\stress --size-gb 1 --files 20 --keep

注意：需要 2 倍目标的空闲空间（一份"卡"，一份"照片库"）。
      跑完默认会删掉临时数据，加 --keep 保留现场。
"""

import argparse
import os
import random
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE.parent / "photoflow.py"
BATCH = "2026-12-20-压测"

try:
    import resource
except ImportError:                # Windows 没有 resource
    resource = None


def human_bytes(n):
    for unit in ("B", "KB", "MB", "GB", "TB"):
        if n < 1024 or unit == "TB":
            return f"{n:.1f}{unit}"
        n /= 1024


def dir_stats(folder):
    """返回 (文件数, 总字节)。"""
    count = total = 0
    for dirpath, _dirnames, filenames in os.walk(folder):
        for fn in filenames:
            try:
                total += os.path.getsize(os.path.join(dirpath, fn))
                count += 1
            except OSError:
                pass
    return count, total


def make_file(path, size, sparse=False, chunk=4 << 20):
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "wb") as fh:
        if sparse:
            fh.truncate(size)
            return
        left = size
        while left > 0:
            n = min(chunk, left)
            fh.write(os.urandom(n))
            left -= n


def build_card(card, size_gb, files, sparse):
    """造卡：一半 .CR3、一半 .JPG，大小带点抖动，总数接近 size_gb。"""
    if card.exists():
        shutil.rmtree(card)
    card.mkdir(parents=True)

    total_bytes = int(size_gb * (1 << 30))
    per = max(1, total_bytes // max(1, files))
    made = written = 0
    t0 = time.perf_counter()

    for i in range(files):
        ext = ".CR3" if i % 2 == 0 else ".JPG"
        # ±20% 抖动，避免所有文件大小完全一致
        size = int(per * random.uniform(0.8, 1.2))
        sub = f"100CANON" if i % 4 < 2 else "101CANON"
        name = f"IMG_{i:05d}{ext}"
        make_file(card / sub / name, size, sparse=sparse)
        made += 1
        written += size

        if made % max(1, files // 10) == 0:
            pct = made * 100 // files
            print(f"    造卡 {pct:3d}%  {human_bytes(written)}", flush=True)

    dt = time.perf_counter() - t0
    print(f"  造卡完成：{made} 个文件，{human_bytes(written)}，"
          f"{dt:.1f}s（{written / dt / 1e6:.0f} MB/s）")


class Runner:
    def __init__(self):
        self.rows = []
        self.python = sys.executable

    def run(self, label, args, apply=False):
        cmd = [self.python, "-I", str(SCRIPT), *[str(a) for a in args]]
        if apply:
            cmd.append("--apply")

        t0 = time.perf_counter()
        proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                text=True, encoding="utf-8", errors="replace")
        peak_kb = 0
        while proc.poll() is None:
            # 边跑边读 /proc/<pid>/status 的 VmHWM（进程峰值常驻内存）
            try:
                with open(f"/proc/{proc.pid}/status", encoding="utf-8") as fh:
                    for line in fh:
                        if line.startswith("VmHWM:"):
                            peak_kb = max(peak_kb, int(line.split()[1]))
                            break
            except OSError:
                pass
            time.sleep(0.02)
        out_text, err_text = proc.communicate()
        dt = time.perf_counter() - t0

        out = (out_text or "") + (err_text or "")
        ok = proc.returncode == 0
        self.rows.append({"step": label, "sec": dt, "rc": proc.returncode,
                          "peak_mb": peak_kb / 1024})
        mem = f"  峰值内存 {peak_kb / 1024:.0f} MB" if peak_kb else ""
        print(f"  {label:<22} {dt:7.2f}s  rc={proc.returncode}{mem}")
        if not ok:
            print(out.strip()[-800:])
        return dt, proc.returncode, out


def sprint_table(rows, total_bytes):
    print("\n" + "=" * 68)
    print(f"{'步骤':<24}{'耗时(s)':>9}{'吞吐(MB/s)':>12}{'峰值内存(MB)':>14}")
    print("-" * 68)
    for r in rows:
        thr = (total_bytes / r["sec"] / 1e6) if r["sec"] > 0 else 0
        mem = f"{r['peak_mb']:.0f}" if r["peak_mb"] else "-"
        print(f"{r['step']:<24}{r['sec']:>9.2f}{thr:>12.0f}{mem:>14}")
    print("=" * 68)


def main():
    ap = argparse.ArgumentParser(description="photoflow 压力测试")
    ap.add_argument("--workdir", required=True, help="临时目录，需要 2 倍目标的空闲空间")
    ap.add_argument("--size-gb", type=float, default=10.0, help="数据总量（GB，默认 10）")
    ap.add_argument("--files", type=int, default=200, help="文件个数（默认 200，大文件）")
    ap.add_argument("--sparse", action="store_true",
                    help="用稀疏文件（几乎不占盘），只测扫描不测真实拷贝")
    ap.add_argument("--keep", action="store_true", help="跑完保留数据，不删除")
    args = ap.parse_args()

    work = Path(args.workdir).resolve()
    card = work / "card"
    lib = work / "lib"
    work.mkdir(parents=True, exist_ok=True)
    if lib.exists():
        shutil.rmtree(lib)

    print(f"工作目录：{work}")
    print(f"目标规模：{args.size_gb:g} GB / {args.files} 个文件"
          f"{'（稀疏文件）' if args.sparse else ''}\n")

    print("[0] 造数据")
    build_card(card, args.size_gb, args.files, args.sparse)
    card_count, card_bytes = dir_stats(card)
    print(f"  卡内合计：{card_count} 个文件，{human_bytes(card_bytes)}\n")

    r = Runner()
    print("[1] 建批次")
    r.run("new --apply", ["new", BATCH, "--root", lib], apply=True)

    real_io = not args.sparse
    print("\n[2] 首次分发（真实拷贝）" if real_io else "\n[2] 首次分发（稀疏文件，只预演不落地）")
    r.run("ingest --apply" if real_io else "ingest 预演",
          ["ingest", card, BATCH, "--root", lib], apply=real_io)
    raw_n, raw_b = dir_stats(lib / "04_RAW原图" / BATCH)
    jpg_n, jpg_b = dir_stats(lib / "05_JPG图" / BATCH)
    print(f"  RAW 目录 {raw_n} 个 / {human_bytes(raw_b)}；"
          f"原图目录 {jpg_n} 个 / {human_bytes(jpg_b)}")
    if real_io:
        assert raw_b + jpg_b == card_bytes, "分发后的字节数和卡上不一致！"

    if not real_io:
        # 稀疏模式下把卡里的文件硬链接进照片库：不占空间，但目录里有真实条目，
        # 后面 doctor / archive 才能扫到东西。
        raw_dir = lib / "04_RAW原图" / BATCH
        jpg_dir = lib / "05_JPG图" / BATCH
        raw_dir.mkdir(parents=True, exist_ok=True)
        jpg_dir.mkdir(parents=True, exist_ok=True)
        linked = 0
        for f in card.rglob("*"):
            if not f.is_file():
                continue
            target = raw_dir if f.suffix.lower() == ".cr3" else jpg_dir
            try:
                os.link(f, target / f.name)
                linked += 1
            except OSError:
                shutil.copy2(f, target / f.name)
        print(f"  已硬链接 {linked} 个文件进照片库（不额外占空间）")

    print("\n[3] 重复分发（同名同大小 → 每个文件走一次 SHA-256 去重）")
    if real_io:
        r.run("ingest 重复 --apply", ["ingest", card, BATCH, "--root", lib], apply=True)
    else:
        print("  稀疏模式跳过：哈希读到的是全零空洞，没有参考价值")

    print("\n[4] 模拟修图：给 20% 的 JPG 生成 -ed 成品")
    ed_dir = lib / "03_缓存-调色ed" / BATCH
    edited = 0
    for i, f in enumerate(sorted((lib / "05_JPG图" / BATCH).rglob("*.JPG"))):
        if i % 5:
            continue
        make_file(ed_dir / f"{f.stem}-ed.jpg", 200 * 1024)
        edited += 1
    print(f"  生成 {edited} 个成品")

    print("\n[5] 归档（原图目录 → 归档目录）")
    r.run("archive --apply" if real_io else "archive 预演",
          ["archive", BATCH, "--root", lib], apply=real_io)
    arc_n, arc_b = dir_stats(lib / "99_图库" / "原图JPG归档" / BATCH)
    left_n, left_b = dir_stats(lib / "05_JPG图" / BATCH)
    print(f"  已归档 {arc_n} 个 / {human_bytes(arc_b)}；"
          f"原图目录剩下 {left_n} 个 / {human_bytes(left_b)}")
    if real_io and arc_n != edited:
        print(f"  !! 归档数量 {arc_n} 与成品数量 {edited} 不一致")

    print("\n[6] 体检（全库扫描）")
    r.run("doctor", ["doctor", "--root", lib])

    total = card_bytes
    sprint_table(r.rows, total)

    ok = all(x["rc"] == 0 for x in r.rows)
    print(f"\n结论：{'全部通过' if ok else '有步骤失败'}")
    print(f"数据量：{human_bytes(total)}，文件数：{card_count}")

    if not args.keep:
        shutil.rmtree(card, ignore_errors=True)
        shutil.rmtree(lib, ignore_errors=True)
        print("临时数据已清理（加 --keep 可保留）")


if __name__ == "__main__":
    main()
