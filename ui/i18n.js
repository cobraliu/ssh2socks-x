"use strict";

// UI text is written in Chinese; EN maps each Chinese string to English.
// `t()` translates strings built in code, and `localizeDom()` translates the
// static page (text nodes, a few attributes and `data-i18n-html` blocks).

const EN = {
  // header + tabs
  "SSH 代理与端口转发": "SSH proxies and port forwarding",
  "隧道": "Tunnels",
  "SSH 配置": "SSH config",
  "密钥": "Keys",
  "全部启动": "Start all",
  "全部停止": "Stop all",
  "＋ 新建隧道": "＋ New tunnel",
  "导入密钥…": "Import key…",
  "＋ 生成密钥": "＋ Generate key",
  "切换到英文": "Switch to English",
  "切换到中文": "Switch to Chinese",
  "主题：跟随系统": "Theme: system",
  "主题：浅色": "Theme: light",
  "主题：深色": "Theme: dark",

  // tunnels view
  "状态": "Status",
  "名称": "Name",
  "映射": "Mapping",
  "连通": "Health",
  "操作": "Actions",
  "还没有隧道": "No tunnels yet",
  "日志": "Log",
  "选择上方隧道查看其 ssh 日志…": "Select a tunnel above to see its ssh log…",
  "已停止": "Stopped",
  "连接中": "Connecting",
  "已连接": "Connected",
  "错误": "Error",
  "打开": "Open",
  "在浏览器中打开": "Open in browser",
  "已在浏览器打开 {url}": "Opened {url} in the browser",
  "编辑": "Edit",
  "删除": "Delete",
  "停止": "Stop",
  "连接": "Connect",
  "连接中…": "Connecting…",
  "检测中…": "Checking…",
  "不通 · {message}": "Down · {message}",
  "通": "Up",
  "通 {ms}ms": "Up {ms}ms",
  "错误 · {detail}": "Error · {detail}",
  "本机 127.0.0.1:{port} → 服务器上的 {target}": "local 127.0.0.1:{port} → {target} on the server",
  ":{rport} → 本机:{port}": ":{rport} → local:{port}",
  "服务器 0.0.0.0:{rport} → 本机 127.0.0.1:{port}": "server 0.0.0.0:{rport} → local 127.0.0.1:{port}",
  "SOCKS5 代理地址：127.0.0.1:{port}": "SOCKS5 proxy: 127.0.0.1:{port}",

  // tunnel editor
  "新建隧道": "New tunnel",
  "编辑隧道": "Edit tunnel",
  "搜索名称或 IP / 主机名…": "Search by name, IP or host name…",
  "类型": "Type",
  "SOCKS5 代理": "SOCKS5 proxy",
  "本地转发": "Local forward",
  "远程转发": "Remote forward",
  "本地 SOCKS 端口": "Local SOCKS port",
  "本机端口": "Local port",
  "本机服务端口": "Local service port",
  "目标地址": "Target host",
  "目标端口": "Target port",
  "服务器端口": "Server port",
  "127.0.0.1（或服务器所在局域网的 IP）": "127.0.0.1 (or an IP on the server's LAN)",
  "探测地址": "Probe URL",
  "进程退出时自动重连": "Reconnect automatically when ssh exits",
  "在本机开一个 SOCKS5 代理，流量经服务器出去。":
    "Opens a SOCKS5 proxy on this machine; traffic leaves through the server.",
  "把服务器能访问到的端口（服务器本机或其局域网）映射到本机，用 127.0.0.1:本机端口 访问。":
    "Maps a port the server can reach (on the server itself or its LAN) to this machine. Connect to 127.0.0.1:<local port>.",
  "把本机端口发布到服务器 0.0.0.0:服务器端口。需 sshd 设置 GatewayPorts yes / clientspecified，否则只监听服务器 127.0.0.1。":
    "Publishes a local port on the server's 0.0.0.0:<server port>. Needs GatewayPorts yes / clientspecified in sshd, otherwise it only listens on the server's 127.0.0.1.",
  "未在 ~/.ssh/config 中找到主机": "No hosts found in ~/.ssh/config",
  "没有匹配的主机": "No matching hosts",
  "编辑前请先停止该隧道。": "Stop the tunnel before editing it.",
  "端口必须在 1–65535 之间。": "Ports must be between 1 and 65535.",
  "取消": "Cancel",
  "确定": "OK",
  "删除隧道": "Delete tunnel",
  "确定删除「{name}」吗？运行中的隧道会被立即断开。":
    "Delete \"{name}\"? If it is running, it is disconnected right away.",

  // ssh config view
  "搜索主机…": "Search hosts…",
  "＋ 添加": "＋ Add",
  "用编辑器打开": "Open in editor",
  "新建主机": "New host",
  "编辑主机 {name}": "Edit host {name}",
  "位于 {where}": "In {where}",
  "将添加到 ~/.ssh/config（放在 Host * 之前）": "Will be added to ~/.ssh/config (before Host *)",
  "~/.ssh/{file} 第 {line} 行": "~/.ssh/{file}, line {line}",
  "别名 Host": "Alias (Host)",
  "例如 web，多个别名用空格分隔": "e.g. web; separate several aliases with spaces",
  "地址 HostName": "Address (HostName)",
  "IP 或域名": "IP or domain name",
  "用户 User": "User",
  "留空使用当前用户名": "Leave empty for your current user name",
  "端口 Port": "Port",
  "密钥 IdentityFile": "Key (IdentityFile)",
  "留空使用默认密钥（~/.ssh/id_*）": "Leave empty for the default keys (~/.ssh/id_*)",
  "连接方式": "Connect via",
  "直连": "Direct",
  "跳板机 ProxyJump": "Jump host (ProxyJump)",
  "跳板机": "Jump host",
  "跳板主机的别名，或 user@host:port": "Alias of the jump host, or user@host:port",
  "例如 ssh -W %h:%p 跳板机": "e.g. ssh -W %h:%p jumphost",
  "模板：": "Templates:",
  "ssh -W %h:%p 跳板机别名": "ssh -W %h:%p jumphost-alias",
  "经跳板机": "Via jump host",
  "经 SOCKS5 代理": "Via SOCKS5 proxy",
  "经 HTTP 代理": "Via HTTP proxy",
  "ncat（Windows）": "ncat (Windows)",
  "%h、%p 会被替换成目标地址和端口。Windows 自带的 OpenSSH 没有 nc，请使用「经跳板机」或安装 Nmap 附带的 ncat。":
    "%h and %p are replaced with the target host and port. The OpenSSH that ships with Windows has no nc; use \"Via jump host\" or install the ncat that comes with Nmap.",
  "其它选项": "Other options",
  "测试连接": "Test connection",
  "撤销修改": "Revert",
  "保存": "Save",
  "选择左侧的主机进行编辑": "Select a host on the left to edit it",
  "~/.ssh/config 中还没有主机": "No hosts in ~/.ssh/config yet",
  "跳板": "Jump",
  "放弃修改": "Discard changes",
  "当前主机的修改还没有保存，确定离开吗？": "Your changes to this host are not saved. Leave anyway?",
  "当前主机的修改还没有保存，确定切换吗？": "Your changes to this host are not saved. Switch anyway?",
  "请填写跳板机。": "Enter a jump host.",
  "请填写 ProxyCommand。": "Enter a ProxyCommand.",
  "已保存到 ~/.ssh/config": "Saved to ~/.ssh/config",
  "删除主机": "Delete host",
  "确定从 ssh 配置中删除「{name}」吗？": "Delete \"{name}\" from the ssh config?",
  "已删除": "Deleted",
  "通配符主机无法直接测试": "Wildcard hosts can't be tested directly",
  "请先保存修改再测试": "Save your changes before testing",
  "正在测试 ssh {alias} …": "Testing ssh {alias} …",
  "✓ 连接成功": "✓ Connected",
  "✗ 连接失败": "✗ Connection failed",

  // keys view
  "指纹": "Fingerprint",
  "注释": "Comment",
  "查看": "View",
  "复制公钥": "Copy public key",
  "关闭": "Close",
  "已复制 {name} 的公钥": "Copied the public key of {name}",
  "复制失败，请手动选中复制": "Copy failed; select the text and copy it yourself",
  "生成密钥对": "Generate key pair",
  "Ed25519（推荐）": "Ed25519 (recommended)",
  "长度": "Size",
  "文件名": "File name",
  "例如 me@laptop，便于在服务器上认出这把钥匙": "e.g. me@laptop, so you can recognize the key on servers",
  "口令": "Passphrase",
  "可留空": "Optional",
  "确认口令": "Confirm passphrase",
  "生成": "Generate",
  "生成中…": "Generating…",
  "已生成 {name}": "Generated {name}",
  "两次输入的口令不一致。": "The passphrases don't match.",
  "将创建 ~/.ssh/{name} 和 {name}.pub": "Will create ~/.ssh/{name} and {name}.pub",
  "将保存为 ~/.ssh/{name} 和 {name}.pub": "Will be saved as ~/.ssh/{name} and {name}.pub",
  "请填写文件名。": "Enter a file name.",
  "文件名只能包含字母、数字、点、下划线和减号，且不能以点开头。":
    "File names may only contain letters, digits, dots, underscores and hyphens, and can't start with a dot.",
  "文件名不需要带 .pub，公钥会自动保存为「文件名.pub」。":
    "Leave out .pub; the public key is saved as \"<name>.pub\" automatically.",
  "「{name}」已存在，请换一个文件名。": "\"{name}\" already exists. Choose another file name.",
  "导入密钥对": "Import key pair",
  "私钥": "Private key",
  "选择文件…": "Choose files…",
  "可同时选中私钥和 .pub 公钥两个文件": "You can select the private key and its .pub file together",
  "公钥（可选）": "Public key (optional)",
  "ssh-ed25519 AAAA…  留空则从私钥导出": "ssh-ed25519 AAAA…  leave empty to derive it from the private key",
  "私钥口令": "Passphrase",
  "私钥未加密可留空": "Leave empty if the key is not encrypted",
  "保存为": "Save as",
  "文件名，例如 id_work": "File name, e.g. id_work",
  "校验": "Check",
  "校验中…": "Checking…",
  "导入": "Import",
  "已导入 {name}": "Imported {name}",
  "{name} 太大了，不像是密钥文件。": "{name} is too large to be a key file.",
};

