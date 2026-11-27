// photoflow 桌面版前端逻辑。
//
// 和 Python 版同样的规矩：
//   - 界面不自己动文件，只拼一条命令行交给 __cli 模式去跑；
//   - 先预演、拿到「清单指纹」之后才允许执行；执行时把指纹带回去，
//     中间目录变了 CLI 会拒绝执行；
//   - 改了任何输入，之前那次预演立刻作废，执行按钮变灰。

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const TITLE = { new: "建批次", ingest: "从卡分发", archive: "归档", doctor: "体检" };

const S = {
  root: "",
  batch: "",
  dirs: {},
  areaNames: {},
  defaultDirs: {},
  configPath: "",
};

// 每个页签最近一次预演时的「输入指纹」和拿到的清单指纹
const previewed = { new: null, ingest: null, archive: null };
const tokens = { new: null, ingest: null, archive: null };

let busy = false;
let activeRun = null;

const $ = (id) => document.getElementById(id);

// ---------------------------------------------------------------- 输入

function currentSig() {
  return JSON.stringify([
    $("root").value.trim(),
    $("batch").value.trim(),
    $("src").value.trim(),
    $("dedupe").checked,
    $("verify").checked,
  ]);
}

function argsFor(name) {
  const batch = $("batch").value.trim();
  if (name === "doctor") return [];

  if (!batch) {
    toast("先去上方「当前批次」填一个，例如 2026-11-26-梧桐山");
    return null;
  }
  if (name === "new" || name === "archive") return [batch];

  if (name === "ingest") {
    const src = $("src").value.trim().replace(/^"|"$/g, "");
    if (!src) {
      toast("先填源目录（相机卡的 DCIM，或者已经拷到本机的目录）");
      return null;
    }
    const args = [src, batch];
    if ($("dedupe").checked) args.push("--dedupe");
    if ($("verify").checked) args.push("--verify");
    return args;
  }
  return null;
}

function invalidate(name) {
  previewed[name] = null;
  tokens[name] = null;
  const btn = document.querySelector(`[data-apply="${name}"]`);
  if (btn) btn.disabled = true;
}

function invalidateAll() {
  ["new", "ingest", "archive"].forEach(invalidate);
}

// ---------------------------------------------------------------- 输出

function appendLine(name, text) {
  const el = $("out-" + name);
  if (!el) return;
  el.textContent += text + "\n";
  // 上万行的时候别把内存吃满：只留最后 20 万字符
  if (el.textContent.length > 200000) {
    el.textContent = el.textContent.slice(-150000);
  }
  el.scrollTop = el.scrollHeight;
}

function setBar(name, pct) {
  const el = $("bar-" + name);
  if (el) el.style.width = Math.max(0, Math.min(100, pct)) + "%";
}

function status(text) {
  $("status").textContent = text;
}

function toast(text) {
  status(text);
  $("settings-hint").textContent = "";
}

function settingsHint(text, kind) {
  const el = $("settings-hint");
  el.textContent = text;
  el.className = "hint-line" + (kind ? " " + kind : "");
}

// ---------------------------------------------------------------- 跑命令

