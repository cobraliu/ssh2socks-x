//! Cross-platform persistence of the tunnel list as JSON.
//!
//! Locations (same as the Python version):
//! - Linux:   `$XDG_CONFIG_HOME/ssh2socks/tunnels.json` (default `~/.config`)
//! - macOS:   `~/Library/Application Support/ssh2socks/tunnels.json`
//! - Windows: `%APPDATA%\ssh2socks\tunnels.json`

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::models::Tunnel;

const APP_NAME: &str = "ssh2socks";

pub fn tunnels_file() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_NAME)
        .join("tunnels.json")
}

pub fn load() -> Vec<Tunnel> {
    load_from(&tunnels_file())
}

pub fn save(tunnels: &[Tunnel]) -> io::Result<()> {
    save_to(&tunnels_file(), tunnels)
}

fn load_from(path: &Path) -> Vec<Tunnel> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(&text) else {
        return Vec::new();
    };
    // Skip malformed entries instead of discarding the whole file.
    items
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect()
}

fn save_to(path: &Path, tunnels: &[Tunnel]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let payload = serde_json::to_string_pretty(tunnels).map_err(io::Error::other)?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, payload)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_skip_bad_entries() {
        let dir = std::env::temp_dir().join(format!("ssh2socks-test-{}", crate::models::new_id()));
        let path = dir.join("tunnels.json");
        let t = Tunnel {
            name: "n".into(),
            host: "h".into(),
            port: 1080,
            probe_url: "http://x".into(),
            auto_reconnect: true,
            id: "1".into(),
        };
        save_to(&path, std::slice::from_ref(&t)).unwrap();
        assert_eq!(load_from(&path), vec![t.clone()]);

        fs::write(
            &path,
            r#"[{"name":"n","host":"h","port":1080,"id":"1"},{"bad":true}]"#,
        )
        .unwrap();
        assert_eq!(load_from(&path).len(), 1);
        assert!(load_from(&dir.join("missing.json")).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
}
