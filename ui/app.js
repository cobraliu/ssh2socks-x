"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const STATE_TEXT = { stopped: "已停止", connecting: "连接中", connected: "已连接", error: "错误" };
const MAX_LOG_LINES = 500;

const KINDS = {
  socks: {
    tag: "SOCKS",
    portLabel: "本地 SOCKS 端口",
    help: "在本机开一个 SOCKS5 代理，流量经服务器出去。",
    firstPort: 1080,
  },
  local: {
    tag: "本地转发",
    portLabel: "本机端口",
    rportLabel: "目标端口",
    help: "把服务器能访问到的端口（服务器本机或其局域网）映射到本机，用 127.0.0.1:本机端口 访问。",
    firstPort: 8080,
  },
  remote: {
    tag: "远程转发",
    portLabel: "本机服务端口",
    rportLabel: "服务器端口",
    help: "把本机端口发布到服务器 0.0.0.0:服务器端口。需 sshd 设置 GatewayPorts yes / clientspecified，否则只监听服务器 127.0.0.1。",
    firstPort: 8080,
  },
};

const hostPort = (h, p) => (h.includes(":") ? `[${h}]:${p}` : `${h}:${p}`);

function mappingOf(v) {
  switch (v.kind) {
    case "local":
      return [`${v.port} → ${hostPort(v.target_host, v.remote_port)}`,
        t("本机 127.0.0.1:{port} → 服务器上的 {target}", { port: v.port, target: hostPort(v.target_host, v.remote_port) })];
    case "remote":
      return [t(":{rport} → 本机:{port}", { rport: v.remote_port, port: v.port }),
        t("服务器 0.0.0.0:{rport} → 本机 127.0.0.1:{port}", { rport: v.remote_port, port: v.port })];
    default:
      return [`127.0.0.1:${v.port}`, t("SOCKS5 代理地址：127.0.0.1:{port}", { port: v.port })];
  }
}

const $ = (id) => document.getElementById(id);
const tunnels = new Map(); // id -> TunnelView
const rows = new Map(); // id -> row element
let selectedId = null;

// ---- helpers -------------------------------------------------------------
function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

let toastTimer;
function toast(message) {
  const box = $("toast");
  box.textContent = message;
  box.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (box.hidden = true), 3200);
}

async function call(cmd, args) {
  try {
    return await invoke(cmd, args);
  } catch (err) {
    toast(String(err));
    throw err;
  }
}

const isActive = (v) => v.state === "connecting" || v.state === "connected";

function healthOf(v) {
  switch (v.state) {
    case "connecting":
      return v.detail ? [v.detail, "warn"] : [t("连接中…"), ""];
    case "connected":
      if (!v.probe) return [t("检测中…"), ""];
      if (!v.probe.ok) return [t("不通 · {message}", { message: v.probe.message }), "bad"];
      return v.probe.latency_ms == null
        ? [t("通"), "ok"]
        : [t("通 {ms}ms", { ms: Math.round(v.probe.latency_ms) }), "ok"];
    case "error":
      return [v.detail ? t("错误 · {detail}", { detail: v.detail }) : t("错误"), "bad"];
    default:
      return ["—", ""];
  }
}

// ---- tunnel list ---------------------------------------------------------
function buildRow(id) {
  const row = el("div", "row");
  row.dataset.id = id;

  const status = el("span", "status");
  status.append(el("span", "dot"), el("span", "state-text"));
  const name = el("span", "cell name");
  const host = el("span", "cell host");
  const addr = el("span", "cell mono addr");
  const health = el("span", "cell health");

  const ops = el("span", "ops");
  const toggle = el("button", "btn small toggle");
  const open = el("button", "btn small open");
  open.addEventListener("click", async (e) => {
    e.stopPropagation();
    const url = await call("open_in_browser", { id });
    toast(t("已在浏览器打开 {url}", { url }));
  });
  const edit = el("button", "btn small edit");
  const del = el("button", "btn small danger delete");
  toggle.addEventListener("click", (e) => {
    e.stopPropagation();
    const v = tunnels.get(id);
    call(isActive(v) ? "stop_tunnel" : "start_tunnel", { id });
  });
  edit.addEventListener("click", (e) => {
    e.stopPropagation();
    openEditor(tunnels.get(id));
  });
  del.addEventListener("click", (e) => {
    e.stopPropagation();
    confirmDelete(tunnels.get(id));
  });
  ops.append(open, toggle, edit, del);

  row.append(status, name, host, addr, health, ops);
  row.addEventListener("click", () => select(id));
  return row;
}