async function start(name, apply) {
  if (busy) {
    status("上一个操作还没跑完");
    return;
  }
  const args = argsFor(name);
  if (args === null) return;

  if (apply) {
    if (previewed[name] !== currentSig() || !tokens[name]) {
      status("输入改过了，或者还没有预演过。请先点「预演」看清清单。");
      return;
    }
    // 必须 await：Tauri 的 dialog 插件把 window.confirm 换成了 async 版本，
    // 不 await 拿到的只是一个 Promise（永远为真），弹窗还没点就已经开跑了。
    const ok = await window.confirm(
      `即将真正执行「${TITLE[name]}」，这会改动照片库里的文件。\n\n` +
      `只执行刚才预演确认过的那份清单；如果这期间目录里多了、少了或者有文件被改动，` +
      `程序会停下来要求重新预演。\n\n确定继续？`
    );
    if (!ok) return;
  }

  const token = apply ? tokens[name] : null;

  // 预演清屏重来；执行接在预演输出后面，保留上下文
  const out = $("out-" + name);
  if (!apply) {
    out.textContent = "> photoflow " + name + " " + args.join(" ") + "\n\n";
  } else {
    out.textContent += "\n> photoflow " + name + " " + args.join(" ") + " --apply\n\n";
  }

  busy = true;
  activeRun = name;
  setBar(name, 0);
  document.querySelectorAll("[data-preview], [data-apply]").forEach((b) => (b.disabled = true));
  status("运行中…");

  const req = {
    cmd: name,
    args,
    root: $("root").value.trim(),
    apply,
    expect_plan: token,
  };

  try {
    const res = await invoke("run_command", { req });
    if (!apply && name !== "doctor") {
      if (res.code === 0 && res.token) {
        previewed[name] = currentSig();
        tokens[name] = res.token;
        const btn = document.querySelector(`[data-apply="${name}"]`);
        if (btn) btn.disabled = false;
        setBar(name, 100);
        status("预演完成 —— 看清清单后点「执行」");
      } else {
        invalidate(name);
        status(res.code === 0 ? "预演完成，但没有拿到清单指纹，请重试" : `预演结束，退出码 ${res.code}`);
      }
    } else if (name === "doctor") {
      setBar(name, 100);
      status(res.code === 0 ? "体检完成" : `体检结束，退出码 ${res.code}`);
    } else {
      // 执行过一次之后，指纹就作废了，要重新预演
      invalidate(name);
      status(res.code === 0 ? "完成" : `结束，退出码 ${res.code}`);
    }
  } catch (e) {
    appendLine(name, "启动失败：" + e);
    status("结束");
  } finally {
    busy = false;
    activeRun = null;
    document.querySelectorAll("[data-preview], [data-apply]").forEach((b) => {
      b.disabled = b.hasAttribute("data-apply");
    });
    // 执行按钮的可用性由 previewed 决定
    ["new", "ingest", "archive"].forEach((n) => {
      const btn = document.querySelector(`[data-apply="${n}"]`);
      if (btn) btn.disabled = !(previewed[n] === currentSig() && tokens[n]);
    });
  }
}

// ---------------------------------------------------------------- 批次与状态

let batchTimer = null;

async function refreshBatches() {
  const root = $("root").value.trim();
  if (!root) {
    $("batch-list").innerHTML = "";
    return;
  }
  try {
    const names = await invoke("list_batches", { root });
    $("batch-list").innerHTML = names
      .map((n) => `<option value="${n.replace(/"/g, "&quot;")}"></option>`)
      .join("");
  } catch (e) {
    status("读批次失败：" + e);
  }
}

function updateRootHint() {
  const root = $("root").value.trim();
  const el = $("root-hint");
  if (!root) {
    el.textContent = "";
    el.className = "hint";
    return;
  }
  el.textContent = "（点「刷新」确认路径可用）";
  el.className = "hint";
}

function onInputsChanged() {
  invalidateAll();
  updateBatchLabels();
  updateRootHint();
}

function updateBatchLabels() {
  const b = $("batch").value.trim() || "（还没填）";
  $("new-batch").textContent = b;
  $("arc-batch").textContent = b;
}

// ---------------------------------------------------------------- 设置页

async function saveSettings() {
  const dirs = {
    raw: $("dir-raw").value.trim(),
    jpg: $("dir-jpg").value.trim(),
    ed: $("dir-ed").value.trim(),
    arc: $("dir-arc").value.trim(),
    vid: $("dir-vid").value.trim(),
  };
  try {
    await invoke("save_settings", {
      root: $("root").value.trim(),
      batch: $("batch").value.trim(),
      dirs,
    });
    settingsHint("已保存。", "ok");
    status("目录设置已保存");
    invalidateAll();
    refreshBatches();
  } catch (e) {
    settingsHint(String(e), "err");
  }
}

