//! Where an ssh connection attempt is, read from its `-v` output, so a
//! connection that hangs can say which step it is stuck at (for example
//! "waiting for the jump host's key exchange reply") instead of just "still
//! waiting".
//!
//! With ProxyJump the helper `ssh -W` inherits `-v` and shares our stderr,
//! so its lines arrive too. The two sessions run one after the other (ssh
//! only hears from the target once the helper is through), so the latest
//! milestone line is always the current step; hosts other than the target
//! are labelled as jump hosts.

/// One step of the connection, in the order ssh goes through them.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    /// ssh started but has not connected anywhere yet.
    Start,
    /// A ProxyCommand was started; nothing heard back yet.
    Proxy,
    /// TCP connect to host:port.
    Tcp(String),
    /// TCP is up, waiting for the server's SSH version line.
    Banner(String),
    /// Key exchange with the host.
    Kex(String),
    /// Key exchange request sent, waiting for the server's reply.
    KexReply(String),
    /// Keys agreed, authenticating.
    Auth(String),
    /// Logged in to the host.
    LoggedIn(String),
    /// Logged in to the target, setting up the forward.
    Forwarding,
}

pub struct Progress {
    /// The target's HostName (from `ssh -G`), to tell it from jump hosts.
    target: String,
    via_proxy: bool,
    host: String,
    step: Step,
    last_debug: String,
    version_seen: bool,
}

/// What ssh prints at `-v` level besides `debugN:` lines. Only progress, not
/// worth a log line (the version is logged once, see [`Progress::feed`]).
/// "Killed by signal" comes from the ProxyJump helper when ssh ends it.
pub fn is_chatter(line: &str) -> bool {
    [
        "OpenSSH",
        "Authenticated to ",
        "Transferred: ",
        "Bytes per second",
        "Killed by signal",
    ]
    .iter()
    .any(|p| line.starts_with(p))
}

impl Progress {
    pub fn new(target: &str) -> Self {
        Progress {
            target: bare_host(target),
            via_proxy: false,
            host: String::new(),
            step: Step::Start,
            last_debug: String::new(),
            version_seen: false,
        }
    }

    /// Take one line of ssh's stderr. Returns the ssh version the first time
    /// it is printed, for the log.
    pub fn feed(&mut self, line: &str) -> Option<String> {
        if let Some(msg) = line
            .strip_prefix("debug")
            .and_then(|l| l.split_once(": "))
            .map(|(_, m)| m)
        {
            self.last_debug = msg.to_string();
            self.debug(msg);
            return None;
        }
        if line.starts_with("Authenticated to ") || line.starts_with("Authentication succeeded") {
            self.step = Step::LoggedIn(self.host.clone());
        } else if line.starts_with("OpenSSH") && !self.version_seen {
            self.version_seen = true;
            return Some(line.to_string());
        }
        None
    }

    fn debug(&mut self, msg: &str) {
        if msg.starts_with("Executing proxy command:") {
            self.via_proxy = true;
            self.step = Step::Proxy;
        } else if let Some(rest) = msg.strip_prefix("Connecting to ") {
            // "host [addr] port 22."
            let name = rest.split_whitespace().next().unwrap_or_default();
            let port = rest
                .rsplit_once(" port ")
                .map_or("", |(_, p)| p.trim_end_matches('.'));
            self.host = if port.is_empty() {
                name.to_string()
            } else {
                format!("{name}:{port}")
            };
            self.step = Step::Tcp(self.host.clone());
        } else if msg.starts_with("Connection established") {
            self.step = Step::Banner(self.host.clone());
        } else if let Some(rest) = msg.strip_prefix("Authenticating to ") {
            // "host:22 as 'user'"
            self.host = rest.split(" as ").next().unwrap_or(rest).to_string();
            self.step = Step::Kex(self.host.clone());
        } else if msg.starts_with("expecting SSH2_MSG_KEX") {
            self.step = Step::KexReply(self.host.clone());
        } else if msg.starts_with("SSH2_MSG_NEWKEYS received") {
            self.step = Step::Auth(self.host.clone());
        } else if msg.starts_with("Local forwarding listening on")
            || msg.starts_with("Local connections to")
            || msg.starts_with("Remote connections from")
        {
            self.step = Step::Forwarding;
        }
    }

    fn is_jump(&self, host: &str) -> bool {
        self.via_proxy && !self.target.is_empty() && bare_host(host) != self.target
    }

    /// "jump host 1.2.3.4:22" / "server 1.2.3.4:22"
    fn name(&self, host: &str) -> String {
        if self.is_jump(host) {
            tr!("跳板机 {}", "jump host {}", host)
        } else {
            tr!("服务器 {}", "server {}", host)
        }
    }

