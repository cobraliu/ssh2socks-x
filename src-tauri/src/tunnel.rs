//! Tunnel lifecycle: one supervisor task per running tunnel.
//!
//! The supervisor spawns `ssh -N -T` with `-D`, `-L` or `-R`, waits
//! (asynchronously) until the forward is up, runs periodic end-to-end probes
//! while connected, and reconnects with exponential backoff when ssh exits.
//!
//! Readiness: for `-D`/`-L` ssh listens locally, so we poll that port. For
//! `-R` nothing is local; ssh runs with `-v` and we wait for its
//! "remote forward success" debug line (other debug output is dropped).

use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tokio::time::{sleep, timeout};

use crate::models::{ProbeResult, Tunnel, TunnelKind, TunnelState, TunnelView};
use crate::progress::{self, Progress};
use crate::{platform, probe, store};

const PORT_POLL: Duration = Duration::from_millis(300);
const PORT_CHECK_TIMEOUT: Duration = Duration::from_secs(1);
const PROBE_INTERVAL: Duration = Duration::from_secs(30);
const BACKOFF_BASE_MS: u64 = 1_000;
const BACKOFF_MAX_MS: u64 = 30_000;
const MAX_LOG_LINES: usize = 500;
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
/// While ssh has not come up yet, say so in the log this often…
const WAIT_NOTICE: Duration = Duration::from_secs(10);
/// …and give up (then retry) after this long. `ConnectTimeout` only covers
/// the TCP connect, not a hung ProxyCommand / jump host / handshake.
const CONNECT_DEADLINE: Duration = Duration::from_secs(45);
/// How long to wait for ssh's last stderr lines once it has exited.
const STDERR_DRAIN: Duration = Duration::from_secs(1);

pub const EVENT_CHANGED: &str = "tunnel-changed";
pub const EVENT_LOG: &str = "tunnel-log";

#[derive(Clone, Serialize)]
struct LogEvent<'a> {
    id: &'a str,
    line: &'a str,
}

#[derive(Default)]
struct Runtime {
    state: TunnelState,
    probe: Option<ProbeResult>,
    logs: VecDeque<String>,
    stop: Option<watch::Sender<bool>>,
    pid: Option<u32>,
    /// Remote forwards: the server address as this machine reaches it.
    public_host: Option<String>,
    /// Latest error / progress message, shown in the list.
    detail: Option<String>,
}

#[derive(Default)]
struct Inner {
    tunnels: Vec<Tunnel>,
    runtimes: HashMap<String, Runtime>,
}

pub struct Manager {
    app: Option<AppHandle>,
    ssh_program: String,
    inner: Mutex<Inner>,
}

enum Event {
    Ready,
    Tick,
    Probe,
    Exited(std::io::Result<std::process::ExitStatus>),
    Stop,
}

enum Outcome {
    /// Stopped on request.
    Stopped,
    /// ssh exited or could not bind; retry if auto-reconnect is on.
    Failed,
    /// Cannot work at all (ssh missing); do not retry.
    Fatal,
}

pub fn ssh_args(t: &Tunnel) -> Vec<String> {
    let mut args: Vec<String> = [
        "-N",
        "-T",
        // Verbose output tells how far a connection got (see `progress`);
        // it is parsed, not logged.
        "-v",
        "-o",
        "ExitOnForwardFailure=yes",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "BatchMode=yes",
        // Trust a server on first contact (instead of the interactive yes/no
        // prompt that BatchMode turns into a failure); a *changed* key is
        // still rejected.
        "-o",
        "StrictHostKeyChecking=accept-new",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let target = crate::probe::host_for_url(&t.target_host);
    match t.kind {
        TunnelKind::Socks => args.extend(["-D".into(), format!("127.0.0.1:{}", t.port)]),
        TunnelKind::Local => args.extend([
            "-L".into(),
            format!("127.0.0.1:{}:{}:{}", t.port, target, t.remote_port),
        ]),
        TunnelKind::Remote => args.extend([
            "-R".into(),
            format!("0.0.0.0:{}:127.0.0.1:{}", t.remote_port, t.port),
        ]),
    }
    args.push(t.host.clone());
    args
}

fn progress_of(p: &Mutex<Progress>) -> std::sync::MutexGuard<'_, Progress> {
    p.lock().unwrap_or_else(|e| e.into_inner())
}

/// Human-readable forward description, also used in logs.
pub fn describe(t: &Tunnel) -> String {
    match t.kind {
        TunnelKind::Socks => tr!(
            "SOCKS5 代理 127.0.0.1:{}",
            "SOCKS5 proxy 127.0.0.1:{}",
            t.port
        ),
        TunnelKind::Local => tr!(
            "127.0.0.1:{} → 服务器上的 {}:{}",
            "127.0.0.1:{} → {}:{} on the server",
            t.port,
            crate::probe::host_for_url(&t.target_host),
            t.remote_port
        ),
        TunnelKind::Remote => tr!(
            "服务器 0.0.0.0:{} → 本机 127.0.0.1:{}",
            "server 0.0.0.0:{} → local 127.0.0.1:{}",
            t.remote_port,
            t.port
        ),
    }
}

