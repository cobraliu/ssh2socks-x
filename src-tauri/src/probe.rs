//! End-to-end connectivity probes.
//!
//! SOCKS tunnels: connects to 127.0.0.1:<port> (the running `ssh -D`), asks it to CONNECT to
//! the probe target and, for `http://` URLs, performs a tiny HTTP request.
//! For `https://` URLs a successful CONNECT is taken as success: it already
//! proves the ssh server reached the target, and skipping TLS keeps the binary
//! small.
//!
//! Port forwards have their own, protocol-agnostic checks; see
//! [`probe_local_forward`] and [`probe_remote_forward`].

use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::models::ProbeResult;

pub const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
/// How long a forwarded connection must stay open to count as working.
const FORWARD_SETTLE: Duration = Duration::from_secs(3);
const LOCAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

fn ok(latency_ms: Option<f64>) -> ProbeResult {
    ProbeResult {
        ok: true,
        latency_ms,
        message: tr!("通", "up"),
    }
}

fn fail(latency_ms: Option<f64>, message: impl Into<String>) -> ProbeResult {
    ProbeResult {
        ok: false,
        latency_ms,
        message: message.into(),
    }
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

#[derive(Debug, PartialEq)]
struct Target {
    https: bool,
    host: String,
    port: u16,
    path: String,
}

fn parse_url(url: &str) -> Option<Target> {
    let url = url.trim();
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r),
        None => ("http".to_string(), url),
    };
    let https = match scheme.as_str() {
        "http" => false,
        "https" => true,
        _ => return None,
    };
    let (authority, path) = match rest.find(['/', '?']) {
        Some(i) if rest[i..].starts_with('/') => (&rest[..i], rest[i..].to_string()),
        Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
        None => (rest, "/".to_string()),
    };
    let path = path.split('#').next().unwrap_or("/").to_string();
    let default_port = if https { 443 } else { 80 };
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (h, after) = v6.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None => default_port,
        };
        (h.to_string(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse().ok()?),
            None => (authority.to_string(), default_port),
        }
    };
    if host.is_empty() {
        return None;
    }
    Some(Target {
        https,
        host,
        port,
        path,
    })
}

async fn socks5_connect(proxy_port: u16, host: &str, port: u16) -> Result<TcpStream, String> {
    let mut sock = TcpStream::connect(("127.0.0.1", proxy_port))
        .await
        .map_err(|e| {
            tr!(
                "无法连接本地代理：{e}",
                "Could not connect to the local proxy: {e}"
            )
        })?;
    // Greeting: SOCKS5, 1 method, no-auth.
    sock.write_all(&[0x05, 0x01, 0x00]).await.map_err(io_err)?;
    let mut reply = [0u8; 2];
    sock.read_exact(&mut reply).await.map_err(io_err)?;
    if reply != [0x05, 0x00] {
        return Err(tr!("SOCKS5 握手被拒绝", "SOCKS5 handshake rejected"));
    }
    // CONNECT with address type = domain name (resolved by the ssh server).
    let host_bytes = host.as_bytes();
    if host_bytes.len() > 255 {
        return Err(tr!("主机名过长", "Host name too long"));
    }
    let mut req = vec![0x05, 0x01, 0x00, 0x03, host_bytes.len() as u8];
    req.extend_from_slice(host_bytes);
    req.extend_from_slice(&port.to_be_bytes());
    sock.write_all(&req).await.map_err(io_err)?;
    let mut header = [0u8; 4];
    sock.read_exact(&mut header).await.map_err(io_err)?;
    if header[1] != 0x00 {
        return Err(tr!(
            "远端无法连接目标（SOCKS 错误码 {}）",
            "The server could not reach the target (SOCKS error {})",
            header[1]
        ));
    }
    let skip = match header[3] {
        0x01 => 4 + 2,
        0x04 => 16 + 2,
        0x03 => {
            let mut len = [0u8; 1];
            sock.read_exact(&mut len).await.map_err(io_err)?;
            len[0] as usize + 2
        }
        _ => return Err(tr!("SOCKS5 响应格式错误", "Malformed SOCKS5 response")),
    };
    let mut rest = vec![0u8; skip];
    sock.read_exact(&mut rest).await.map_err(io_err)?;
    Ok(sock)
}