function updateRow(v) {
  const row = rows.get(v.id);
  if (!row) return;
  row.querySelector(".dot").className = `dot ${v.state}`;
  row.querySelector(".state-text").textContent = t(STATE_TEXT[v.state]);
  row.querySelector(".name").textContent = v.name;
  row.querySelector(".host").textContent = v.host;
  const [mapText, mapTitle] = mappingOf(v);
  const addr = row.querySelector(".addr");
  addr.replaceChildren(el("span", "tag", t(KINDS[v.kind]?.tag ?? v.kind)), el("span", "addr-text", mapText));
  addr.title = mapTitle;
  const open = row.querySelector(".open");
  open.hidden = v.kind === "socks";
  open.disabled = v.state !== "connected";
  const [text, cls] = healthOf(v);
  const health = row.querySelector(".health");
  health.textContent = text;
  health.title = text;
  health.className = `cell health ${cls}`;
  row.querySelector(".toggle").textContent = isActive(v) ? t("停止") : t("连接");
  open.textContent = t("打开");
  open.title = t("在浏览器中打开");
  row.querySelector(".edit").textContent = t("编辑");
  row.querySelector(".delete").textContent = t("删除");
}

async function reload() {
  const views = await call("list_tunnels");
  tunnels.clear();
  const list = $("list");
  const keep = new Set();
  for (const v of views) {
    tunnels.set(v.id, v);
    keep.add(v.id);
    if (!rows.has(v.id)) rows.set(v.id, buildRow(v.id));
    list.append(rows.get(v.id)); // re-append keeps order
    updateRow(v);
  }
  for (const [id, row] of rows) {
    if (!keep.has(id)) {
      row.remove();
      rows.delete(id);
    }
  }
  $("empty").hidden = views.length > 0;
  if (selectedId && !tunnels.has(selectedId)) select(null);
  else if (!selectedId && views.length) select(views[0].id);
}

// ---- logs ----------------------------------------------------------------
async function select(id) {
  selectedId = id;
  for (const [rid, row] of rows) row.classList.toggle("selected", rid === id);
  const log = $("log");
  if (!id) {
    $("log-title").textContent = "";
    log.replaceChildren(el("span", "muted", t("选择上方隧道查看其 ssh 日志…")));
    return;
  }
  $("log-title").textContent = `· ${tunnels.get(id)?.name ?? ""}`;
  const lines = await call("tunnel_logs", { id });
  if (selectedId !== id) return;
  log.textContent = lines.join("\n");
  log.scrollTop = log.scrollHeight;
}

function appendLog(line) {
  const log = $("log");
  const atBottom = log.scrollHeight - log.scrollTop - log.clientHeight < 24;
  if (log.firstChild && log.firstChild.nodeType !== Node.TEXT_NODE) log.textContent = "";
  log.append((log.textContent ? "\n" : "") + line);
  const text = log.textContent;
  const lines = text.split("\n");
  if (lines.length > MAX_LOG_LINES) log.textContent = lines.slice(-MAX_LOG_LINES).join("\n");
  if (atBottom) log.scrollTop = log.scrollHeight;
}

// ---- editor dialog -------------------------------------------------------
let editing = null; // TunnelView being edited, or null for new
let hosts = [];
let chosenHost = null;

const currentKind = () => document.querySelector('input[name="kind"]:checked')?.value ?? "socks";

function applyKind() {
  const kind = currentKind();
  const k = KINDS[kind];
  $("port-label").textContent = t(k.portLabel);
  $("rport-label").textContent = k.rportLabel ? t(k.rportLabel) : "";
  $("kind-help").textContent = t(k.help);
  for (const node of document.querySelectorAll("#editor [data-kinds]")) {
    node.hidden = !node.dataset.kinds.split(" ").includes(kind);
  }
}

async function onKindChange() {
  applyKind();
  if (editing) return;
  const kind = currentKind();
  const first = KINDS[kind].firstPort;
  // A remote forward publishes an existing local service, so its port is not
  // a free one to pick; default both sides to the same common port.
  $("port").value = kind === "remote" ? first : await call("suggest_port", { start: first });
  if (kind === "remote" && !$("rport").value) $("rport").value = first;
}

