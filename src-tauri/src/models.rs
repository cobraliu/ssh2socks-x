//! Core data model for a user-defined SSH tunnel.

use serde::{Deserialize, Serialize};

pub const DEFAULT_PROBE_URL: &str = "http://www.gstatic.com/generate_204";

fn default_probe_url() -> String {
    DEFAULT_PROBE_URL.to_string()
}

fn default_true() -> bool {
    true
}

fn default_target_host() -> String {
    "127.0.0.1".to_string()
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// What kind of forwarding a tunnel sets up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TunnelKind {
    /// `ssh -D`: a local SOCKS5 proxy.
    #[default]
    Socks,
    /// `ssh -L`: local port -> `target_host:remote_port` as seen from the server.
    Local,
    /// `ssh -R`: server `0.0.0.0:remote_port` -> local `127.0.0.1:port`.
    Remote,
}

impl TunnelKind {
    /// Whether ssh listens on the local `port` (vs. connecting to it).
    pub fn listens_locally(self) -> bool {
        matches!(self, TunnelKind::Socks | TunnelKind::Local)
    }
}

/// A saved tunnel definition (persisted to disk).
///
/// The JSON layout extends the earlier Python version (which only had SOCKS
/// tunnels), so an existing `tunnels.json` keeps working.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tunnel {
    pub name: String,
    /// The ssh_config Host alias.
    pub host: String,
    #[serde(default)]
    pub kind: TunnelKind,
    /// Socks/Local: the local listen port. Remote: the local service port.
    pub port: u16,
    /// Local: target port on the server side. Remote: port opened on the server.
    #[serde(default)]
    pub remote_port: u16,
    /// Local only: target address as seen from the server (loopback or LAN).
    #[serde(default = "default_target_host")]
    pub target_host: String,
    #[serde(default = "default_probe_url")]
    pub probe_url: String,
    #[serde(default = "default_true")]
    pub auto_reconnect: bool,
    #[serde(default = "new_id")]
    pub id: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TunnelState {
    #[default]
    Stopped,
    Connecting,
    Connected,
    Error,
}

impl TunnelState {
    pub fn active(self) -> bool {
        matches!(self, TunnelState::Connecting | TunnelState::Connected)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ProbeResult {
    pub ok: bool,
    pub latency_ms: Option<f64>,
    pub message: String,
}

/// What the UI sees for one tunnel: its definition plus live status.
#[derive(Clone, Debug, Serialize)]
pub struct TunnelView {
    #[serde(flatten)]
    pub tunnel: Tunnel,
    pub state: TunnelState,
    pub probe: Option<ProbeResult>,
    /// Latest error / progress message worth showing next to the tunnel.
    pub detail: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_python_era_json() {
        let json = r#"{"name":"a","host":"h","port":1080,"probe_url":"http://x",
                       "auto_reconnect":false,"id":"abc","extra":1}"#;
        let t: Tunnel = serde_json::from_str(json).unwrap();
        assert_eq!(t.port, 1080);
        assert!(!t.auto_reconnect);
        assert_eq!(t.id, "abc");
    }

    #[test]
    fn fills_defaults() {
        let t: Tunnel = serde_json::from_str(r#"{"name":"a","host":"h","port":1}"#).unwrap();
        assert_eq!(t.probe_url, DEFAULT_PROBE_URL);
        assert!(t.auto_reconnect);
        assert_eq!(t.id.len(), 32);
        assert_eq!(t.kind, TunnelKind::Socks);
        assert_eq!(t.target_host, "127.0.0.1");
    }
}