function resetDirs() {
  $("dir-raw").value = S.defaultDirs.raw || "04_RAW原图";
  $("dir-jpg").value = S.defaultDirs.jpg || "05_JPG图";
  $("dir-ed").value = S.defaultDirs.ed || "03_缓存-调色ed";
  $("dir-arc").value = S.defaultDirs.arc || "99_图库/原图JPG归档";
  $("dir-vid").value = S.defaultDirs.vid || "02-视频";
  settingsHint("已恢复默认值，点「保存」才会生效。", "");
}

async function openConfigDir() {
  const p = S.configPath || "";
  const dir = p.replace(/[\\/][^\\/]*$/, "");
  try {
    await invoke("open_path", { path: dir || p });
  } catch (e) {
    settingsHint("打不开：" + e, "err");
  }
}

// ---------------------------------------------------------------- 启动

async function boot() {
  // 子进程输出 → 控制台
  await listen("pf-line", (ev) => {
    if (activeRun) appendLine(activeRun, ev.payload);
  });
  await listen("pf-progress", (ev) => {
    const { done, total, label } = ev.payload;
    if (!activeRun) return;
    const pct = total > 0 ? (done / total) * 100 : 0;
    setBar(activeRun, pct);
    status(`${label} ${done}/${total}（${Math.round(pct)}%）`);
  });

  // 页签切换
  document.querySelectorAll(".tab").forEach((tab) => {
    tab.addEventListener("click", () => {
      document.querySelectorAll(".tab").forEach((t) => t.classList.remove("active"));
      document.querySelectorAll(".panel").forEach((p) => p.classList.remove("active"));
      tab.classList.add("active");
      $("panel-" + tab.dataset.tab).classList.add("active");
    });
  });

  // 按钮
  document.querySelectorAll("[data-preview]").forEach((b) => {
    b.addEventListener("click", () => start(b.dataset.preview, false));
  });
  document.querySelectorAll("[data-apply]").forEach((b) => {
    b.addEventListener("click", () => start(b.dataset.apply, true));
  });

  // 顶部输入
  $("root").addEventListener("input", () => {
    onInputsChanged();
    clearTimeout(batchTimer);
    batchTimer = setTimeout(refreshBatches, 500);
  });
  $("batch").addEventListener("input", onInputsChanged);
  $("src").addEventListener("input", onInputsChanged);
  $("dedupe").addEventListener("change", onInputsChanged);
  $("verify").addEventListener("change", onInputsChanged);
  $("refresh-batches").addEventListener("click", refreshBatches);

  // 浏览 / 打开
  $("pick-root").addEventListener("click", async () => {
    const p = await invoke("pick_directory", { title: "选择照片库根目录" });
    if (p) {
      $("root").value = p;
      onInputsChanged();
      refreshBatches();
    }
  });
  $("open-root").addEventListener("click", async () => {
    const p = $("root").value.trim();
    if (!p) return;
    try {
      await invoke("open_path", { path: p });
    } catch (e) {
      status("打不开：" + e);
    }
  });
  $("pick-src").addEventListener("click", async () => {
    const p = await invoke("pick_directory", { title: "选择相机存储卡或已拷到本机的目录" });
    if (p) {
      $("src").value = p;
      onInputsChanged();
    }
  });

  // 设置页
  $("save-settings").addEventListener("click", saveSettings);
  $("reset-dirs").addEventListener("click", resetDirs);
  $("open-config").addEventListener("click", openConfigDir);

  // 载入状态
  const st = await invoke("get_state");
  S.root = st.root;
  S.batch = st.batch;
  S.dirs = st.dirs;
  S.areaNames = st.area_names;
  S.defaultDirs = st.default_dirs;
  S.configPath = st.config_path;

  $("root").value = st.root || "";
  $("batch").value = st.batch || "";
  $("dir-raw").value = st.dirs.raw || "";
  $("dir-jpg").value = st.dirs.jpg || "";
  $("dir-ed").value = st.dirs.ed || "";
  $("dir-arc").value = st.dirs.arc || "";
  $("dir-vid").value = st.dirs.vid || "";
  $("config-path").textContent = "配置文件：" + st.config_path;

  updateBatchLabels();
  updateRootHint();
  await refreshBatches();
  status("就绪");
}

boot().catch((e) => {
  status("界面初始化失败：" + e);
});