function renderHosts() {
  const q = $("search").value.trim().toLowerCase();
  const list = $("hosts");
  list.replaceChildren();
  if (!hosts.length) {
    list.append(el("li", "placeholder", t("未在 ~/.ssh/config 中找到主机")));
    return;
  }
  const shown = hosts.filter(
    (h) => !q || h.alias.toLowerCase().includes(q) || h.hostname.toLowerCase().includes(q)
  );
  if (!shown.length) {
    list.append(el("li", "placeholder", t("没有匹配的主机")));
    return;
  }
  for (const h of shown) {
    const li = el("li");
    li.setAttribute("role", "option");
    li.append(el("span", null, h.alias));
    if (h.hostname && h.hostname !== h.alias) li.append(el("span", "muted mono", h.hostname));
    if (h.alias === chosenHost) li.classList.add("selected");
    li.addEventListener("click", () => {
      chosenHost = h.alias;
      if (!$("name").value.trim()) $("name").value = h.alias;
      renderHosts();
    });
    li.addEventListener("dblclick", () => submitEditor());
    list.append(li);
  }
  list.querySelector(".selected")?.scrollIntoView({ block: "nearest" });
}

async function openEditor(view) {
  if (view && isActive(view)) {
    toast(t("编辑前请先停止该隧道。"));
    return;
  }
  editing = view ?? null;
  $("editor-title").textContent = view ? t("编辑隧道") : t("新建隧道");
  $("editor-error").hidden = true;
  $("search").value = "";
  const kind = view ? view.kind : "socks";
  const [hostList, port, probe] = await Promise.all([
    call("list_hosts"),
    view ? view.port : call("suggest_port", { start: KINDS[kind].firstPort }),
    call("default_probe_url"),
  ]);
  document.querySelector(`input[name="kind"][value="${kind}"]`).checked = true;
  $("target").value = view ? view.target_host : "127.0.0.1";
  $("rport").value = view && view.remote_port ? view.remote_port : "";
  applyKind();
  hosts = hostList;
  chosenHost = view ? view.host : null;
  $("name").value = view ? view.name : "";
  $("port").value = port;
  $("probe").value = view ? view.probe_url : probe;
  $("auto").checked = view ? view.auto_reconnect : true;
  renderHosts();
  $("editor").showModal();
  $("search").focus();
}

function showEditorError(message) {
  const box = $("editor-error");
  box.textContent = message;
  box.hidden = false;
}

async function submitEditor() {
  const kind = currentKind();
  const valid = (p) => Number.isInteger(p) && p >= 1 && p <= 65535;
  const port = Number($("port").value);
  const remotePort = kind === "socks" ? 0 : Number($("rport").value);
  if (!valid(port) || (kind !== "socks" && !valid(remotePort))) {
    showEditorError(t("端口必须在 1–65535 之间。"));
    return;
  }
  const input = {
    id: editing ? editing.id : null,
    name: $("name").value,
    host: chosenHost ?? "",
    kind,
    port,
    remote_port: remotePort,
    target_host: $("target").value,
    probe_url: $("probe").value,
    auto_reconnect: $("auto").checked,
  };
  try {
    await invoke("save_tunnel", { input });
  } catch (err) {
    showEditorError(String(err));
    return;
  }
  $("editor").close();
  await reload();
}

// ---- confirmation ------------------------------------------------------------
function ask(title, text, okLabel) {
  const dialog = $("confirm");
  $("confirm-title").textContent = title;
  $("confirm-text").textContent = text;
  $("confirm-ok").textContent = okLabel;
  dialog.returnValue = "";
  return new Promise((resolve) => {
    dialog.onclose = () => resolve(dialog.returnValue === "ok");
    dialog.showModal();
  });
}

async function confirmDelete(view) {
  if (!(await ask(t("删除隧道"), t("确定删除「{name}」吗？运行中的隧道会被立即断开。", { name: view.name }), t("删除")))) return;
  await call("delete_tunnel", { id: view.id });
  await reload();
}

// ---- tabs ----------------------------------------------------------------------
let currentView = "tunnels";

