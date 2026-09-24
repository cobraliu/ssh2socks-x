//! End-to-end connectivity probe through the local SOCKS5 proxy.
//!
//! Connects to 127.0.0.1:<port> (the running `ssh -D`), asks it to CONNECT to
//! the probe target and, for `http://` URLs, performs a tiny HTTP request.
//! For `https://` URLs a successful CONNECT is taken as success: it already
//! proves the ssh server reached the target, and skipping TLS keeps the binary
//! small.

use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::models::ProbeResult;

pub const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

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
        .map_err(|e| format!("无法连接本地代理：{e}"))?;
    // Greeting: SOCKS5, 1 method, no-auth.
    sock.write_all(&[0x05, 0x01, 0x00]).await.map_err(io_err)?;
    let mut reply = [0u8; 2];
    sock.read_exact(&mut reply).await.map_err(io_err)?;
    if reply != [0x05, 0x00] {
        return Err("SOCKS5 握手被拒绝".into());
    }
    // CONNECT with address type = domain name (resolved by the ssh server).
    let host_bytes = host.as_bytes();
    if host_bytes.len() > 255 {
        return Err("主机名过长".into());
    }
    let mut req = vec![0x05, 0x01, 0x00, 0x03, host_bytes.len() as u8];
    req.extend_from_slice(host_bytes);
    req.extend_from_slice(&port.to_be_bytes());
    sock.write_all(&req).await.map_err(io_err)?;
    let mut header = [0u8; 4];
    sock.read_exact(&mut header).await.map_err(io_err)?;
    if header[1] != 0x00 {
        return Err(format!("远端无法连接目标（SOCKS 错误码 {}）", header[1]));
    }
    let skip = match header[3] {
        0x01 => 4 + 2,
        0x04 => 16 + 2,
        0x03 => {
            let mut len = [0u8; 1];
            sock.read_exact(&mut len).await.map_err(io_err)?;
            len[0] as usize + 2
        }
        _ => return Err("SOCKS5 响应格式错误".into()),
    };
    let mut rest = vec![0u8; skip];
    sock.read_exact(&mut rest).await.map_err(io_err)?;
    Ok(sock)
}

fn io_err(e: std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::UnexpectedEof {
        "代理关闭了连接".into()
    } else {
        e.to_string()
    }
}

async fn probe_inner(proxy_port: u16, target: &Target) -> Result<(), String> {
    let mut sock = socks5_connect(proxy_port, &target.host, target.port).await?;
    if target.https {
        return Ok(());
    }
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: ssh2socks\r\nConnection: close\r\nAccept: */*\r\n\r\n",
        target.path, target.host
    );
    sock.write_all(request.as_bytes()).await.map_err(io_err)?;
    let mut buf = [0u8; 64];
    let n = sock.read(&mut buf).await.map_err(io_err)?;
    if buf[..n].starts_with(b"HTTP/") {
        Ok(())
    } else {
        Err("无有效响应".into())
    }
}

pub async fn run_probe(proxy_port: u16, probe_url: &str, limit: Duration) -> ProbeResult {
    let Some(target) = parse_url(probe_url) else {
        return ProbeResult {
            ok: false,
            latency_ms: 0.0,
            message: "无效的探测地址".into(),
        };
    };
    let started = Instant::now();
    let outcome = timeout(limit, probe_inner(proxy_port, &target)).await;
    let latency_ms = started.elapsed().as_secs_f64() * 1000.0;
    match outcome {
        Ok(Ok(())) => ProbeResult {
            ok: true,
            latency_ms,
            message: "通".into(),
        },
        Ok(Err(message)) => ProbeResult {
            ok: false,
            latency_ms,
            message,
        },
        Err(_) => ProbeResult {
            ok: false,
            latency_ms,
            message: "超时".into(),
        },
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
    async fn probe_no_proxy() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let r = run_probe(port, "http://example.com/", Duration::from_secs(3)).await;
        assert!(!r.ok);
    }
}