/// Extra advice for well-known ssh failures.
fn hint_for(line: &str) -> Option<String> {
    if line.contains("REMOTE HOST IDENTIFICATION HAS CHANGED") {
        Some(tr!("提示：服务器主机密钥与 known_hosts 中的记录不一致（服务器重装过，或存在中间人攻击）。确认安全后执行 ssh-keygen -R <主机地址> 删除旧记录再重试。", "Hint: the server's host key does not match the one in known_hosts (the server was reinstalled, or someone is intercepting). Once you are sure it is safe, run ssh-keygen -R <host> to remove the old entry and try again."))
    } else if line.contains("Permission denied (publickey") {
        Some(tr!("提示：需要配置密钥免密登录（例如 ssh-copy-id），本程序无法输入密码。", "Hint: set up key-based login (for example with ssh-copy-id); this app can't enter passwords."))
    } else if line.contains("remote port forwarding failed") {
        Some(tr!("提示：服务器上该端口已被占用，或 sshd 不允许端口转发（AllowTcpForwarding）。", "Hint: the port is already in use on the server, or sshd does not allow port forwarding (AllowTcpForwarding)."))
    } else {
        None
    }
}

/// Effective server address for an ssh_config alias (`ssh -G`), falling back
/// to the alias itself.
async fn resolve_hostname(ssh_program: &str, alias: &str) -> String {
    let mut cmd = Command::new(ssh_program);
    cmd.args(["-G", alias])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    platform::hide_console(&mut cmd);
    if let Ok(Ok(out)) = timeout(RESOLVE_TIMEOUT, cmd.output()).await {
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(host) = text
            .lines()
            .find_map(|l| l.strip_prefix("hostname "))
            .map(str::trim)
            .filter(|h| !h.is_empty())
        {
            return host.to_string();
        }
    }
    alias.to_string()
}

/// True if nothing listens on 127.0.0.1:<port>. std sets SO_REUSEADDR on Unix
/// (ignores TIME_WAIT, still fails on a live listener); on Windows a plain
/// bind fails on any existing listener.
pub fn port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

async fn port_open(port: u16) -> bool {
    // A refused connect takes ~2s on Windows; this runs off the UI thread and
    // is bounded by a timeout so the poll keeps its pace.
    matches!(
        timeout(PORT_CHECK_TIMEOUT, TcpStream::connect(("127.0.0.1", port))).await,
        Ok(Ok(_))
    )
}

fn backoff(attempts: u32) -> Duration {
    let ms = BACKOFF_BASE_MS.saturating_mul(1u64 << attempts.min(16));
    Duration::from_millis(ms.min(BACKOFF_MAX_MS))
}

async fn wait_stop(rx: &mut watch::Receiver<bool>) {
    // Also resolves if the sender is gone.
    let _ = rx.wait_for(|stop| *stop).await;
}

impl Manager {
    pub fn new(app: Option<AppHandle>, tunnels: Vec<Tunnel>) -> Arc<Self> {
        Self::with_program(app, tunnels, "ssh")
    }