async function showView(name) {
  if (name === currentView) return;
  if (currentView === "config" && hostDirty && !(await ask(t("放弃修改"), t("当前主机的修改还没有保存，确定离开吗？"), t("放弃修改")))) return;
  if (currentView === "config") hostDirty = false;
  currentView = name;
  for (const tab of document.querySelectorAll(".tab")) tab.classList.toggle("active", tab.dataset.view === name);
  for (const v of ["tunnels", "config", "keys"]) $(`view-${v}`).hidden = v !== name;
  $("tunnel-actions").hidden = name !== "tunnels";
  $("key-actions").hidden = name !== "keys";
  if (name === "config") loadConfig();
  if (name === "keys") loadKeys();
}

// ---- clipboard -----------------------------------------------------------------
async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    // Older WebKitGTK: fall back to a hidden textarea. It must live inside
    // an open modal dialog, since everything outside it is inert.
    const area = document.createElement("textarea");
    area.value = text;
    area.style.cssText = "position:fixed;opacity:0;left:0;top:0;";
    (document.querySelector("dialog[open]") ?? document.body).append(area);
    area.select();
    const ok = document.execCommand("copy");
    area.remove();
    return ok;
  }
}

async function copyKey(key) {
  toast((await copyText(key.public_key)) ? t("已复制 {name} 的公钥", { name: key.name }) : t("复制失败，请手动选中复制"));
}

// ---- keys view -----------------------------------------------------------------
let keys = [];

async function loadKeys() {
  keys = await call("list_keys");
  const list = $("key-list");
  list.replaceChildren();
  for (const key of keys) {
    const row = el("div", "row key-row");
    const view = el("button", "btn small", t("查看"));
    const copy = el("button", "btn small primary", t("复制公钥"));
    view.addEventListener("click", () => showKey(key));
    copy.addEventListener("click", () => copyKey(key));
    const ops = el("span", "ops");
    ops.append(view, copy);
    const fp = el("span", "cell mono", key.fingerprint);
    fp.title = key.fingerprint;
    const comment = el("span", "cell muted", key.comment || "—");
    comment.title = key.comment;
    row.append(el("span", "cell", key.name), el("span", "cell mono", key.kind.replace(/^ssh-/, "")), fp, comment, ops);
    row.addEventListener("dblclick", () => showKey(key));
    list.append(row);
  }
  $("key-empty").hidden = keys.length > 0;
}

let shownKey = null;
function showKey(key) {
  shownKey = key;
  $("key-title").textContent = `${key.name}.pub`;
  $("key-meta").textContent = `${key.kind} · ${key.fingerprint}`;
  $("key-text").value = key.public_key;
  $("key-dialog").showModal();
  $("key-text").select();
}

// ---- generate / import keys ------------------------------------------------------
// Private-key files are named `name`, public ones `name.pub`, both in ~/.ssh.
const NAME_RE = /^[A-Za-z0-9_-][A-Za-z0-9._-]*$/;

/// Client-side name check; the backend repeats it against the real files.
function nameProblem(name) {
  if (!name) return t("请填写文件名。");
  if (!NAME_RE.test(name)) return t("文件名只能包含字母、数字、点、下划线和减号，且不能以点开头。");
  if (name.endsWith(".pub")) return t("文件名不需要带 .pub，公钥会自动保存为「文件名.pub」。");
  if (keys.some((k) => k.name === name)) return t("「{name}」已存在，请换一个文件名。", { name });
  return "";
}

function showError(id, message) {
  $(id).textContent = message;
  $(id).hidden = !message;
}

let genNameTouched = false;

async function openGenerate() {
  genNameTouched = false;
  $("gen-form").reset();
  showError("gen-error", "");
  await onGenKindChange();
  $("gen-dialog").showModal();
  $("gen-name").focus();
}

const genKind = () => document.querySelector('input[name="gen-kind"]:checked').value;

async function onGenKindChange() {
  const rsa = genKind() === "rsa";
  for (const node of document.querySelectorAll("[data-rsa]")) node.hidden = !rsa;
  const d = await call("key_defaults", { kind: genKind() });
  // Follow the type (id_ed25519 ↔ id_rsa) until the user types a name.
  if (!genNameTouched) $("gen-name").value = d.name;
  if (!$("gen-comment").value) $("gen-comment").value = d.comment;
  onGenNameInput();
}