fn io_err(e: std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::UnexpectedEof {
        tr!("代理关闭了连接", "The proxy closed the connection")
    } else {
        e.to_string()
    }
}

async fn probe_inner(proxy_port: u16, target: &Target) -> Result<(), String> {
    let mut sock = socks5_connect(proxy_port, &target.host, target.port).await?;
    if target.https {
        return Ok(());
    }
    let request = format!("GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: ssh2socks\r\nConnection: close\r\nAccept: */*\r\n\r\n",
        target.path, target.host
    );
    sock.write_all(request.as_bytes()).await.map_err(io_err)?;
    let mut buf = [0u8; 64];
    let n = sock.read(&mut buf).await.map_err(io_err)?;
    if buf[..n].starts_with(b"HTTP/") {
        Ok(())
    } else {
        Err(tr!("无有效响应", "No valid response"))
    }
}

pub async fn run_probe(proxy_port: u16, probe_url: &str, limit: Duration) -> ProbeResult {
    let Some(target) = parse_url(probe_url) else {
        return fail(None, tr!("无效的探测地址", "Invalid probe URL"));
    };
    let started = Instant::now();
    let outcome = timeout(limit, probe_inner(proxy_port, &target)).await;
    let latency = Some(elapsed_ms(started));
    match outcome {
        Ok(Ok(())) => ok(latency),
        Ok(Err(message)) => fail(latency, message),
        Err(_) => fail(latency, tr!("超时", "Timed out")),
    }
}

