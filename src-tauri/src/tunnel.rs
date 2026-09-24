//! Tunnel lifecycle: one supervisor task per running tunnel.
//!
//! The supervisor spawns `ssh -N -T -D`, waits (asynchronously) for the local
//! SOCKS port to come up, runs periodic end-to-end probes while connected, and
//! reconnects with exponential backoff when ssh exits.

use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tokio::time::{sleep, timeout};

use crate::models::{ProbeResult, Tunnel, TunnelState, TunnelView};
use crate::{platform, probe, store};

const PORT_POLL: Duration = Duration::from_millis(300);
const PORT_CHECK_TIMEOUT: Duration = Duration::from_secs(1);
const PROBE_INTERVAL: Duration = Duration::from_secs(30);
const BACKOFF_BASE_MS: u64 = 1_000;
const BACKOFF_MAX_MS: u64 = 30_000;
const MAX_LOG_LINES: usize = 500;

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
    [
        "-N",
        "-T",
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
        "-D",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain([format!("127.0.0.1:{}", t.port), t.host.clone()])
    .collect()
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

    pub fn suggest_port(&self) -> u16 {
        let inner = self.lock();
        let mut port = 1080;
        while inner.tunnels.iter().any(|t| t.port == port) {
            port += 1;
        }
        port
    }

    fn tunnel(&self, id: &str) -> Option<Tunnel> {
        self.lock().tunnels.iter().find(|t| t.id == id).cloned()
    }

    // ---- editing --------------------------------------------------------
    pub fn upsert(&self, tunnel: Tunnel) -> Result<(), String> {
        let mut inner = self.lock();
        if let Some(other) = inner
            .tunnels
            .iter()
            .find(|t| t.port == tunnel.port && t.id != tunnel.id)
        {
            return Err(format!(
                "端口 {} 已被隧道「{}」使用",
                tunnel.port, other.name
            ));
        }
        match inner.tunnels.iter().position(|t| t.id == tunnel.id) {
            Some(i) => {
                if inner
                    .runtimes
                    .get(&tunnel.id)
                    .is_some_and(|r| r.state.active())
                {
                    return Err("编辑前请先停止该隧道。".into());
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
            let outcome = if !port_free(tunnel.port) {
                self.log(
                    &id,
                    &format!(
                        "本地端口 {} 已被占用（可能是残留的 ssh 进程或其他程序）",
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
                    self.log(&id, &format!("{}s 后自动重连…", delay.as_secs()));
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
                self.log(
                    id,
                    &format!("无法启动 ssh：请确认系统已安装 OpenSSH 客户端（{e}）"),
                );
                return Outcome::Fatal;
            }
        };
        platform::adopt(&child);
        self.set_pid(id, child.id());
        if let Some(stderr) = child.stderr.take() {
            let me = Arc::clone(self);
            let id = id.to_string();
            tauri::async_runtime::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut buf = Vec::new();
                while matches!(reader.read_until(b'\n', &mut buf).await, Ok(n) if n > 0) {
                    let line = platform::decode(&buf);
                    let line = line.trim_end();
                    if !line.trim().is_empty() {
                        me.log(&id, line);
                    }
                    buf.clear();
                }
            });
        }

        // Phase 1: wait for the SOCKS port.
        let ready = async {
            loop {
                sleep(PORT_POLL).await;
                if port_open(tunnel.port).await {
                    break;
                }
            }
        };
        let event = tokio::select! {
            _ = ready => Event::Ready,
            status = child.wait() => Event::Exited(status),
            _ = wait_stop(stop) => Event::Stop,
        };
        match event {
            Event::Exited(status) => return self.exited(id, status),
            Event::Stop => return self.kill(id, child).await,
            Event::Ready | Event::Probe => {}
        }

        *attempts = 0;
        self.set_state(id, TunnelState::Connected);
        self.log(
            id,
            &format!("SOCKS5 代理已就绪于 127.0.0.1:{}", tunnel.port),
        );

        // Phase 2: connected; probe periodically until ssh exits or we stop.
        let mut ticker = tokio::time::interval(PROBE_INTERVAL);
        loop {
            let event = tokio::select! {
                _ = ticker.tick() => Event::Probe,
                status = child.wait() => Event::Exited(status),
                _ = wait_stop(stop) => Event::Stop,
            };
            match event {
                Event::Probe | Event::Ready => self.spawn_probe(tunnel),
                Event::Exited(status) => return self.exited(id, status),
                Event::Stop => return self.kill(id, child).await,
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
        self.log(id, &format!("ssh 进程退出 (code={code})"));
        Outcome::Failed
    }

    async fn kill(&self, id: &str, mut child: Child) -> Outcome {
        let _ = child.kill().await;
        self.set_pid(id, None);
        Outcome::Stopped
    }

    fn spawn_probe(self: &Arc<Self>, tunnel: &Tunnel) {
        let me = Arc::clone(self);
        let (id, port, url) = (tunnel.id.clone(), tunnel.port, tunnel.probe_url.clone());
        tauri::async_runtime::spawn(async move {
            let result = probe::run_probe(port, &url, probe::PROBE_TIMEOUT).await;
            {
                let mut inner = me.lock();
                match inner.runtimes.get_mut(&id) {
                    Some(rt) if rt.state == TunnelState::Connected => rt.probe = Some(result),
                    _ => return,
                }
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

    fn log(&self, id: &str, line: &str) {
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

fn view_of(inner: &Inner, t: &Tunnel) -> TunnelView {
    let rt = inner.runtimes.get(&t.id);
    TunnelView {
        tunnel: t.clone(),
        state: rt.map(|r| r.state).unwrap_or_default(),
        probe: rt.and_then(|r| r.probe.clone()),
    }
}

fn persist(tunnels: &[Tunnel]) -> Result<(), String> {
    store::save(tunnels).map_err(|e| format!("保存配置失败：{e}"))
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
        assert!(port_free(port));
    }

    #[test]
    fn args_end_with_forward_and_host() {
        let t = Tunnel {
            name: "n".into(),
            host: "myhost".into(),
            port: 1081,
            probe_url: String::new(),
            auto_reconnect: true,
            id: "x".into(),
        };
        let args = ssh_args(&t);
        assert_eq!(&args[args.len() - 3..], ["-D", "127.0.0.1:1081", "myhost"]);
        assert!(args.contains(&"BatchMode=yes".to_string()));
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

        // Start -> connected; stderr is captured.
        mgr.start("e2e");
        wait(TunnelState::Connected, 10);
        assert!(logs().contains("fake ssh authenticating"));
        assert!(logs().contains("SOCKS5 代理已就绪"));

        // ssh dies -> auto reconnect.
        let first = pid().expect("pid while connected");
        crate::platform::terminate_pid(first);
        wait(TunnelState::Connecting, 5);
        wait(TunnelState::Connected, 10);
        assert!(logs().contains("自动重连"));
        assert_ne!(pid(), Some(first));

        // Stop, then restart right away: the new process must survive.
        mgr.stop("e2e");
        wait(TunnelState::Stopped, 5);
        mgr.start("e2e");
        wait(TunnelState::Connected, 10);
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(state(), TunnelState::Connected);
        mgr.stop("e2e");
        wait(TunnelState::Stopped, 5);

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

        std::fs::remove_dir_all(dir).unwrap();
    }
}