function onGenNameInput() {
  const name = $("gen-name").value.trim();
  $("gen-where").textContent = name ? t("将创建 ~/.ssh/{name} 和 {name}.pub", { name }) : "";
  showError("gen-error", name ? nameProblem(name) : "");
}

async function submitGenerate() {
  const name = $("gen-name").value.trim();
  const problem = nameProblem(name);
  if (problem) return showError("gen-error", problem);
  if ($("gen-pass").value !== $("gen-pass2").value) return showError("gen-error", t("两次输入的口令不一致。"));
  const kind = genKind();
  const ok = $("gen-ok");
  ok.disabled = true;
  ok.textContent = t("生成中…");
  try {
    const key = await invoke("generate_key", {
      input: {
        name,
        kind,
        bits: kind === "rsa" ? Number(document.querySelector('input[name="gen-bits"]:checked').value) : null,
        comment: $("gen-comment").value.trim(),
        passphrase: $("gen-pass").value,
      },
    });
    $("gen-dialog").close();
    await loadKeys();
    toast(t("已生成 {name}", { name: key.name }));
    showKey(key);
  } catch (err) {
    showError("gen-error", String(err));
  } finally {
    ok.disabled = false;
    ok.textContent = t("生成");
  }
}

let importChecked = false; // the current inputs passed every check

function openImport() {
  $("import-form").reset();
  resetImportCheck();
  $("import-dialog").showModal();
}

function resetImportCheck() {
  importChecked = false;
  $("imp-ok").disabled = true;
  $("imp-steps").hidden = true;
  $("imp-summary").hidden = true;
  showError("imp-error", "");
  const name = $("imp-name").value.trim();
  $("imp-where").textContent = name ? t("将保存为 ~/.ssh/{name} 和 {name}.pub", { name }) : "";
}

function importInput() {
  return {
    name: $("imp-name").value.trim(),
    private_key: $("imp-private").value,
    public_key: $("imp-public").value,
    passphrase: $("imp-pass").value,
  };
}

async function pickImportFiles(files) {
  for (const file of files) {
    if (file.size > 64 * 1024) {
      showError("imp-error", t("{name} 太大了，不像是密钥文件。", { name: file.name }));
      continue;
    }
    const text = await file.text();
    if (file.name.endsWith(".pub") || /^(ssh-|ecdsa-|sk-)/.test(text.trim())) {
      $("imp-public").value = text.trim();
    } else {
      $("imp-private").value = text.trim();
      // Default to the source file name; the duplicate check still applies.
      if (!$("imp-name").value.trim()) $("imp-name").value = file.name.replace(/\.(pem|key|txt)$/i, "");
    }
  }
  $("imp-file").value = "";
  resetImportCheck();
}

async function checkImport() {
  const input = importInput();
  const problem = nameProblem(input.name);
  resetImportCheck();
  const btn = $("imp-check");
  btn.disabled = true;
  btn.textContent = t("校验中…");
  try {
    const report = await invoke("check_key_import", { input });
    const list = $("imp-steps");
    list.replaceChildren(
      ...report.steps.map((step) => {
        const li = el("li");
        li.append(
          el("span", `mark ${step.ok ? "ok" : "bad"}`, step.ok ? "✓" : "✗"),
          el("span", "", step.title),
          el("span", "detail", step.detail),
        );
        return li;
      }),
    );
    list.hidden = false;
    $("imp-summary").textContent = report.summary;
    $("imp-summary").hidden = !report.summary;
    importChecked = report.ok && !problem;
    $("imp-ok").disabled = !importChecked;
    if (report.ok && problem) showError("imp-error", problem);
    return importChecked;
  } catch (err) {
    showError("imp-error", String(err));
    return false;
  } finally {
    btn.disabled = false;
    btn.textContent = t("校验");
  }
}

async function submitImport() {
  if (!importChecked && !(await checkImport())) return;
  try {
    const key = await invoke("import_key", { input: importInput() });
    $("import-dialog").close();
    await loadKeys();
    toast(t("已导入 {name}", { name: key.name }));
    showKey(key);
  } catch (err) {
    showError("imp-error", String(err));
  }
}