// Blocks with inline markup, translated as a whole (keyed by data-i18n-html).
const EN_HTML = {
  "empty-tunnels": "Click \"New tunnel\" at the top right and pick a host from <code>~/.ssh/config</code>.",
  "empty-host": "Or click \"Add\" to create one. The file is backed up to <code>config.ssh2socks.bak</code> before each save.",
  "empty-keys-title": "No public keys found (<code>~/.ssh/*.pub</code>)",
  "empty-keys": "Click \"Generate key\" at the top right to create a new key pair, or \"Import key\" to use an existing private key.",
  "pick-host": "Choose a host from <code>~/.ssh/config</code>",
  "authorized-keys": "Append this line to <code>~/.ssh/authorized_keys</code> on the server to log in without a password.",
  "passphrase-help": "This app can't type a passphrase when it opens a tunnel, so add a protected key to ssh-agent with <code>ssh-add</code> first.",
};

// English text back to its Chinese source, for nodes code filled in while
// the UI was in English.
const ZH_OF = Object.fromEntries(Object.entries(EN).map(([zh, en]) => [en, zh]));

let uiLang = "zh";

function t(zh, vars) {
  let text = uiLang === "en" ? (EN[zh] ?? zh) : zh;
  if (vars) text = text.replace(/\{(\w+)\}/g, (m, k) => (k in vars ? String(vars[k]) : m));
  return text;
}