    fn with_program(app: Option<AppHandle>, tunnels: Vec<Tunnel>, ssh: &str) -> Arc<Self> {
        let runtimes = tunnels
            .iter()
            .map(|t| (t.id.clone(), Runtime::default()))
            .collect();
        Arc::new(Self {
            app,
            ssh_program: ssh.to_string(),
            inner: Mutex::new(Inner { tunnels, runtimes }),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---- queries --------------------------------------------------------
    pub fn views(&self) -> Vec<TunnelView> {
        let inner = self.lock();
        inner.tunnels.iter().map(|t| view_of(&inner, t)).collect()
    }

    pub fn logs(&self, id: &str) -> Vec<String> {
        self.lock()
            .runtimes
            .get(id)
            .map(|r| r.logs.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn connected_summary(&self) -> (usize, usize) {
        let inner = self.lock();
        let connected = inner
            .runtimes
            .values()
            .filter(|r| r.state == TunnelState::Connected)
            .count();
        (connected, inner.tunnels.len())
    }

    /// First port from `start` that no tunnel listens on and is free now.
    pub fn suggest_port(&self, start: u16) -> u16 {
        let inner = self.lock();
        let mut port = start.max(1);
        while port < u16::MAX
            && (inner
                .tunnels
                .iter()
                .any(|t| t.kind.listens_locally() && t.port == port)
                || !port_free(port))
        {
            port += 1;
        }
        port
    }

    /// Browser address for a port forward.
    pub async fn browser_url(&self, id: &str) -> Result<String, String> {
        let tunnel = self
            .tunnel(id)
            .ok_or_else(|| tr!("隧道不存在", "Tunnel not found"))?;
        match tunnel.kind {
            TunnelKind::Socks => Err(tr!(
                "SOCKS 代理没有可打开的网页地址",
                "A SOCKS proxy has no web address to open"
            )),
            TunnelKind::Local => Ok(format!("http://127.0.0.1:{}/", tunnel.port)),
            TunnelKind::Remote => {
                let cached = self
                    .lock()
                    .runtimes
                    .get(id)
                    .and_then(|r| r.public_host.clone());
                let host = match cached {
                    Some(h) => h,
                    None => resolve_hostname(&self.ssh_program, &tunnel.host).await,
                };
                Ok(format!(
                    "http://{}:{}/",
                    crate::probe::host_for_url(&host),
                    tunnel.remote_port
                ))
            }
        }
    }

    fn tunnel(&self, id: &str) -> Option<Tunnel> {
        self.lock().tunnels.iter().find(|t| t.id == id).cloned()
    }

    // ---- editing --------------------------------------------------------
    pub fn upsert(&self, tunnel: Tunnel) -> Result<(), String> {
        let mut inner = self.lock();
        if let Some(err) = conflict(&inner.tunnels, &tunnel) {
            return Err(err);
        }
        match inner.tunnels.iter().position(|t| t.id == tunnel.id) {
            Some(i) => {
                if inner
                    .runtimes
                    .get(&tunnel.id)
                    .is_some_and(|r| r.state.active())
                {
                    return Err(tr!(
                        "编辑前请先停止该隧道。",
                        "Stop the tunnel before editing it."
                    ));
                }
                inner.tunnels[i] = tunnel;
            }
            None => {
                inner.runtimes.insert(tunnel.id.clone(), Runtime::default());
                inner.tunnels.push(tunnel);
            }
        }
        persist(&inner.tunnels)
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        let mut inner = self.lock();
        if let Some(rt) = inner.runtimes.remove(id) {
            if let Some(stop) = rt.stop {
                let _ = stop.send(true);
            }
        }
        inner.tunnels.retain(|t| t.id != id);
        persist(&inner.tunnels)
    }

    // ---- control --------------------------------------------------------
    pub fn start(self: &Arc<Self>, id: &str) {
        let rx = {
            let mut inner = self.lock();
            let Some(rt) = inner.runtimes.get_mut(id) else {
                return;
            };
            if rt.state.active() {
                return;
            }
            let (tx, rx) = watch::channel(false);
            rt.stop = Some(tx);
            rx
        };
        let me = Arc::clone(self);
        let id = id.to_string();
        tauri::async_runtime::spawn(async move { me.supervise(id, rx).await });
    }

    pub fn stop(&self, id: &str) {
        let had_task = {
            let mut inner = self.lock();
            let Some(rt) = inner.runtimes.get_mut(id) else {
                return;
            };
            match rt.stop.take() {
                Some(tx) => tx.send(true).is_ok(),
                None => false,
            }
        };
        if !had_task {
            self.set_state(id, TunnelState::Stopped);
        }
    }

    pub fn start_all(self: &Arc<Self>) {
        for id in self.ids() {
            self.start(&id);
        }
    }

    pub fn stop_all(&self) {
        for id in self.ids() {
            self.stop(&id);
        }
    }

    /// Synchronous last-resort cleanup when the app exits.
    pub fn kill_all_now(&self) {
        let mut inner = self.lock();
        for rt in inner.runtimes.values_mut() {
            if let Some(tx) = rt.stop.take() {
                let _ = tx.send(true);
            }
            if let Some(pid) = rt.pid.take() {
                platform::terminate_pid(pid);
            }
        }
    }

    fn ids(&self) -> Vec<String> {
        self.lock().tunnels.iter().map(|t| t.id.clone()).collect()
    }

    // ---- supervisor -----------------------------------------------------
    async fn supervise(self: Arc<Self>, id: String, mut stop: watch::Receiver<bool>) {
        let mut attempts = 0u32;
        loop {
            let Some(tunnel) = self.tunnel(&id) else {
                return;
            };
            self.set_state(&id, TunnelState::Connecting);
            let outcome = if tunnel.kind.listens_locally() && !port_free(tunnel.port) {
                self.note(
                    &id,
                    &tr!("本地端口 {} 已被占用（可能是残留的 ssh 进程或其他程序）", "Local port {} is already in use (maybe a leftover ssh process or another program)",
                        tunnel.port
                    ),
                );
                Outcome::Failed
            } else {
                self.run_once(&tunnel, &mut stop, &mut attempts).await
            };
            match outcome {
                Outcome::Stopped => break,
                Outcome::Fatal => {
                    self.finish(&id, TunnelState::Error);
                    return;
                }
                Outcome::Failed if !tunnel.auto_reconnect => {
                    self.finish(&id, TunnelState::Error);
                    return;
                }
                Outcome::Failed => {
                    let delay = backoff(attempts);
                    attempts += 1;
                    self.log(
                        &id,
                        &tr!("{}s 后自动重连…", "Reconnecting in {}s…", delay.as_secs()),
                    );
                    tokio::select! {
                        _ = sleep(delay) => {}
                        _ = wait_stop(&mut stop) => break,
                    }
                }
            }
        }
        self.finish(&id, TunnelState::Stopped);
    }

    async fn run_once(
        self: &Arc<Self>,
        tunnel: &Tunnel,
        stop: &mut watch::Receiver<bool>,
        attempts: &mut u32,
    ) -> Outcome {
        let id = tunnel.id.as_str();
        let target = resolve_hostname(&self.ssh_program, &tunnel.host).await;
        let progress = Arc::new(Mutex::new(Progress::new(&target)));
        if tunnel.kind == TunnelKind::Remote {
            if let Some(rt) = self.lock().runtimes.get_mut(id) {
                rt.public_host = Some(target);
            }
            if port_free(tunnel.port) {
                self.log(
                    id,
                    &tr!(
                        "注意：本机 127.0.0.1:{} 目前没有服务在监听",
                        "Note: nothing is listening on local 127.0.0.1:{} right now",
                        tunnel.port
                    ),
                );
            }
        }
        let args = ssh_args(tunnel);
        let mut cmd = Command::new(&self.ssh_program);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        platform::hide_console(&mut cmd);
        platform::prepare(&mut cmd);
        self.log(id, &format!("$ ssh {}", args.join(" ")));

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                self.note(
                    id,
                    &tr!(
                        "无法启动 ssh：请确认系统已安装 OpenSSH 客户端（{e}）",
                        "Could not start ssh. Make sure the OpenSSH client is installed ({e})"
                    ),
                );
                return Outcome::Fatal;
            }
        };
        // Dropped on every way out of this attempt, taking any ProxyJump /
        // ProxyCommand helpers down with ssh.
        let tree = platform::adopt(&child);
        self.set_pid(id, child.id());
        let (forwarded_tx, mut forwarded) = watch::channel(false);
        let mut stderr_done = None;
        if let Some(stderr) = child.stderr.take() {
            let me = Arc::clone(self);
            let id = id.to_string();
            let progress = Arc::clone(&progress);
            stderr_done = Some(tauri::async_runtime::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut buf = Vec::new();
                while matches!(reader.read_until(b'\n', &mut buf).await, Ok(n) if n > 0) {
                    let line = platform::decode(&buf);
                    let line = line.trim_end();
                    let version = progress_of(&progress).feed(line);
                    if let Some(version) = version {
                        me.log(&id, &version);
                    }
                    if line.starts_with("debug") {
                        if line.contains("remote forward success") {
                            let _ = forwarded_tx.send(true);
                        } else if line.contains("remote forward failure") {
                            me.note(&id, line.trim_start_matches("debug1: "));
                        }
                    } else if !line.trim().is_empty() && !progress::is_chatter(line) {
                        me.note(&id, line);
                        if let Some(hint) = hint_for(line) {
                            me.log(&id, &hint);
                        }
                    }
                    buf.clear();
                }
            }));
        }

        // Phase 1: wait for the forward to come up.
        let ready = async {
            if tunnel.kind.listens_locally() {
                loop {
                    sleep(PORT_POLL).await;
                    if port_open(tunnel.port).await {
                        break;
                    }
                }
            } else {
                let _ = forwarded.wait_for(|up| *up).await;
                // Sender dropped without success: ssh is exiting; let
                // child.wait() win the select.
                if !*forwarded.borrow() {
                    std::future::pending::<()>().await;
                }
            }
        };
        tokio::pin!(ready);
        let started = Instant::now();
        let event = loop {
            let event = tokio::select! {
                _ = &mut ready => Event::Ready,
                status = child.wait() => Event::Exited(status),
                _ = wait_stop(stop) => Event::Stop,
                _ = sleep(WAIT_NOTICE) => Event::Tick,
            };
            if !matches!(event, Event::Tick) {
                break event;
            }
            let waited = started.elapsed().as_secs();
            let (step, hint, last) = {
                let p = progress_of(&progress);
                (p.describe(), p.hint(), p.last_debug().to_string())
            };
            if started.elapsed() >= CONNECT_DEADLINE {
                self.note(
                    id,
                    &tr!(
                        "等待 {}s 仍未连上，卡在：{}。结束本次尝试",
                        "Still not connected after {}s, stuck at: {}. Giving up on this attempt",
                        waited,
                        step
                    ),
                );
                if let Some(hint) = hint {
                    self.log(id, &hint);
                }
                if !last.is_empty() {
                    self.log(id, &tr!("ssh 最后的输出：{}", "Last ssh output: {}", last));
                }
                let _ = self.kill(id, child, tree).await;
                return Outcome::Failed;
            }
            self.note(
                id,
                &tr!(
                    "仍在等待 ssh 建立连接…（已 {}s）当前：{}",
                    "Still waiting for ssh to connect… ({}s so far) now: {}",
                    waited,
                    step
                ),
            );
        };
        match event {
            Event::Exited(status) => {
                // Let the last lines (ssh's error) arrive first. A ProxyJump
                // helper may still hold the pipe open, hence the timeout.
                if let Some(done) = stderr_done {
                    let _ = timeout(STDERR_DRAIN, done).await;
                }
                // ssh's own error may be vague ("Connection reset"); say how
                // far it got.
                let (step, hint) = {
                    let p = progress_of(&progress);
                    (p.describe(), p.hint())
                };
                self.log(
                    id,
                    &tr!("断开前的进度：{}", "Progress before it ended: {}", step),
                );
                if let Some(hint) = hint {
                    self.log(id, &hint);
                }
                return self.exited(id, status);
            }
            Event::Stop => return self.kill(id, child, tree).await,
            Event::Ready | Event::Probe | Event::Tick => {}
        }

        *attempts = 0;
        self.set_state(id, TunnelState::Connected);
        self.log(id, &tr!("已就绪：{}", "Ready: {}", describe(tunnel)));

        // Phase 2: connected; probe periodically until ssh exits or we stop.
        let mut ticker = tokio::time::interval(PROBE_INTERVAL);
        loop {
            let event = tokio::select! {
                _ = ticker.tick() => Event::Probe,
                status = child.wait() => Event::Exited(status),
                _ = wait_stop(stop) => Event::Stop,
            };
            match event {
                Event::Probe | Event::Ready | Event::Tick => self.spawn_probe(tunnel),
                Event::Exited(status) => return self.exited(id, status),
                Event::Stop => return self.kill(id, child, tree).await,
            }
        }
    }

    fn exited(&self, id: &str, status: std::io::Result<std::process::ExitStatus>) -> Outcome {
        self.set_pid(id, None);
        let code = match status {
            Ok(s) => s
                .code()
                .map_or_else(|| "signal".to_string(), |c| c.to_string()),
            Err(e) => e.to_string(),
        };
        let line = tr!("ssh 进程退出 (code={code})", "ssh exited (code={code})");
        self.log(id, &line);
        let mut inner = self.lock();
        if let Some(rt) = inner.runtimes.get_mut(id) {
            rt.detail.get_or_insert(line);
        }
        Outcome::Failed
    }

    async fn kill(&self, id: &str, mut child: Child, tree: platform::ProcTree) -> Outcome {
        tree.kill();
        let _ = child.kill().await;
        self.set_pid(id, None);
        Outcome::Stopped
    }

    fn spawn_probe(self: &Arc<Self>, tunnel: &Tunnel) {
        let me = Arc::clone(self);
        let tunnel = tunnel.clone();
        tauri::async_runtime::spawn(async move {
            let id = tunnel.id.clone();
            let result = match tunnel.kind {
                TunnelKind::Socks => {
                    probe::run_probe(tunnel.port, &tunnel.probe_url, probe::PROBE_TIMEOUT).await
                }
                TunnelKind::Local => {
                    let target = format!(
                        "{}:{}",
                        probe::host_for_url(&tunnel.target_host),
                        tunnel.remote_port
                    );
                    probe::probe_local_forward(tunnel.port, &target).await
                }
                TunnelKind::Remote => {
                    let host = me
                        .lock()
                        .runtimes
                        .get(&id)
                        .and_then(|r| r.public_host.clone())
                        .unwrap_or_else(|| tunnel.host.clone());
                    probe::probe_remote_forward(tunnel.port, &host, tunnel.remote_port).await
                }
            };
            let changed = {
                let mut inner = me.lock();
                match inner.runtimes.get_mut(&id) {
                    Some(rt) if rt.state == TunnelState::Connected => {
                        let changed = rt.probe.as_ref().map(|p| p.ok) != Some(result.ok);
                        rt.probe = Some(result.clone());
                        changed
                    }
                    _ => return,
                }
            };
            if changed {
                let line = match (result.ok, result.latency_ms) {
                    (true, Some(ms)) => tr!("探测：通（{ms:.0}ms）", "Probe: up ({ms:.0}ms)"),
                    (true, None) => tr!("探测：通", "Probe: up"),
                    (false, _) => tr!("探测失败：{}", "Probe failed: {}", result.message),
                };
                me.log(&id, &line);
            }
            me.emit_changed(&id);
        });
    }

    // ---- state + events -------------------------------------------------
    fn finish(&self, id: &str, state: TunnelState) {
        if let Some(rt) = self.lock().runtimes.get_mut(id) {
            rt.stop = None;
            rt.pid = None;
        }
        self.set_state(id, state);
    }

    fn set_pid(&self, id: &str, pid: Option<u32>) {
        if let Some(rt) = self.lock().runtimes.get_mut(id) {
            rt.pid = pid;
        }
    }

    fn set_state(&self, id: &str, state: TunnelState) {
        {
            let mut inner = self.lock();
            let Some(rt) = inner.runtimes.get_mut(id) else {
                return;
            };
            if rt.state == state {
                return;
            }
            rt.state = state;
            if state != TunnelState::Connected {
                rt.probe = None;
            }
            if matches!(state, TunnelState::Connected | TunnelState::Stopped) {
                rt.detail = None;
            }
        }
        self.emit_changed(id);
    }

    fn emit_changed(&self, id: &str) {
        let view = {
            let inner = self.lock();
            inner
                .tunnels
                .iter()
                .find(|t| t.id == id)
                .map(|t| view_of(&inner, t))
        };
        if let (Some(app), Some(view)) = (&self.app, view) {
            let _ = app.emit(EVENT_CHANGED, view);
            crate::update_tray_tooltip(app, self);
        }
    }

    /// Log a line and also show it next to the tunnel in the list.
    fn note(&self, id: &str, line: &str) {
        self.log(id, line);
        {
            let mut inner = self.lock();
            let Some(rt) = inner.runtimes.get_mut(id) else {
                return;
            };
            rt.detail = Some(line.to_string());
        }
        self.emit_changed(id);
    }

    fn log(&self, id: &str, line: &str) {
        let line = format!("[{}] {line}", chrono::Local::now().format("%H:%M:%S"));
        let line = line.as_str();
        {
            let mut inner = self.lock();
            let Some(rt) = inner.runtimes.get_mut(id) else {
                return;
            };
            rt.logs.push_back(line.to_string());
            while rt.logs.len() > MAX_LOG_LINES {
                rt.logs.pop_front();
            }
        }
        if let Some(app) = &self.app {
            let _ = app.emit(EVENT_LOG, LogEvent { id, line });
        }
    }
}

/// Another tunnel already using the port this one needs.
fn conflict(tunnels: &[Tunnel], tunnel: &Tunnel) -> Option<String> {
    let mut others = tunnels.iter().filter(|t| t.id != tunnel.id);
    if tunnel.kind.listens_locally() {
        others
            .find(|t| t.kind.listens_locally() && t.port == tunnel.port)
            .map(|o| {
                tr!(
                    "本地端口 {} 已被隧道「{}」使用",
                    "Local port {} is already used by tunnel \"{}\"",
                    tunnel.port,
                    o.name
                )
            })
    } else {
        others
            .find(|t| {
                t.kind == TunnelKind::Remote
                    && t.host == tunnel.host
                    && t.remote_port == tunnel.remote_port
            })
            .map(|o| {
                tr!(
                    "服务器端口 {} 已被隧道「{}」使用",
                    "Server port {} is already used by tunnel \"{}\"",
                    tunnel.remote_port,
                    o.name
                )
            })
    }
}

fn view_of(inner: &Inner, t: &Tunnel) -> TunnelView {
    let rt = inner.runtimes.get(&t.id);
    TunnelView {
        tunnel: t.clone(),
        state: rt.map(|r| r.state).unwrap_or_default(),
        probe: rt.and_then(|r| r.probe.clone()),
        detail: rt.and_then(|r| r.detail.clone()),
    }
}

fn persist(tunnels: &[Tunnel]) -> Result<(), String> {
    store::save(tunnels).map_err(|e| tr!("保存配置失败：{e}", "Could not save settings: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff(0), Duration::from_secs(1));
        assert_eq!(backoff(3), Duration::from_secs(8));
        assert_eq!(backoff(10), Duration::from_secs(30));
        assert_eq!(backoff(100), Duration::from_secs(30));
    }

    #[test]
    fn detects_busy_port() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!port_free(port));
        drop(listener);
        // Other tests running in parallel may briefly get the same port for
        // a connection of their own.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !port_free(port) {
            assert!(Instant::now() < deadline, "port {port} never freed");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn args_end_with_forward_and_host() {
        let t = Tunnel {
            name: "n".into(),
            host: "myhost".into(),
            kind: crate::models::TunnelKind::Socks,
            remote_port: 0,
            target_host: "127.0.0.1".into(),
            port: 1081,
            probe_url: String::new(),
            auto_reconnect: true,
            id: "x".into(),
        };
        let args = ssh_args(&t);
        assert_eq!(&args[args.len() - 3..], ["-D", "127.0.0.1:1081", "myhost"]);
        assert!(args.contains(&"BatchMode=yes".to_string()));
        assert!(args.contains(&"StrictHostKeyChecking=accept-new".to_string()));

        let local = Tunnel {
            kind: TunnelKind::Local,
            remote_port: 80,
            target_host: "192.168.1.10".into(),
            ..t.clone()
        };
        let args = ssh_args(&local);
        assert_eq!(
            &args[args.len() - 3..],
            ["-L", "127.0.0.1:1081:192.168.1.10:80", "myhost"]
        );

        let remote = Tunnel {
            kind: TunnelKind::Remote,
            port: 3000,
            remote_port: 8080,
            ..t
        };
        let args = ssh_args(&remote);
        assert_eq!(
            &args[args.len() - 3..],
            ["-R", "0.0.0.0:8080:127.0.0.1:3000", "myhost"]
        );
        assert!(args.contains(&"-v".to_string()));
    }

    #[test]
    fn port_conflicts_depend_on_kind() {
        let base = Tunnel {
            name: "a".into(),
            host: "h".into(),
            kind: TunnelKind::Socks,
            port: 45_001,
            remote_port: 0,
            target_host: "127.0.0.1".into(),
            probe_url: String::new(),
            auto_reconnect: true,
            id: "a".into(),
        };
        let existing = [base.clone()];

        // A local forward on the same local port conflicts…
        let clash = Tunnel {
            kind: TunnelKind::Local,
            id: "b".into(),
            ..base.clone()
        };
        assert!(conflict(&existing, &clash).unwrap().contains("本地端口"));
        // …a remote forward only uses the local port as a target.
        let remote = Tunnel {
            kind: TunnelKind::Remote,
            remote_port: 8080,
            id: "c".into(),
            ..base.clone()
        };
        assert!(conflict(&existing, &remote).is_none());
        // Two remote forwards to the same server port conflict.
        let existing = [remote.clone()];
        let again = Tunnel {
            id: "d".into(),
            port: 4000,
            ..remote
        };
        assert!(conflict(&existing, &again).unwrap().contains("服务器端口"));
    }

    /// End-to-end supervisor test with a fake `ssh` that opens the SOCKS
    /// port after a short "authentication" delay.
    #[cfg(unix)]
    #[test]
    fn supervisor_lifecycle() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::Instant;

        let dir = std::env::temp_dir().join(format!("ssh2socks-e2e-{}", crate::models::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("ssh");
        std::fs::write(
            &fake,
            "#!/usr/bin/env python3\n\
             import socket, sys, time\n\
             if '-G' in sys.argv:\n    print('user x'); print('hostname 127.0.0.1'); sys.exit(0)\n\
             if '-R' in sys.argv:\n\
             \x20   print('debug1: Connecting', file=sys.stderr, flush=True)\n\
             \x20   time.sleep(0.3)\n\
             \x20   print('debug1: remote forward success for: listen 0.0.0.0:1, connect 127.0.0.1:2', file=sys.stderr, flush=True)\n\
             \x20   time.sleep(3600)\n\
             import os, subprocess\n\
             helper = subprocess.Popen(['sleep', '3600'])\n\
             with open(os.path.join(os.path.dirname(sys.argv[0]), 'helpers'), 'a') as f: f.write(f'{helper.pid}\\n')\n\
             port = int(sys.argv[sys.argv.index('-D') + 1].split(':')[1])\n\
             print('fake ssh authenticating', file=sys.stderr, flush=True)\n\
             time.sleep(0.4)\n\
             s = socket.socket()\n\
             s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)\n\
             s.bind(('127.0.0.1', port))\n\
             s.listen(8)\n\
             while True:\n    s.accept()[0].close()\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let tunnel = Tunnel {
            name: "e2e".into(),
            host: "fake".into(),
            kind: crate::models::TunnelKind::Socks,
            remote_port: 0,
            target_host: "127.0.0.1".into(),
            port,
            probe_url: "http://127.0.0.1/".into(),
            auto_reconnect: true,
            id: "e2e".into(),
        };
        let mgr = Manager::with_program(None, vec![tunnel], fake.to_str().unwrap());
        let state = || mgr.lock().runtimes["e2e"].state;
        let pid = || mgr.lock().runtimes["e2e"].pid;
        let logs = || mgr.logs("e2e").join("\n");
        let wait = |want: TunnelState, secs: u64| {
            let deadline = Instant::now() + Duration::from_secs(secs);
            while state() != want {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for {want:?}:\n{}",
                    logs()
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        };

        // Every running ssh is recorded for a later launch's cleanup.
        crate::reaper::init(&dir);
        let run_file = dir.join("run").join(format!("{}.json", std::process::id()));
        let recorded = |pid: u32| {
            std::fs::read_to_string(&run_file)
                .unwrap()
                .contains(&format!("\"pid\":{pid},"))
        };

        // Start -> connected; stderr is captured.
        mgr.start("e2e");
        wait(TunnelState::Connected, 10);
        assert!(recorded(pid().unwrap()));
        assert!(logs().contains("fake ssh authenticating"));
        assert!(logs().contains("已就绪：SOCKS5 代理"));

        // ssh dies -> auto reconnect.
        let first = pid().expect("pid while connected");
        crate::platform::terminate_pid(first);
        wait(TunnelState::Connecting, 5);
        wait(TunnelState::Connected, 10);
        assert!(logs().contains("自动重连"));
        assert_ne!(pid(), Some(first));
        assert!(!recorded(first) && recorded(pid().unwrap()));
        let second = pid().unwrap();

        // Stop, then restart right away: the new process must survive.
        mgr.stop("e2e");
        wait(TunnelState::Stopped, 5);
        mgr.start("e2e");
        wait(TunnelState::Connected, 10);
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(state(), TunnelState::Connected);
        let third = pid().unwrap();
        mgr.stop("e2e");
        wait(TunnelState::Stopped, 5);
        assert!(!recorded(second) && !recorded(third));

        // Helpers ssh started (like a ProxyJump `ssh -W`) die with it.
        let helpers = std::fs::read_to_string(dir.join("helpers")).unwrap();
        let helpers: Vec<i32> = helpers.lines().map(|l| l.parse().unwrap()).collect();
        assert!(helpers.len() >= 3, "{helpers:?}");
        let alive = |pid: i32| {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            let exists = unsafe { libc::kill(pid, 0) } == 0;
            exists && !stat.contains(") Z ")
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while helpers.iter().any(|&p| alive(p)) {
            assert!(
                Instant::now() < deadline,
                "helpers left running: {helpers:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        // Port taken by someone else -> reported, not mistaken for ssh.
        let blocker = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        mgr.start("e2e");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !logs().contains("已被占用") {
            assert!(
                Instant::now() < deadline,
                "no busy-port message:\n{}",
                logs()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(state(), TunnelState::Connecting);
        mgr.stop("e2e");
        wait(TunnelState::Stopped, 5);
        drop(blocker);

        // Remote forward: ready once ssh reports success; debug lines are
        // not logged, the resolved server address is used for the URL.
        let remote = Tunnel {
            name: "r".into(),
            host: "fake".into(),
            kind: TunnelKind::Remote,
            port,
            remote_port: 18080,
            target_host: "127.0.0.1".into(),
            probe_url: String::new(),
            auto_reconnect: false,
            id: "r".into(),
        };
        let mgr = Manager::with_program(None, vec![remote], fake.to_str().unwrap());
        mgr.start("r");
        let deadline = Instant::now() + Duration::from_secs(10);
        while mgr.lock().runtimes["r"].state != TunnelState::Connected {
            assert!(Instant::now() < deadline, "{}", mgr.logs("r").join("\n"));
            std::thread::sleep(Duration::from_millis(50));
        }
        let rlogs = mgr.logs("r").join("\n");
        assert!(rlogs.contains("已就绪：服务器 0.0.0.0:18080"), "{rlogs}");
        assert!(!rlogs.contains("debug1"), "{rlogs}");
        let url = tauri::async_runtime::block_on(mgr.browser_url("r")).unwrap();
        assert_eq!(url, "http://127.0.0.1:18080/");
        mgr.stop("r");

        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A connection that dies mid-way says how far it got, and ssh's `-v`
    /// output itself stays out of the log.
    #[cfg(unix)]
    #[test]
    fn reports_progress_when_ssh_gives_up() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::Instant;

        let dir = std::env::temp_dir().join(format!("ssh2socks-prog-{}", crate::models::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("ssh");
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             [ \"$1\" = -G ] && { echo 'hostname 10.76.0.73'; exit 0; }\n\
             cat >&2 <<'EOF'\n\
             OpenSSH_for_Windows_9.5p1, LibreSSL 3.8.2\n\
             debug1: Executing proxy command: exec ssh -v -W '[10.76.0.73]:22' gate\n\
             debug1: Connecting to 121.43.96.239 [121.43.96.239] port 22.\n\
             debug1: Connection established.\n\
             debug1: Authenticating to 121.43.96.239:22 as 'root'\n\
             debug1: expecting SSH2_MSG_KEX_ECDH_REPLY\n\
             client_loop: send disconnect: Connection reset\n\
             EOF\n\
             exit 255\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let tunnel = Tunnel {
            name: "p".into(),
            host: "flabproxy".into(),
            kind: crate::models::TunnelKind::Socks,
            remote_port: 0,
            target_host: "127.0.0.1".into(),
            port: std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port(),
            probe_url: String::new(),
            auto_reconnect: false,
            id: "p".into(),
        };
        let mgr = Manager::with_program(None, vec![tunnel], fake.to_str().unwrap());
        mgr.start("p");
        let logs = || mgr.logs("p").join("\n");
        let deadline = Instant::now() + Duration::from_secs(10);
        // The exit line comes last in that path.
        while !logs().contains("ssh 进程退出") {
            assert!(Instant::now() < deadline, "no progress line:\n{}", logs());
            std::thread::sleep(Duration::from_millis(50));
        }
        let logs = logs();
        assert!(
            logs.contains("断开前的进度：已向跳板机 121.43.96.239:22 发出密钥交换请求"),
            "{logs}"
        );
        assert!(logs.contains("OpenSSH_for_Windows_9.5p1"), "{logs}");
        assert!(logs.contains("Connection reset"), "{logs}");
        assert!(logs.contains("MTU"), "{logs}");
        assert!(!logs.contains("debug1"), "{logs}");
        mgr.stop("p");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