// ---- ssh config view -----------------------------------------------------------
let blocks = [];
let configPath = "";
let selectedBlock = null; // HostBlock being edited, or null for a new one
let hostDirty = false;

const blockKey = (b) => `${b.file}\n${b.line}`;
const sshDir = () => configPath.replace(/[\\/]config$/, "");
const concreteAliases = (patterns) => patterns.split(/\s+/).filter((a) => a && !/[*?!]/.test(a));

function whereOf(b) {
  const dir = sshDir();
  const file = b.file === configPath ? "config" : b.file.startsWith(dir) ? b.file.slice(dir.length + 1) : b.file;
  return t("~/.ssh/{file} 第 {line} 行", { file: file.replace(/\\/g, "/"), line: b.line + 1 });
}

async function loadConfig(select) {
  const view = await call("list_ssh_hosts");
  blocks = view.blocks;
  configPath = view.path;
  $("config-path").textContent = configPath;
  $("config-path").title = configPath;
  const [keyList] = await Promise.all([call("list_keys")]);
  $("key-options").replaceChildren(
    ...keyList.filter((k) => k.identity_file).map((k) => {
      const o = document.createElement("option");
      o.value = k.identity_file;
      o.label = `${k.kind} ${k.comment}`;
      return o;
    })
  );
  $("host-options").replaceChildren(
    ...blocks.flatMap((b) => concreteAliases(b.patterns)).map((a) => {
      const o = document.createElement("option");
      o.value = a;
      return o;
    })
  );
  if (select) selectedBlock = blocks.find((b) => blockKey(b) === blockKey(select)) ?? null;
  else if (selectedBlock) selectedBlock = blocks.find((b) => blockKey(b) === blockKey(selectedBlock)) ?? null;
  renderBlocks();
  if (selectedBlock) fillHostForm(selectedBlock);
  else if (!$("host-form").hidden && !isNewHost) closeHostForm();
}

function renderBlocks() {
  const q = $("host-filter").value.trim().toLowerCase();
  const list = $("host-blocks");
  list.replaceChildren();
  const shown = blocks.filter(
    (b) => !q || b.patterns.toLowerCase().includes(q) || b.hostname.toLowerCase().includes(q)
  );
  if (!shown.length) {
    list.append(el("li", "placeholder", blocks.length ? t("没有匹配的主机") : t("~/.ssh/config 中还没有主机")));
    return;
  }
  for (const b of shown) {
    const li = el("li");
    const name = el("div", "b-name");
    name.append(el("span", null, b.patterns));
    if (b.proxy_jump) name.append(el("span", "tag", t("跳板")));
    if (b.proxy_command) name.append(el("span", "tag", "ProxyCommand"));
    const sub = [b.user && `${b.user}@`, b.hostname || "", b.port && `:${b.port}`].join("");
    li.append(name, el("div", "b-sub", sub || (b.file === configPath ? "—" : whereOf(b))));
    li.title = whereOf(b);
    if (selectedBlock && blockKey(b) === blockKey(selectedBlock)) li.classList.add("selected");
    li.addEventListener("click", () => selectBlock(b));
    list.append(li);
  }
}

let isNewHost = false;

async function selectBlock(b) {
  if (hostDirty && !(await ask(t("放弃修改"), t("当前主机的修改还没有保存，确定切换吗？"), t("放弃修改")))) return;
  selectedBlock = b;
  isNewHost = !b;
  renderBlocks();
  fillHostForm(b);
}

const currentVia = () => document.querySelector('input[name="via"]:checked')?.value ?? "direct";

function applyVia() {
  const via = currentVia();
  for (const node of document.querySelectorAll("#host-form [data-via]")) node.hidden = node.dataset.via !== via;
}

function fillHostForm(b) {
  const form = $("host-form");
  form.hidden = false;
  $("host-empty").hidden = true;
  $("host-error").hidden = true;
  $("host-test-out").hidden = true;
  hostFormHeader(b);
  $("h-patterns").value = b?.patterns ?? "";
  $("h-hostname").value = b?.hostname ?? "";
  $("h-user").value = b?.user ?? "";
  $("h-port").value = b?.port ?? "";
  $("h-identity").value = b?.identity_file ?? "";
  $("h-jump").value = b?.proxy_jump ?? "";
  $("h-command").value = b?.proxy_command ?? "";
  const via = b?.proxy_command ? "command" : b?.proxy_jump ? "jump" : "direct";
  document.querySelector(`input[name="via"][value="${via}"]`).checked = true;
  applyVia();
  const others = b?.others ?? [];
  $("h-others").textContent = others.join("\n");
  for (const node of document.querySelectorAll("#host-form [data-others]")) node.hidden = !others.length;
  $("host-test").disabled = !b;
  $("host-delete").disabled = !b;
  hostDirty = false;
  if (!b) $("h-patterns").focus();
}