    /// The current step, for "stuck at …" / "progress: …" messages.
    pub fn describe(&self) -> String {
        match &self.step {
            Step::Start => tr!(
                "ssh 尚未开始连接（在读取配置或解析主机名）",
                "ssh has not started connecting (reading its config or resolving the host name)"
            ),
            Step::Proxy => tr!(
                "代理命令已启动，还没有收到服务器的回应",
                "the proxy command is running, but nothing has come back from the server"
            ),
            Step::Tcp(h) => tr!(
                "正在连接{}，对方没有响应",
                "connecting to {}, no answer",
                self.name(h)
            ),
            Step::Banner(h) => tr!(
                "已连上{} 的端口，但对方没有发来 SSH 版本信息",
                "connected to the port of {}, but it has not sent its SSH version",
                self.name(h)
            ),
            Step::Kex(h) => tr!("正在与{} 交换密钥", "exchanging keys with {}", self.name(h)),
            Step::KexReply(h) => tr!(
                "已向{} 发出密钥交换请求，一直没有收到回应",
                "sent the key exchange request to {}, no reply",
                self.name(h)
            ),
            Step::Auth(h) => tr!("正在向{} 验证身份", "logging in to {}", self.name(h)),
            Step::LoggedIn(h) if self.is_jump(h) => tr!(
                "已登录{}，正在通过它连接目标服务器",
                "logged in to {}, connecting to the target server through it",
                self.name(h)
            ),
            Step::LoggedIn(h) => tr!(
                "已登录{}，正在建立端口转发",
                "logged in to {}, setting up the forward",
                self.name(h)
            ),
            Step::Forwarding => tr!(
                "已登录，正在等待端口转发就绪",
                "logged in, waiting for the forward to be ready"
            ),
        }
    }

    /// Likely causes for being stuck at the current step, if there is
    /// something useful to say.
    pub fn hint(&self) -> Option<String> {
        Some(match &self.step {
            Step::Proxy => tr!(
                "可能原因：ProxyCommand 本身卡住，或它连不到目标地址。",
                "Possible causes: the ProxyCommand itself is stuck, or it cannot reach the target."
            ),
            Step::Tcp(_) => tr!(
                "可能原因：地址或端口不对、网络不通，或被防火墙/安全组拦截。",
                "Possible causes: wrong address or port, no network route, or a firewall / security group blocking it."
            ),
            Step::Banner(_) => tr!(
                "可能原因：该端口上不是 sshd，或 sshd 负载过高、连接被中间设备拦截。",
                "Possible causes: something other than sshd on that port, sshd overloaded, or a device in between holding the connection."
            ),
            Step::KexReply(_) => tr!(
                "可能原因：网络路径上的设备丢弃了较大的数据包（MTU 问题），或经跳板机转发的链路卡住。可以稍后重试，或换个网络确认。",
                "Possible causes: a device on the network path drops larger packets (an MTU problem), or the link through the jump host is stuck. Retry later, or try another network to confirm."
            ),
            Step::LoggedIn(h) if self.is_jump(h) => tr!(
                "可能原因：跳板机连不到目标服务器的地址或端口。",
                "Possible causes: the jump host cannot reach the target server's address or port."
            ),
            _ => return None,
        })
    }

    /// The last `-v` line ssh printed, for the log when giving up.
    pub fn last_debug(&self) -> &str {
        &self.last_debug
    }
}

