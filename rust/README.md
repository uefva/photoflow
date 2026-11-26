# photoflow-rs

photoflow 的 Rust 版。功能对齐 Python 版（`../photoflow.py`）的 **CLI 全部**：
`new` / `ingest` / `archive` / `doctor` 四个命令、配置文件、预演/执行两段式、
计划指纹、跨卷安全移动、`--progress` 与 `@@PLAN@@` 协议。

零第三方依赖（`[dependencies]` 是空的）：SHA-256 自己实现，JSON 自己解析，
时间格式化自己算——和原版「单文件、无依赖」的取舍一致，也方便在没网的机器上直接编译。

图形界面在 `gui/`，用 **Tauri 2**（系统 WebView2，不打包浏览器内核）。
界面不自己实现任何文件操作——它把参数拼成命令行，用 `__cli` 隐藏模式调用
**自己这个 exe**，再把输出流回界面。所以界面和命令行永远是同一份代码、同一个二进制。

```
cd gui/src-tauri
cargo build --release        # 产物就是带界面的 exe（前端资源已编进去）
# 或者要安装包：
cargo install tauri-cli --version "^2"
cd ..            # 回到 gui/
cargo tauri build
```

命令行那支仍可用：`cargo build --release` 出来的 `photoflow(.exe)` 就是原来的 CLI。

## 怎么跑

```
cargo test              # 40 个用例：27 单元 + 13 集成
cargo build --release   # 产物 target/release/photoflow(.exe)

photoflow new     2026-11-26-梧桐山 --apply
photoflow ingest  F:\DCIM 2026-11-26-梧桐山 --apply
photoflow archive 2026-11-26-梧桐山            # 预演，打印清单和计划指纹
photoflow archive 2026-11-26-梧桐山 --apply --expect-plan <指纹>
photoflow doctor
```

通用参数：`--root PATH`、`--apply`、`--progress`、`--expect-plan X`；
`ingest` 另有 `--dedupe` / `--verify`，`new` 另有 `--force`。

根目录的取值顺序：`--root` → 配置文件里的 `root` → 环境变量 `PHOTOFLOW_ROOT` →
内置默认值。配置文件位置 `%USERPROFILE%\.photoflow.json`，可用 `PHOTOFLOW_CONFIG` 指走。

## 测试

两层，都必须在你的机器上跑一遍：

1. **Rust 自己的**：`cargo test` —— 27 单元（命名/后缀/指纹/哈希/复制移动/config JSON）
   + 13 集成（合成照片库跑真实二进制：侧车闸门、多版本只归档一次、降噪中缀、
   冲突不覆盖、重复份清理、多格式歧义、单候选跨格式、指纹锁清单、路径穿越、
   分发改名不顶名、不留临时文件）。
2. **Python 那套 49 条**：把环境变量指向 Rust 二进制，同一套用例原样验收：

   ```
   PHOTOFLOW_BIN=/path/to/photoflow python ../test_photoflow.py
   ```

   其中 13 条是进程内的（要 `import photoflow` 直接调函数），会自动跳过——
   这些在 Rust 侧由 `cargo test` 的单元测试一一对应覆盖。

## 与 Python 版的差异

只有两处，都是有意为之：

1. **成品扩展名必须是图片。** 成品目录里的 `.xmp` / `.aae` / `.dop` 这类伴生文件
   即使带 `-ed`，也不会触发归档（只在输出里列成「侧车」）。原版会把它当成品、
   进而把真实原图归档掉——照片从时间线消失。
2. **无参数运行**：Python 版直接开图形界面；Rust 版打印用法并退出 2。

其余全部对齐，包括输出文案（测试是按文案断言的）、退出码（出错为 2）、
日志格式（`99_图库/原图JPG归档/_logs/archive-YYYY-MM.tsv`，5 列）、
以及「同卷改名 / 跨卷复制+校验+删源」的移动语义。

## 移植时特意保住的三个坑

**跨卷绝不靠 `std::fs::rename`。** Rust 在 Windows 上它走
`MoveFileEx(MOVEFILE_COPY_ALLOWED)`，跨卷会自己悄悄复制再删、且不校验。
这里同一卷用「硬链接 + 删源」（目标已存在则失败，天然不覆盖），
Windows 上直接调不带 `COPY_ALLOWED`/`REPLACE_EXISTING` 的 `MoveFileExW`，
跨卷如实返回错误码 17，进入真正的「复制 → 校验 → 删源」。

**`copy2` 的 mtime 要自己补。** `fs::copy` 不拷时间；计划指纹吃 mtime，
不补的话重跑同一张卡会被判成全新照片。复制后显式 `set_times`。

**跨卷错误码分平台。** Unix 18 / Windows 17，用 `raw_os_error()` + `cfg` 分别判，
不做「一把梭」的 errno 比较。

## 还没验证的

Windows 目标已在 Linux 上做过 `cargo check --target x86_64-pc-windows-gnu`
（类型检查通过，`MoveFileExW` / `GetTimeZoneInformation` 这些 `cfg(windows)` 分支都能编过），
但**没有在真机 Windows 上跑过**。第一次在你机器上用，建议先只做预演、再拿一个
小批次试 `--apply`，重点看两件事：中文文件名正常、跨卷（原图和归档在同一个卷上时
应该走改名，秒完）。
