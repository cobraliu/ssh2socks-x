"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const STATE_TEXT = { stopped: "已停止", connecting: "连接中", connected: "已连接", error: "错误" };
const MAX_LOG_LINES = 500;

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
      return ["连接中…", ""];
    case "connected":
      if (!v.probe) return ["检测中…", ""];
      return v.probe.ok
        ? [`通 ${Math.round(v.probe.latency_ms)}ms`, "ok"]
        : [`不通 · ${v.probe.message}`, "bad"];
    case "error":
      return ["错误", "bad"];
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
  const edit = el("button", "btn small", "编辑");
  const del = el("button", "btn small danger", "删除");
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
  ops.append(toggle, edit, del);

  row.append(status, name, host, addr, health, ops);
  row.addEventListener("click", () => select(id));
  return row;
}

function updateRow(v) {
  const row = rows.get(v.id);
  if (!row) return;
  row.querySelector(".dot").className = `dot ${v.state}`;
  row.querySelector(".state-text").textContent = STATE_TEXT[v.state];
  row.querySelector(".name").textContent = v.name;
  row.querySelector(".host").textContent = v.host;
  row.querySelector(".addr").textContent = `127.0.0.1:${v.port}`;
  row.querySelector(".addr").title = `SOCKS5 代理地址：127.0.0.1:${v.port}`;
  const [text, cls] = healthOf(v);
  const health = row.querySelector(".health");
  health.textContent = text;
  health.title = text;
  health.className = `cell health ${cls}`;
  row.querySelector(".toggle").textContent = isActive(v) ? "停止" : "连接";
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
    log.replaceChildren(el("span", "muted", "选择上方隧道查看其 ssh 日志…"));
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

function renderHosts() {
  const q = $("search").value.trim().toLowerCase();
  const list = $("hosts");
  list.replaceChildren();
  if (!hosts.length) {
    list.append(el("li", "placeholder", "未在 ~/.ssh/config 中找到主机"));
    return;
  }
  const shown = hosts.filter(
    (h) => !q || h.alias.toLowerCase().includes(q) || h.hostname.toLowerCase().includes(q)
  );
  if (!shown.length) {
    list.append(el("li", "placeholder", "没有匹配的主机"));
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
    toast("编辑前请先停止该隧道。");
    return;
  }
  editing = view ?? null;
  $("editor-title").textContent = view ? "编辑隧道" : "新建隧道";
  $("editor-error").hidden = true;
  $("search").value = "";
  const [hostList, port, probe] = await Promise.all([
    call("list_hosts"),
    view ? view.port : call("suggest_port"),
    call("default_probe_url"),
  ]);
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
  const port = Number($("port").value);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    showEditorError("端口必须在 1–65535 之间。");
    return;
  }
  const input = {
    id: editing ? editing.id : null,
    name: $("name").value,
    host: chosenHost ?? "",
    port,
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

// ---- delete confirmation ---------------------------------------------------
function confirmDelete(view) {
  const dialog = $("confirm");
  $("confirm-text").textContent = `确定删除「${view.name}」吗？运行中的隧道会被立即断开。`;
  dialog.returnValue = "";
  dialog.onclose = async () => {
    if (dialog.returnValue !== "ok") return;
    await call("delete_tunnel", { id: view.id });
    await reload();
  };
  dialog.showModal();
}

// ---- wiring ----------------------------------------------------------------
$("add").addEventListener("click", () => openEditor(null));
$("start-all").addEventListener("click", () => call("start_all"));
$("stop-all").addEventListener("click", () => call("stop_all"));
$("search").addEventListener("input", renderHosts);
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

reload();