/// "host:22" / "[::1]:22" / "HOST" -> "host" (lowercase, no port).
fn bare_host(host: &str) -> String {
    let h = host.trim();
    let h = match h.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(rest),
        None => match h.rsplit_once(':') {
            // One colon: host:port. More: a bare IPv6 address.
            Some((name, port)) if !name.contains(':') && port.parse::<u16>().is_ok() => name,
            _ => h,
        },
    };
    h.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(target: &str, transcript: &str) -> Progress {
        let mut p = Progress::new(target);
        for line in transcript.lines() {
            p.feed(line);
        }
        p
    }

    // Trimmed from real OpenSSH 8.9 `-v` output.
    const JUMP: &str = "\
OpenSSH_8.9p1 Ubuntu-3ubuntu0.13, OpenSSL 3.0.2 15 Mar 2022
debug1: Setting implicit ProxyCommand from ProxyJump: ssh -v -W '[%h]:%p' gate
debug1: Executing proxy command: exec ssh -v -W '[10.76.0.73]:22' gate
debug1: Local version string SSH-2.0-OpenSSH_8.9p1 Ubuntu-3ubuntu0.13
OpenSSH_8.9p1 Ubuntu-3ubuntu0.13, OpenSSL 3.0.2 15 Mar 2022
debug1: Connecting to 121.43.96.239 [121.43.96.239] port 22.
debug1: Connection established.
debug1: Remote protocol version 2.0, remote software version OpenSSH_8.9p1
debug1: Authenticating to 121.43.96.239:22 as 'root'
debug1: SSH2_MSG_KEXINIT sent
debug1: SSH2_MSG_KEXINIT received
debug1: expecting SSH2_MSG_KEX_ECDH_REPLY
debug1: SSH2_MSG_NEWKEYS received
debug1: Authentications that can continue: publickey
Authenticated to 121.43.96.239 ([121.43.96.239]:22) using \"publickey\".
debug1: channel_connect_stdio_fwd: 10.76.0.73:22
debug1: Requesting no-more-sessions@openssh.com
debug1: Remote protocol version 2.0, remote software version OpenSSH_9.6
debug1: Authenticating to 10.76.0.73:22 as 'root'
debug1: SSH2_MSG_KEXINIT sent
debug1: SSH2_MSG_KEXINIT received
debug1: expecting SSH2_MSG_KEX_ECDH_REPLY
debug1: SSH2_MSG_NEWKEYS received
Authenticated to 10.76.0.73 (via proxy) using \"publickey\".
debug1: Local connections to 127.0.0.1:18086 forwarded to remote address socks:0
debug1: Local forwarding listening on 127.0.0.1 port 18086.
";

    /// Progress after the first `n` lines of `JUMP`.
    fn jump_after(n: usize) -> Progress {
        run(
            "10.76.0.73",
            &JUMP.lines().take(n).collect::<Vec<_>>().join("\n"),
        )
    }

    #[test]
    fn follows_a_jump_host_connection() {
        let at = |n| jump_after(n).step;
        let gate = "121.43.96.239:22".to_string();
        let target = "10.76.0.73:22".to_string();
        assert_eq!(at(1), Step::Start);
        assert_eq!(at(4), Step::Proxy);
        assert_eq!(at(6), Step::Tcp(gate.clone()));
        assert_eq!(at(8), Step::Banner(gate.clone()));
        assert_eq!(at(11), Step::Kex(gate.clone()));
        assert_eq!(at(12), Step::KexReply(gate.clone()));
        assert_eq!(at(14), Step::Auth(gate.clone()));
        // The helper's no-more-sessions is not the target's forward.
        assert_eq!(at(17), Step::LoggedIn(gate.clone()));
        assert_eq!(at(22), Step::KexReply(target.clone()));
        assert_eq!(at(24), Step::LoggedIn(target));
        assert_eq!(at(26), Step::Forwarding);
    }

    #[test]
    fn names_jump_host_and_target() {
        let p = jump_after(12);
        assert_eq!(
            p.describe(),
            "已向跳板机 121.43.96.239:22 发出密钥交换请求，一直没有收到回应"
        );
        assert!(p.hint().unwrap().contains("MTU"));
        assert_eq!(p.last_debug(), "expecting SSH2_MSG_KEX_ECDH_REPLY");
        let p = jump_after(17);
        assert!(p.describe().contains("正在通过它连接目标服务器"));
        assert!(p.hint().unwrap().contains("跳板机连不到目标"));
        let p = jump_after(22);
        assert!(
            p.describe().contains("服务器 10.76.0.73:22"),
            "{}",
            p.describe()
        );
    }

    #[test]
    fn direct_connection_has_no_jump_host() {
        let p = run(
            "10.0.0.5",
            "debug1: Connecting to 10.0.0.5 [10.0.0.5] port 2222.\n\
             debug1: Connection established.\n",
        );
        assert_eq!(p.step, Step::Banner("10.0.0.5:2222".into()));
        assert!(!p.is_jump("10.0.0.5:2222"));
        // Without a proxy nothing is a jump host, even under another name.
        assert!(!p.is_jump("other:22"));
    }

    #[test]
    fn logs_the_version_once() {
        let mut p = Progress::new("h");
        assert!(p
            .feed("OpenSSH_for_Windows_9.5p1, LibreSSL 3.8.2")
            .is_some());
        assert!(p
            .feed("OpenSSH_for_Windows_9.5p1, LibreSSL 3.8.2")
            .is_none());
        assert!(is_chatter("OpenSSH_for_Windows_9.5p1, LibreSSL 3.8.2"));
        assert!(is_chatter(
            "Transferred: sent 2176, received 2264 bytes, in 2.9 seconds"
        ));
        assert!(is_chatter("Killed by signal 1."));
        assert!(!is_chatter(
            "ssh: connect to host 10.255.255.1 port 22: Connection timed out"
        ));
    }

    #[test]
    fn bare_hosts() {
        assert_eq!(bare_host("Host.Example:22"), "host.example");
        assert_eq!(bare_host("[::1]:22"), "::1");
        assert_eq!(bare_host("fe80::1"), "fe80::1");
        assert_eq!(bare_host("10.0.0.1"), "10.0.0.1");
    }
}