/// `ssh -L`: ssh accepts on the local port and then asks the server to open
/// the target; when the server cannot reach it, ssh closes our connection
/// right away. A connection that stays open (or gets a banner) is working.
pub async fn probe_local_forward(port: u16, target: &str) -> ProbeResult {
    let started = Instant::now();
    let mut sock = match timeout(
        LOCAL_CONNECT_TIMEOUT,
        TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    {
        Ok(Ok(sock)) => sock,
        _ => {
            return fail(
                None,
                tr!(
                    "本地端口 {port} 未监听",
                    "Nothing is listening on local port {port}"
                ),
            )
        }
    };
    let mut buf = [0u8; 1];
    match timeout(FORWARD_SETTLE, sock.read(&mut buf)).await {
        // The service spoke first (ssh/mysql/redis banner…): a real round trip.
        Ok(Ok(n)) if n > 0 => ok(Some(elapsed_ms(started))),
        // Silent services (HTTP waits for a request) keep the line open.
        Err(_) => ok(None),
        Ok(_) => fail(
            None,
            tr!("服务器无法连接 {target}", "The server can't reach {target}"),
        ),
    }
}

/// `ssh -R`: checks that the local service is up, then connects to the
/// published address from this machine.
pub async fn probe_remote_forward(
    local_port: u16,
    public_host: &str,
    remote_port: u16,
) -> ProbeResult {
    let local = timeout(
        LOCAL_CONNECT_TIMEOUT,
        TcpStream::connect(("127.0.0.1", local_port)),
    )
    .await;
    if !matches!(local, Ok(Ok(_))) {
        return fail(
            None,
            tr!(
                "本地 {local_port} 端口没有服务在运行",
                "No service is running on local port {local_port}"
            ),
        );
    }
    let started = Instant::now();
    match timeout(
        PROBE_TIMEOUT,
        TcpStream::connect((public_host, remote_port)),
    )
    .await
    {
        Ok(Ok(_)) => ok(Some(elapsed_ms(started))),
        _ => fail(
            None,
            tr!("本机访问不到 {}:{remote_port}（检查服务器防火墙 / sshd 的 GatewayPorts）", "{}:{remote_port} is not reachable from this machine (check the server firewall / sshd GatewayPorts)",
                host_for_url(public_host)
            ),
        ),
    }
}

/// Bracket IPv6 literals for use in `host:port` / URLs.
pub fn host_for_url(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn parses_urls() {
        let t = parse_url("http://www.gstatic.com/generate_204").unwrap();
        assert_eq!(
            t,
            Target {
                https: false,
                host: "www.gstatic.com".into(),
                port: 80,
                path: "/generate_204".into()
            }
        );
        let t = parse_url("https://example.com:8443?q=1#frag").unwrap();
        assert_eq!((t.https, t.port, t.path.as_str()), (true, 8443, "/?q=1"));
        let t = parse_url("baidu.com").unwrap();
        assert_eq!((t.https, t.host.as_str(), t.port), (false, "baidu.com", 80));
        let t = parse_url("http://[::1]:8080/x").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("::1", 8080));
        assert!(parse_url("ftp://x").is_none());
        assert!(parse_url("http://").is_none());
    }

    /// Minimal SOCKS5 server: accepts CONNECT, then answers like an HTTP server
    /// (or fails the CONNECT with `connect_code`).
    async fn fake_proxy(connect_code: u8) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut greet = [0u8; 3];
            s.read_exact(&mut greet).await.unwrap();
            s.write_all(&[5, 0]).await.unwrap();
            let mut head = [0u8; 5];
            s.read_exact(&mut head).await.unwrap();
            let mut rest = vec![0u8; head[4] as usize + 2];
            s.read_exact(&mut rest).await.unwrap();
            s.write_all(&[5, connect_code, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            if connect_code == 0 {
                let mut req = [0u8; 256];
                let _ = s.read(&mut req).await.unwrap();
                s.write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
                    .await
                    .unwrap();
            }
        });
        port
    }

    #[tokio::test]
    async fn probe_success() {
        let port = fake_proxy(0).await;
        let r = run_probe(port, "http://example.com/generate_204", PROBE_TIMEOUT).await;
        assert!(r.ok, "{r:?}");
    }

    #[tokio::test]
    async fn probe_remote_failure() {
        let port = fake_proxy(5).await;
        let r = run_probe(port, "http://example.com/", PROBE_TIMEOUT).await;
        assert!(!r.ok);
        assert!(r.message.contains("5"), "{}", r.message);
    }

    #[tokio::test]
    async fn local_forward_detects_closed_channel() {
        // Accepts then closes immediately: what ssh does when the server
        // cannot reach the target.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let (s, _) = listener.accept().await.unwrap();
                drop(s);
            }
        });
        let r = probe_local_forward(port, "10.0.0.1:80").await;
        assert!(!r.ok, "{r:?}");
        assert!(r.message.contains("10.0.0.1:80"));
    }

    #[tokio::test]
    async fn local_forward_accepts_banner() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            s.write_all(b"SSH-2.0-test\r\n").await.unwrap();
            sleep_forever().await;
        });
        let r = probe_local_forward(port, "x:22").await;
        assert!(r.ok && r.latency_ms.is_some(), "{r:?}");
    }

    #[tokio::test]
    async fn remote_forward_checks_local_service_first() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let r = probe_remote_forward(port, "127.0.0.1", 1).await;
        assert!(!r.ok && r.message.contains("没有服务"), "{r:?}");

        // Local service up and "published" port reachable.
        let local = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let public = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let r = probe_remote_forward(
            local.local_addr().unwrap().port(),
            "127.0.0.1",
            public.local_addr().unwrap().port(),
        )
        .await;
        assert!(r.ok, "{r:?}");
    }

    #[test]
    fn brackets_ipv6() {
        assert_eq!(host_for_url("::1"), "[::1]");
        assert_eq!(host_for_url("example.com"), "example.com");
    }

    async fn sleep_forever() {
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }

    #[tokio::test]
    async fn probe_no_proxy() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let r = run_probe(port, "http://example.com/", Duration::from_secs(3)).await;
        assert!(!r.ok);
    }
}