// node -> { zh, shown } for text nodes; element -> { attr: { zh, shown } }.
const textSource = new WeakMap();
const attrSource = new WeakMap();
const htmlSource = new WeakMap();
const ATTRS = ["placeholder", "title", "data-tpl"];
// User data and backend output: filled in by code in the current language.
const SKIP =
  "script, style, textarea, [data-i18n-html], #log, #list, #hosts, #host-blocks, #key-list, " +
  "#h-others, #host-test-out, #key-title, #key-meta, #config-path, #log-title, #imp-steps, #imp-summary, .error, #toast";

/// Translated form of `current`; `rec` remembers the Chinese source so the
/// node can be switched back. Values changed by code start a new record.
function localizeValue(current, rec) {
  if (!rec || rec.shown !== current) {
    const key = current.trim();
    if (!key) return null;
    const zh = EN[key] !== undefined ? key : ZH_OF[key];
    if (zh === undefined) return null;
    const start = current.indexOf(key);
    rec = { zh, pre: current.slice(0, start), post: current.slice(start + key.length) };
  }
  rec.shown = rec.pre + t(rec.zh) + rec.post;
  return rec;
}

function localizeDom(root = document.body) {
  for (const el of root.querySelectorAll("[data-i18n-html]")) {
    if (!htmlSource.has(el)) htmlSource.set(el, el.innerHTML);
    const en = EN_HTML[el.dataset.i18nHtml];
    el.innerHTML = uiLang === "en" && en ? en : htmlSource.get(el);
  }
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode: (node) =>
      node.parentElement.closest(SKIP)
        ? NodeFilter.FILTER_REJECT
        : NodeFilter.FILTER_ACCEPT,
  });
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    const rec = localizeValue(node.nodeValue, textSource.get(node));
    if (!rec) continue;
    textSource.set(node, rec);
    if (node.nodeValue !== rec.shown) node.nodeValue = rec.shown;
  }
  for (const el of root.querySelectorAll(ATTRS.map((a) => `[${a}]`).join(","))) {
    const recs = attrSource.get(el) ?? {};
    for (const attr of ATTRS) {
      if (!el.hasAttribute(attr)) continue;
      const rec = localizeValue(el.getAttribute(attr), recs[attr]);
      if (!rec) continue;
      recs[attr] = rec;
      el.setAttribute(attr, rec.shown);
    }
    attrSource.set(el, recs);
  }
}

function setUiLang(lang) {
  uiLang = lang === "en" ? "en" : "zh";
  document.documentElement.lang = uiLang === "en" ? "en" : "zh-CN";
  localizeDom();
}
