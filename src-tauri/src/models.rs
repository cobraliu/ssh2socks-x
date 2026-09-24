//! Core data model for a user-defined SSH SOCKS tunnel.

use serde::{Deserialize, Serialize};

pub const DEFAULT_PROBE_URL: &str = "http://www.gstatic.com/generate_204";

fn default_probe_url() -> String {
    DEFAULT_PROBE_URL.to_string()
}

fn default_true() -> bool {
    true
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// A saved tunnel definition (persisted to disk).
///
/// The JSON layout matches the earlier Python version, so an existing
/// `tunnels.json` keeps working.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tunnel {
    pub name: String,
    /// The ssh_config Host alias.
    pub host: String,
    /// Local SOCKS listen port.
    pub port: u16,
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
    pub latency_ms: f64,
    pub message: String,
}

/// What the UI sees for one tunnel: its definition plus live status.
#[derive(Clone, Debug, Serialize)]
pub struct TunnelView {
    #[serde(flatten)]
    pub tunnel: Tunnel,
    pub state: TunnelState,
    pub probe: Option<ProbeResult>,
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
    }
}
