# photoflow-rs

photoflow 的 Rust 版。功能对齐 Python 版（`../python/photoflow.py`）的 **CLI 全部**：
`new` / `ingest` / `archive` / `doctor` 四个命令、配置文件、预演/执行两段式、
计划指纹、跨卷安全移动、`--progress` 与 `@@PLAN@@` 协议。

零第三方依赖（`[dependencies]` 是空的）：SHA-256 自己实现，JSON 自己解析，
时间格式化自己算——和原版「单文件、无依赖」的取舍一致，也方便在没网的机器上直接编译。

图形界面在 `gui/`，用 **Tauri 2**（系统 WebView2，不打包浏览器内核）。
界面不自己实现任何文件操作——它把参数拼成命令行，用 `__cli` 隐藏模式调用
**自己这个 exe**，再把输出流回界面。所以界面和命令行永远是同一份代码、同一个二进制。

```
cd gui/src-tauri
cargo build --release        # 产物 target/release/photoflow-gui.exe（前端资源已编进去，
                             # release 下不弹控制台）
# 或者要安装包：
cargo install tauri-cli --version "^2"
cd ..            # 回到 gui/
cargo tauri build
```

命令行那支仍可用：`cargo build --release` 出来的 `photoflow(.exe)` 就是原来的 CLI。

## 怎么跑

以下命令都在 `rust/` 目录下执行：

```
cargo test              # 44 个用例：29 单元 + 15 集成
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

1. **Rust 自己的**：`cargo test` —— 29 单元（命名/后缀/指纹/哈希/复制移动/config JSON）
   + 15 集成（合成照片库跑真实二进制：侧车闸门、多版本只归档一次、降噪中缀、
   冲突不覆盖、重复份清理、多格式歧义、单候选跨格式、指纹锁清单、路径穿越、
   分发改名不顶名、不留临时文件、目录撞车拒绝执行）。
2. **Python 那套 24 条**：`python/test/test_photoflow.py`，跑的是 Python 版主程序。
   它没有 `PHOTOFLOW_BIN` 这类开关，不能指向 Rust 二进制；改了一边最好把
   `cargo test` 和这套都跑一遍（见根目录 README「跑测试」）。

## 与 Python 版的差异

只有两处，都是有意为之：

1. **成品扩展名必须是图片。** 成品目录里的 `.xmp` / `.aae` / `.dop` 这类伴生文件
   即使带 `-ed`，也不会触发归档（只在输出里列成「侧车」）。原版会把它当成品、
   进而把真实原图归档掉——照片从时间线消失。
2. **无参数运行**：Python 版直接开图形界面；Rust 的 CLI 二进制打印用法并退出 2
   （带 Tauri 界面的那个 exe，无参数就是开界面）。

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

## 验证情况

- `cargo test`：44 条（29 单元 + 15 集成）全绿。
- Windows 实机：CLI 和 Tauri 界面都已编译并实际使用过。期间发现并修掉两个问题——
  `--verify` 的哈希读缓冲（1 MiB 栈数组）撑爆主线程栈；界面里的确认框因为是
  async 版本而没拦住执行。
- 跨平台分支：开发环境是 Linux，另做过 `cargo check --target x86_64-pc-windows-gnu`，
  `MoveFileExW` / `GetTimeZoneInformation` 这些 `cfg(windows)` 代码都能编过。

第一次在自己的库上用，仍建议先只预演、再拿一个小批次试 `--apply`，重点看中文
文件名、以及原图和归档同卷时是否走改名（应该瞬间完成，不走复制）。