function hostFormHeader(b) {
  $("host-form-title").textContent = b ? t("编辑主机 {name}", { name: b.patterns }) : t("新建主机");
  $("host-form-where").textContent = b ? t("位于 {where}", { where: whereOf(b) }) : t("将添加到 ~/.ssh/config（放在 Host * 之前）");
}

function closeHostForm() {
  $("host-form").hidden = true;
  $("host-empty").hidden = false;
  hostDirty = false;
  isNewHost = false;
}

function showHostError(message) {
  const box = $("host-error");
  box.textContent = message;
  box.hidden = !message;
}

async function saveHost() {
  const via = currentVia();
  const input = {
    file: selectedBlock?.file ?? null,
    line: selectedBlock?.line ?? null,
    original_patterns: selectedBlock?.patterns ?? null,
    patterns: $("h-patterns").value,
    hostname: $("h-hostname").value,
    user: $("h-user").value,
    port: $("h-port").value,
    identity_file: $("h-identity").value,
    proxy_jump: via === "jump" ? $("h-jump").value : "",
    proxy_command: via === "command" ? $("h-command").value : "",
  };
  if (via === "jump" && !input.proxy_jump.trim()) return showHostError(t("请填写跳板机。"));
  if (via === "command" && !input.proxy_command.trim()) return showHostError(t("请填写 ProxyCommand。"));
  let saved;
  try {
    saved = await invoke("save_ssh_host", { input });
  } catch (err) {
    showHostError(String(err));
    return;
  }
  hostDirty = false;
  isNewHost = false;
  toast(t("已保存到 ~/.ssh/config"));
  await loadConfig(saved);
}

async function deleteHost() {
  const b = selectedBlock;
  if (!b || !(await ask(t("删除主机"), t("确定从 ssh 配置中删除「{name}」吗？", { name: b.patterns }), t("删除")))) return;
  await call("delete_ssh_host", { file: b.file, line: b.line, patterns: b.patterns });
  selectedBlock = null;
  closeHostForm();
  toast(t("已删除"));
  await loadConfig();
}

async function testHost() {
  const alias = selectedBlock && concreteAliases(selectedBlock.patterns)[0];
  if (!alias) return toast(t("通配符主机无法直接测试"));
  if (hostDirty) return toast(t("请先保存修改再测试"));
  const out = $("host-test-out");
  out.hidden = false;
  out.className = "test-out";
  out.textContent = t("正在测试 ssh {alias} …", { alias });
  $("host-test").disabled = true;
  try {
    const r = await call("test_ssh_host", { alias });
    out.className = `test-out ${r.ok ? "ok" : "bad"}`;
    out.textContent = `${r.ok ? t("✓ 连接成功") : t("✗ 连接失败")}\n${r.output}`;
  } finally {
    $("host-test").disabled = false;
  }
}

