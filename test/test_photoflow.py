#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
photoflow 复现测试。

测试用例断言的是「正确行为」——所以用例失败 = 该问题被复现。
每条用例都在临时目录里造数据、调用真实 CLI，不碰 J 盘。

运行：python -m unittest test_photoflow -v
"""

import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "photoflow.py"

DIR_RAW = "04_RAW原图"
DIR_JPG = "05_JPG图"
DIR_ED = "03_缓存-调色ed"
DIR_ARC = Path("99_图库") / "原图JPG归档"


# 测试里一律用这个不存在的配置文件，保证跑的是默认目录名，
# 不受本机 ~/.photoflow.json 里改过的设置影响。
NO_CONFIG = HERE / "_no_such_config.json"


def _env(config=None):
    env = dict(os.environ)
    env["PHOTOFLOW_CONFIG"] = str(config or NO_CONFIG)
    return env


def run(*args, config=None):
    """调用真实 CLI，返回 (退出码, 合并输出)。"""
    cmd = [sys.executable, "-I", str(SCRIPT)] + [str(a) for a in args]
    p = subprocess.run(cmd, capture_output=True, text=True,
                       encoding="utf-8", errors="replace", env=_env(config))
    return p.returncode, (p.stdout or "") + (p.stderr or "")


def w(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="pf-test-"))
        self.root = self.tmp / "lib"
        for d in (DIR_RAW, DIR_JPG, DIR_ED, DIR_ARC):
            (self.root / d).mkdir(parents=True)
        self.batch = "2026-01-01-测试地点"

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def files(self, rel):
        d = self.root / rel
        return sorted(p.name for p in d.iterdir() if p.is_file()) if d.is_dir() else []


class IngestTests(Base):
    def test_01_目标同名同大小但内容不同(self):
        """已在 05 的照片与卡上文件同名同大小、内容不同：应报冲突，不能当已存在跳过。"""
        card = self.tmp / "card"
        w(card / "IMG_0001.JPG", b"A" * 1000)
        w(self.root / DIR_JPG / self.batch / "IMG_0001.JPG", b"B" * 1000)

        rc, out = run("ingest", card, self.batch, "--root", self.root, "--apply")

        self.assertIn("冲突", out,
                      "同大小不同内容被静默当成「已存在跳过」，照片被漏导入。\n" + out)

    def test_02_卡内同名同大小但内容不同(self):
        """卡内两个子目录同名同大小、内容不同：这是两张不同的照片，必须报冲突而不是静默丢弃。"""
        card = self.tmp / "card"
        w(card / "100CANON" / "IMG_0002.JPG", b"C" * 800)
        w(card / "101CANON" / "IMG_0002.JPG", b"D" * 800)

        rc, out = run("ingest", card, self.batch, "--root", self.root, "--apply")

        self.assertIn("冲突", out, "内容不同却被当成重复，没有任何提示。\n" + out)
        self.assertIn("IMG_0002", out, "冲突清单里没有列出这张照片。\n" + out)
        got = self.files(Path(DIR_JPG) / self.batch)
        self.assertEqual(len(got), 1, f"预期先导入一张、另一张报冲突，实际 {got}")


class ArchiveTests(Base):
    def _setup_ed(self, *names):
        for n in names:
            w(self.root / DIR_ED / self.batch / n, b"edited")

    def test_03_只按主名匹配导致多扩展名全被移走(self):
        """05 里同一主名有 jpg/png/tif 三种，03 只有一张 -ed：不应把三个都归档。"""
        for ext in (".jpg", ".png", ".tif"):
            w(self.root / DIR_JPG / self.batch / f"DSC0001{ext}", b"orig" + ext.encode())
        self._setup_ed("DSC0001-ed.jpg")

        rc, out = run("archive", self.batch, "--root", self.root, "--apply")

        got = self.files(Path(DIR_ARC) / self.batch)
        self.assertTrue(
            len(got) == 1 or "多个" in out or "警告" in out,
            f"一次性移走了 {len(got)} 个同主名文件且没有任何警告：{got}\n" + out)

    def test_04_两张成品对应一张原图时统计虚高(self):
        """DSC0001-ed.jpg 和 DSC0001-ed-2.jpg 对应同一张原图：实际只移 1 个，统计应为 1。"""
        w(self.root / DIR_JPG / self.batch / "DSC0001.jpg", b"orig")
        self._setup_ed("DSC0001-ed.jpg", "DSC0001-ed-2.jpg")

        rc, out = run("archive", self.batch, "--root", self.root, "--apply")

        actual = len(self.files(Path(DIR_ARC) / self.batch))
        self.assertIn(f"归档 {actual} 个", out,
                      f"99 里实际只有 {actual} 个文件，但统计数字对不上。\n" + out)

    def test_05_99已有同名但内容不同的文件(self):
        """99 里已归档的同名文件与 05 的原图内容不同：应报冲突，不能静默跳过。"""
        w(self.root / DIR_JPG / self.batch / "DSC0002.jpg", b"original-from-card")
        w(self.root / DIR_ARC / self.batch / "DSC0002.jpg", b"totally-different-file")
        self._setup_ed("DSC0002-ed.jpg")

        rc, out = run("archive", self.batch, "--root", self.root, "--apply")

        self.assertIn("冲突", out,
                      "99 里同名但内容不同的文件被静默跳过，没有任何提示。\n" + out)


class DoctorTests(Base):
    def test_06_缺少03目录时doctor不应崩溃(self):
        """03_缓存-调色ed 不存在时，doctor 应正常给出报告，而不是抛异常。"""
        shutil.rmtree(self.root / DIR_ED)

        rc, out = run("doctor", "--root", self.root)

        self.assertNotIn("Traceback", out, "doctor 直接崩溃了：\n" + out)
        self.assertEqual(rc, 0, f"退出码 {rc}，预期 0。\n" + out)


def load_module():
    """把 photoflow.py 当模块加载，用来测进程内的函数。"""
    spec = importlib.util.spec_from_file_location("photoflow_mod", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class SafetyTests(Base):
    def test_08_批次名不能穿越目录(self):
        """批次名里带 / 和 .. 时，不能接受，更不能在照片库外建目录。"""
        evil = "2026-11-25-x/../../../escaped"
        rc, out = run("new", evil, "--root", self.root, "--apply")

        outside = self.tmp / "escaped"
        self.assertFalse(outside.exists(), f"在照片库外创建了目录：{outside}\n{out}")
        self.assertNotEqual(rc, 0, f"非法批次名被接受并执行了\n{out}")

    def test_09_文件变化后哈希缓存应失效(self):
        """同一进程内文件被改过，same_content 不能沿用旧哈希。"""
        mod = load_module()
        a = self.tmp / "h1.bin"
        b = self.tmp / "h2.bin"
        a.write_bytes(b"SAME")
        b.write_bytes(b"SAME")
        self.assertTrue(mod.same_content(a, b), "初始内容相同，应判为相同")

        b.write_bytes(b"DIFF")          # 同大小、不同内容
        self.assertFalse(mod.same_content(a, b),
                         "文件已被改写，缓存仍返回「相同」，会误判为重复")

    def test_10_三张同名只有两种内容(self):
        """a=AAAA、b=BBBB、c=BBBB，--dedupe 后应只留 2 份，不是 3 份。"""
        card = self.tmp / "card"
        w(card / "a" / "IMG.jpg", b"AAAA")
        w(card / "b" / "IMG.jpg", b"BBBB")
        w(card / "c" / "IMG.jpg", b"BBBB")

        run("ingest", card, self.batch, "--root", self.root, "--dedupe", "--apply")

        got = self.files(Path(DIR_JPG) / self.batch)
        self.assertEqual(len(got), 2, f"同为 BBBB 的两张被判成不同照片，实际留下 {got}")

    def test_11_归档索引要区分扩展名(self):
        """99 里只有 IMG.png，不能据此认定 IMG.jpg 已归档。"""
        w(self.root / DIR_ED / self.batch / "IMG-ed.jpg", b"edited")
        w(self.root / DIR_ARC / self.batch / "IMG.png", b"other-format")

        rc, out = run("archive", self.batch, "--root", self.root)

        self.assertNotIn("已在归档中", out,
                         "把扩展名不同的 IMG.png 当成了 IMG.jpg 已归档。\n" + out)
        self.assertIn("找不到对应原图", out, "应报告原图缺失。\n" + out)


    def test_12_网络卷不支持realpath时不应崩溃(self):
        """极空间这类卷上 Path.resolve() 会抛 WinError 1005，路径校验不能依赖它。"""
        mod = load_module()
        orig = Path.resolve

        def boom(self, *a, **k):
            raise OSError(1005, "此卷不包含可识别的文件系统")

        Path.resolve = boom
        try:
            target = mod.batch_dir(self.root / DIR_RAW, "2026-11-25-测试")
        finally:
            Path.resolve = orig

        self.assertEqual(target.name, "2026-11-25-测试")

    def test_13_单元级越界仍被拦截(self):
        """去掉 resolve 之后，路径越界仍要挡住。"""
        mod = load_module()
        with self.assertRaises(SystemExit):
            mod.batch_dir(self.root / DIR_RAW, "2026-11-25-x/../../../escaped")


    def test_14_ed2后缀也要认作成品(self):
        """DSC00596-ed2.jpg 是实际在用的命名，不能被当成「不带 -ed 的文件」。"""
        w(self.root / DIR_JPG / self.batch / "DSC00596.JPG", b"orig")
        w(self.root / DIR_ED / self.batch / "DSC00596-ed2.jpg", b"edited")

        rc, out = run("archive", self.batch, "--root", self.root, "--apply")

        self.assertIn("归档 1 个", out, "没认出 -ed2，原图没被归档。\n" + out)
        self.assertTrue((self.root / DIR_ARC / self.batch / "DSC00596.JPG").exists())
        self.assertFalse((self.root / DIR_JPG / self.batch / "DSC00596.JPG").exists())

    def test_15_同一张有两个成品版本只归档一次(self):
        """-ed 和 -ed2 同时存在时，原图只移一次，不能报两次成功。"""
        w(self.root / DIR_JPG / self.batch / "DSC00596.JPG", b"orig")
        w(self.root / DIR_ED / self.batch / "DSC00596-ed.jpg", b"v1")
        w(self.root / DIR_ED / self.batch / "DSC00596-ed2.jpg", b"v2")

        rc, out = run("archive", self.batch, "--root", self.root, "--apply")

        actual = len(self.files(Path(DIR_ARC) / self.batch))
        self.assertEqual(actual, 1, f"99 里应有 1 个原图，实际 {actual}")
        self.assertIn(f"归档 {actual} 个", out, out)


    def test_16_LR降噪中缀也要能匹配原图(self):
        """DSC07287-已增强-降噪-ed.JPG 对应对原图 DSC07287.JPG。"""
        w(self.root / DIR_JPG / self.batch / "DSC07287.JPG", b"orig")
        w(self.root / DIR_ED / self.batch / "DSC07287-已增强-降噪-ed.JPG", b"edited")

        rc, out = run("archive", self.batch, "--root", self.root, "--apply")

        self.assertIn("归档 1 个", out, "降噪中缀导致原图没被匹配到。\n" + out)
        self.assertTrue((self.root / DIR_ARC / self.batch / "DSC07287.JPG").exists())

    def test_17_英文降噪中缀且doctor不再误报(self):
        """英文版 -Enhanced-NR 同样要认，且不能出现在「不带 -ed」清单里。"""
        w(self.root / DIR_JPG / self.batch / "DSC07332.JPG", b"orig")
        w(self.root / DIR_ED / self.batch / "DSC07332-Enhanced-NR-ed.jpg", b"edited")

        rc, out = run("doctor", "--root", self.root)
        self.assertNotIn("不带 -ed", out, "降噪成品被当成不带 -ed 的文件。\n" + out)

        rc, out = run("archive", self.batch, "--root", self.root, "--apply")
        self.assertIn("归档 1 个", out, out)

    def test_18_其他后缀不被误剥(self):
        """-edited、-raw 这类不该被当成降噪或成品后缀。"""
        mod = load_module()
        self.assertEqual(mod.strip_ed("DSC07287-已增强-降噪-ed"), "DSC07287")
        self.assertEqual(mod.strip_ed("DSC07287-Enhanced-NR-ed2"), "DSC07287")
        self.assertIsNone(mod.strip_ed("DSC07287-edited"))
        self.assertIsNone(mod.strip_ed("DSC07287"))


class ConfigTests(Base):
    def test_19_目录名可以在配置里改(self):
        """四个目录名写进配置后，命令应该按新名字建目录。"""
        cfg = self.tmp / "cfg.json"
        cfg.write_text(json.dumps({"dirs": {
            "raw": "底片", "jpg": "待修", "ed": "成品", "arc": "归档/原图"}},
            ensure_ascii=False), encoding="utf-8")

        rc, out = run("new", self.batch, "--root", self.root, "--apply", config=cfg)

        self.assertEqual(rc, 0, out)
        for rel in ("底片", "待修", "成品", os.path.join("归档", "原图")):
            self.assertTrue((self.root / rel / self.batch).is_dir(),
                            f"没按配置建出 {rel}\n{out}")

    def test_20_非法目录名回退默认(self):
        """配置里写了绝对路径或 .. 时，该用默认名，不能照单全收。"""
        cfg = self.tmp / "bad.json"
        cfg.write_text(json.dumps({"dirs": {
            "raw": "D:\\absolute", "jpg": "..\\逃逸"}}, ensure_ascii=False),
            encoding="utf-8")

        rc, out = run("new", self.batch, "--root", self.root, "--apply", config=cfg)

        self.assertEqual(rc, 0, out)
        self.assertTrue((self.root / "04_RAW原图" / self.batch).is_dir(),
                        f"非法名字没有回退到默认\n{out}")
        self.assertFalse((self.tmp / "逃逸").exists(), "路径逃出去了")


class ControlTests(Base):
    def test_07_正常流程不受影响(self):
        """对照组：new -> ingest -> 手动造 -ed -> archive -> doctor 全流程应当干净。"""
        card = self.tmp / "card"
        for i in ("0101", "0102"):
            w(card / "100EOSR5" / f"IMG_{i}.CR3", b"raw" * 100)
            w(card / "100EOSR5" / f"IMG_{i}.JPG", b"jpg" * 100)

        rc, out = run("new", self.batch, "--root", self.root, "--apply")
        self.assertEqual(rc, 0, out)

        rc, out = run("ingest", card, self.batch, "--root", self.root, "--apply")
        self.assertEqual(rc, 0, out)
        self.assertEqual(len(self.files(Path(DIR_RAW) / self.batch)), 2, out)
        self.assertEqual(len(self.files(Path(DIR_JPG) / self.batch)), 2, out)

        w(self.root / DIR_ED / self.batch / "IMG_0101-ed.jpg", b"edited")
        rc, out = run("archive", self.batch, "--root", self.root, "--apply")
        self.assertIn("归档 1 个", out, out)

        rc, out = run("doctor", "--root", self.root)
        self.assertEqual(rc, 0, out)
        self.assertIn("没发现问题", out, out)


class HardeningTests(Base):
    """
    三个加固点：
    1. 分发中途失败不能留下半截文件（否则下次被当成冲突，得手工清）。
    2. 归档跨卷复制出错时，原图不能丢，也不能悄悄存一份错的。
    3. 预演确认的清单要能锁住，执行时清单变了就拒绝。
    """

    def _load(self):
        mod = load_module()
        mod._PROGRESS["on"] = False
        return mod

    def test_21_分发中途失败不留下半截文件(self):
        card = self.tmp / "card"
        w(card / "IMG_0001.JPG", b"A" * 4000)

        mod = self._load()
        args = mod.build_parser().parse_args(
            ["ingest", str(card), self.batch, "--root", str(self.root), "--apply"])

        real = shutil.copy2

        def half_then_fail(src, dst, **kw):
            data = Path(src).read_bytes()
            Path(dst).write_bytes(data[: len(data) // 2])   # 只写一半
            raise OSError("模拟：传输中断")

        mod.shutil.copy2 = half_then_fail
        try:
            with redirect_stdout(io.StringIO()):
                mod.cmd_ingest(args)
        finally:
            mod.shutil.copy2 = real

        jpg = self.root / DIR_JPG / self.batch
        left = sorted(p.name for p in jpg.iterdir()) if jpg.is_dir() else []
        self.assertEqual(left, [],
                         f"复制失败后目录里留下了残留文件，下次会被当成冲突：{left}")

    def test_22_verify开启时能发现大小相同但内容错误(self):
        card = self.tmp / "card"
        w(card / "IMG_0001.JPG", b"A" * 4000)

        mod = self._load()
        args = mod.build_parser().parse_args(
            ["ingest", str(card), self.batch, "--root", str(self.root),
             "--apply", "--verify"])

        real = shutil.copy2

        def same_size_wrong_bytes(src, dst, **kw):
            n = Path(src).stat().st_size
            Path(dst).write_bytes(b"X" * n)                 # 大小对，内容错
            shutil.copystat(src, dst)

        mod.shutil.copy2 = same_size_wrong_bytes
        try:
            with redirect_stdout(io.StringIO()):
                mod.cmd_ingest(args)
        finally:
            mod.shutil.copy2 = real

        jpg = self.root / DIR_JPG / self.batch
        left = sorted(p.name for p in jpg.iterdir()) if jpg.is_dir() else []
        self.assertEqual(left, [],
                         f"--verify 没拦住内容不对的复制，库里出现了 {left}")

    def test_23_归档跨卷复制失败时原图不能丢(self):
        w(self.root / DIR_JPG / self.batch / "IMG_0001.JPG", b"O" * 4000)
        w(self.root / DIR_ED / self.batch / "IMG_0001-ed.JPG", b"e" * 100)

        mod = self._load()
        args = mod.build_parser().parse_args(
            ["archive", self.batch, "--root", str(self.root), "--apply"])

        real_rename = os.rename
        real_copy2 = shutil.copy2

        def cross_device(*a, **kw):
            raise OSError(18, "Invalid cross-device link")

        def same_size_wrong_bytes(src, dst, **kw):
            n = Path(src).stat().st_size
            Path(dst).write_bytes(b"X" * n)
            shutil.copystat(src, dst)

        mod.os.rename = cross_device          # 逼它走「复制 + 删源」那条路
        mod.shutil.copy2 = same_size_wrong_bytes
        try:
            with redirect_stdout(io.StringIO()) as buf:
                mod.cmd_archive(args)
        finally:
            os.rename = real_rename
            shutil.copy2 = real_copy2

        out = buf.getvalue()
        src = self.root / DIR_JPG / self.batch / "IMG_0001.JPG"
        self.assertTrue(src.exists(), "跨卷复制校验失败时，原图被删了：\n" + out)
        self.assertEqual(self.files(DIR_ARC / self.batch), [],
                         "校验失败的内容落进了归档目录：\n" + out)
        self.assertIn("失败 1 个", out, out)

    def test_24_清单变了就拒绝执行(self):
        for i in (1, 2):
            w(self.root / DIR_JPG / self.batch / f"IMG_{i:04d}.JPG", b"o" * 100)
            w(self.root / DIR_ED / self.batch / f"IMG_{i:04d}-ed.JPG", b"e" * 50)

        rc, out = run("archive", self.batch, "--root", self.root, "--progress")
        self.assertEqual(rc, 0, out)
        tokens = [l.split("\t")[1] for l in out.splitlines()
                  if l.startswith("@@PLAN@@")]
        self.assertTrue(tokens, "预演没有输出计划指纹：\n" + out)
        token = tokens[0]

        # 预演之后又多了一组照片
        w(self.root / DIR_JPG / self.batch / "IMG_0003.JPG", b"o" * 100)
        w(self.root / DIR_ED / self.batch / "IMG_0003-ed.JPG", b"e" * 50)

        rc, out = run("archive", self.batch, "--root", self.root,
                      "--expect-plan", token, "--apply")
        self.assertNotEqual(rc, 0, "清单已经变了，却照旧执行了：\n" + out)
        self.assertIn("计划已经变了", out, out)
        self.assertEqual(self.files(DIR_ARC / self.batch), [],
                         "拒绝执行时不应该动任何文件：\n" + out)

        # 重新预演拿到新指纹后，才能执行
        rc, out = run("archive", self.batch, "--root", self.root, "--progress")
        token2 = [l.split("\t")[1] for l in out.splitlines()
                  if l.startswith("@@PLAN@@")][0]
        self.assertNotEqual(token, token2, "清单变了，指纹却没变")

        rc, out = run("archive", self.batch, "--root", self.root,
                      "--expect-plan", token2, "--apply")
        self.assertEqual(rc, 0, out)
        self.assertEqual(len(self.files(DIR_ARC / self.batch)), 3, out)


if __name__ == "__main__":
    unittest.main(verbosity=2)