// ---- wiring ----------------------------------------------------------------
for (const tab of document.querySelectorAll(".tab")) {
  tab.addEventListener("click", () => showView(tab.dataset.view));
}
$("host-filter").addEventListener("input", renderBlocks);
$("host-add").addEventListener("click", () => selectBlock(null));
$("config-open").addEventListener("click", () => call("open_ssh_config"));
$("host-form").addEventListener("input", () => (hostDirty = true));
$("host-form").addEventListener("submit", (e) => {
  e.preventDefault();
  saveHost();
});
$("host-reset").addEventListener("click", () => {
  if (selectedBlock) fillHostForm(selectedBlock);
  else closeHostForm();
});
$("host-delete").addEventListener("click", deleteHost);
$("host-test").addEventListener("click", testHost);
for (const radio of document.querySelectorAll('input[name="via"]')) {
  radio.addEventListener("change", applyVia);
}
for (const btn of document.querySelectorAll("[data-tpl]")) {
  btn.addEventListener("click", () => {
    $("h-command").value = btn.dataset.tpl;
    hostDirty = true;
    $("h-command").focus();
  });
}
$("key-copy").addEventListener("click", () => shownKey && copyKey(shownKey));
$("key-generate").addEventListener("click", openGenerate);
$("key-import").addEventListener("click", openImport);
for (const radio of document.querySelectorAll('input[name="gen-kind"]')) {
  radio.addEventListener("change", onGenKindChange);
}
$("gen-name").addEventListener("input", () => {
  genNameTouched = true;
  onGenNameInput();
});
$("gen-cancel").addEventListener("click", () => $("gen-dialog").close());
$("gen-form").addEventListener("submit", (e) => {
  e.preventDefault();
  submitGenerate();
});
$("imp-pick").addEventListener("click", () => $("imp-file").click());
$("imp-file").addEventListener("change", () => pickImportFiles([...$("imp-file").files]));
for (const id of ["imp-private", "imp-public", "imp-pass", "imp-name"]) {
  $(id).addEventListener("input", resetImportCheck);
}
$("imp-check").addEventListener("click", checkImport);
$("imp-cancel").addEventListener("click", () => $("import-dialog").close());
$("import-form").addEventListener("submit", (e) => {
  e.preventDefault();
  submitImport();
});

$("add").addEventListener("click", () => openEditor(null));
$("start-all").addEventListener("click", () => call("start_all"));
$("stop-all").addEventListener("click", () => call("stop_all"));
$("search").addEventListener("input", renderHosts);
for (const radio of document.querySelectorAll('input[name="kind"]')) {
  radio.addEventListener("change", onKindChange);
}
$("editor-cancel").addEventListener("click", () => $("editor").close());
$("editor-form").addEventListener("submit", (e) => {
  e.preventDefault();
  submitEditor();
});

listen("tunnel-changed", ({ payload }) => {
  tunnels.set(payload.id, payload);
  updateRow(payload);
});
listen("tunnel-log", ({ payload }) => {
  if (payload.id === selectedId) appendLog(payload.line);
});

// ---- language + theme ------------------------------------------------------------
const prefs = { lang: "zh", theme: "system" };
const THEMES = {
  system: ["◐", "主题：跟随系统"],
  light: ["☀", "主题：浅色"],
  dark: ["☾", "主题：深色"],
};

function applyTheme() {
  const root = document.documentElement;
  if (prefs.theme === "system") delete root.dataset.theme;
  else root.dataset.theme = prefs.theme;
  const [icon, title] = THEMES[prefs.theme];
  $("theme-toggle").textContent = icon;
  $("theme-toggle").title = t(title);
}

function applyLang() {
  setUiLang(prefs.lang);
  $("lang-toggle").textContent = prefs.lang === "en" ? "中" : "EN";
  $("lang-toggle").title = prefs.lang === "en" ? t("切换到中文") : t("切换到英文");
  applyTheme();
  // Text that code filled in with parameters.
  for (const v of tunnels.values()) updateRow(v);
  if (!selectedId) select(null);
  if (keys.length) loadKeys();
  renderBlocks();
  if (!$("host-form").hidden) hostFormHeader(selectedBlock);
}

async function savePrefs() {
  try {
    await invoke("set_prefs", { prefs });
  } catch (err) {
    toast(String(err));
  }
}

async function initPrefs() {
  try {
    const saved = await invoke("get_prefs");
    prefs.lang = saved.lang || (/^zh\b/i.test(navigator.language) ? "zh" : "en");
    prefs.theme = THEMES[saved.theme] ? saved.theme : "system";
    applyLang();
    // Tell the backend (messages, tray menu, window theme) what the UI chose.
    if (!saved.lang || !saved.theme) await savePrefs();
  } finally {
    document.documentElement.dataset.ready = "";
  }
}

$("lang-toggle").addEventListener("click", () => {
  prefs.lang = prefs.lang === "en" ? "zh" : "en";
  applyLang();
  savePrefs();
});
$("theme-toggle").addEventListener("click", () => {
  const order = Object.keys(THEMES);
  prefs.theme = order[(order.indexOf(prefs.theme) + 1) % order.length];
  applyTheme();
  savePrefs();
});

initPrefs().finally(reload);
