//! Point the system proxy at a SOCKS tunnel, and put it back afterwards.
//!
//! Every setting we touch is saved first (`before`) together with the value
//! we write (`ours`) in `sysproxy.json`, so the previous setup comes back
//! when the tunnel stops, when the app quits, or on the next launch after a
//! crash. A setting is only put back while it still holds our value: if
//! the user (or another tool) changed it in the meantime, theirs is kept.
//!
//! - Windows: WinINet settings in the registry (`ProxyServer=socks=…`).
//! - macOS:   `networksetup` SOCKS proxy on every enabled network service.
//! - Linux:   GNOME `gsettings org.gnome.system.proxy` (also used by
//!   Cinnamon, Budgie, MATE apps and browsers there).

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Entry {
    key: String,
    before: Option<String>,
    ours: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Record {
    tunnel: String,
    port: u16,
    entries: Vec<Entry>,
}

/// Where the settings live. `wanted` lists what to write, in order.
trait Backend {
    fn wanted(&self, port: u16) -> Result<Vec<(String, Option<String>)>, String>;
    fn read(&self, key: &str) -> Result<Option<String>, String>;
    fn write(&self, key: &str, value: Option<&str>) -> Result<(), String>;
    /// Tell running programs the settings changed.
    fn notify(&self) {}
}

struct Proxy<B> {
    backend: B,
    file: PathBuf,
    current: Option<Record>,
}

impl<B: Backend> Proxy<B> {
    fn new(backend: B, file: PathBuf) -> Self {
        Self {
            backend,
            file,
            current: None,
        }
    }

    fn save(&self) -> Result<(), String> {
        match &self.current {
            None => match std::fs::remove_file(&self.file) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
                _ => Ok(()),
            },
            Some(record) => {
                if let Some(dir) = self.file.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                }
                let text = serde_json::to_string_pretty(record).map_err(|e| e.to_string())?;
                std::fs::write(&self.file, text).map_err(|e| e.to_string())
            }
        }
    }

    /// Put back what an earlier run left set (it crashed or was killed).
    fn recover(&mut self) {
        if self.current.is_some() {
            return;
        }
        let Ok(text) = std::fs::read_to_string(&self.file) else {
            return;
        };
        match serde_json::from_str::<Record>(&text) {
            Ok(record) => {
                self.current = Some(record);
                let _ = self.restore();
            }
            Err(_) => {
                let _ = std::fs::remove_file(&self.file);
            }
        }
    }

    fn owner(&self) -> Option<&str> {
        self.current.as_ref().map(|r| r.tunnel.as_str())
    }

    fn enable(&mut self, tunnel: &str, port: u16) -> Result<(), String> {
        self.restore()?;
        let mut entries = Vec::new();
        for (key, ours) in self.backend.wanted(port)? {
            let before = self.backend.read(&key)?;
            entries.push(Entry { key, before, ours });
        }
        // Saved before anything changes, so a crash half-way can be undone.
        self.current = Some(Record {
            tunnel: tunnel.to_string(),
            port,
            entries: entries.clone(),
        });
        self.save()?;
        for entry in &entries {
            if let Err(e) = self.backend.write(&entry.key, entry.ours.as_deref()) {
                let _ = self.restore();
                return Err(e);
            }
        }
        self.backend.notify();
        Ok(())
    }

    /// Undo `enable`, keeping any setting someone else changed since.
    fn restore(&mut self) -> Result<(), String> {
        let Some(record) = self.current.clone() else {
            return Ok(());
        };
        let mut result = Ok(());
        for entry in record.entries.iter().rev() {
            match self.backend.read(&entry.key) {
                Ok(now) if now == entry.ours => {
                    if let Err(e) = self.backend.write(&entry.key, entry.before.as_deref()) {
                        result = Err(e);
                    }
                }
                Ok(_) => {}
                Err(e) => result = Err(e),
            }
        }
        self.backend.notify();
        // Even after an error: retrying would not do better, and a stale
        // record must not keep overwriting the user's settings later.
        self.current = None;
        self.save()?;
        result
    }
}

// ---- process-wide instance ---------------------------------------------------

#[cfg(target_os = "linux")]
type SystemBackend = gnome::Gnome;
#[cfg(target_os = "macos")]
type SystemBackend = mac::NetworkSetup;
#[cfg(windows)]
type SystemBackend = win::WinInet;

static SYSTEM: Mutex<Option<Proxy<SystemBackend>>> = Mutex::new(None);
/// Copy of the owner, readable while a slow change holds `SYSTEM`.
static OWNER: Mutex<Option<String>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut Proxy<SystemBackend>) -> T) -> T {
    let mut guard = SYSTEM.lock().unwrap_or_else(|e| e.into_inner());
    let proxy = guard.get_or_insert_with(|| {
        Proxy::new(
            SystemBackend::default(),
            crate::store::config_dir().join("sysproxy.json"),
        )
    });
    let result = f(proxy);
    *OWNER.lock().unwrap_or_else(|e| e.into_inner()) = proxy.owner().map(str::to_string);
    result
}

fn failed(e: String) -> String {
    tr!(
        "设置系统代理失败：{e}",
        "Could not change the system proxy: {e}"
    )
}

/// Undo what a crashed earlier run left behind. Call once at startup.
pub fn recover() {
    with(|p| p.recover());
}

/// The tunnel the system proxy currently points at.
pub fn owner() -> Option<String> {
    OWNER.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn enable(tunnel: &str, port: u16) -> Result<(), String> {
    with(|p| p.enable(tunnel, port)).map_err(failed)
}

pub fn disable() -> Result<(), String> {
    with(|p| p.restore()).map_err(failed)
}

/// Restore the previous settings if they point at this tunnel.
pub fn release(tunnel: &str) -> bool {
    if owner().as_deref() != Some(tunnel) {
        return false;
    }
    let _ = disable();
    true
}

// ---- Linux: GNOME ------------------------------------------------------------

#[cfg(target_os = "linux")]
mod gnome {
    use std::process::{Command, Stdio};

    /// Keys are "schema key"; values are GVariant text as `gsettings get`
    /// prints it.
    #[derive(Default)]
    pub struct Gnome {
        /// Test hook: extra environment for `gsettings`.
        pub env: Vec<(String, String)>,
    }

    impl Gnome {
        fn run(&self, args: &[&str]) -> Result<String, String> {
            let out = Command::new("gsettings")
                .args(args)
                .envs(self.env.iter().map(|(k, v)| (k, v)))
                .stdin(Stdio::null())
                .output()
                .map_err(|_| {
                    tr!(
                        "找不到 gsettings，当前桌面环境暂不支持自动设置（支持 GNOME 系桌面）。可以用「终端配置」手动设置",
                        "gsettings was not found, so this desktop is not supported (GNOME-based desktops are). Use the terminal settings instead"
                    )
                })?;
            if !out.status.success() {
                return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
            }
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
    }

    impl super::Backend for Gnome {
        fn wanted(&self, port: u16) -> Result<Vec<(String, Option<String>)>, String> {
            // Fails early when the schema is missing (not a GNOME system).
            self.run(&["list-keys", "org.gnome.system.proxy"])?;
            let p = "org.gnome.system.proxy";
            Ok(vec![
                (format!("{p}.socks host"), Some("'127.0.0.1'".into())),
                (format!("{p}.socks port"), Some(port.to_string())),
                // Manual mode would also use any HTTP proxy left configured.
                (format!("{p}.http host"), Some("''".into())),
                (format!("{p}.https host"), Some("''".into())),
                (format!("{p}.ftp host"), Some("''".into())),
                (format!("{p} mode"), Some("'manual'".into())),
            ])
        }

        fn read(&self, key: &str) -> Result<Option<String>, String> {
            let (schema, key) = key.split_once(' ').ok_or("bad key")?;
            self.run(&["get", schema, key]).map(Some)
        }

        fn write(&self, key: &str, value: Option<&str>) -> Result<(), String> {
            let (schema, key) = key.split_once(' ').ok_or("bad key")?;
            match value {
                Some(v) => self.run(&["set", schema, key, v]),
                None => self.run(&["reset", schema, key]),
            }
            .map(drop)
        }
    }
}

// ---- macOS: networksetup ---------------------------------------------------------

#[cfg(target_os = "macos")]
mod mac {
    use std::process::{Command, Stdio};

    /// Keys are network service names; values are "Yes|No server port".
    #[derive(Default)]
    pub struct NetworkSetup;

    fn run(args: &[&str]) -> Result<String, String> {
        let out = Command::new("/usr/sbin/networksetup")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        // networksetup reports some errors on stdout with status 0.
        if !out.status.success() || text.starts_with("** Error") {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(if err.is_empty() { text } else { err });
        }
        Ok(text)
    }

    /// Enabled services from `-listallnetworkservices` (the first line is
    /// a note; disabled services start with `*`).
    pub fn services(listing: &str) -> Vec<String> {
        listing
            .lines()
            .skip(1)
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('*'))
            .map(str::to_string)
            .collect()
    }

    /// `-getsocksfirewallproxy` output as "Yes|No server port".
    pub fn parse_state(text: &str) -> String {
        let field = |name: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(name))
                .map(|v| v.trim().to_string())
                .unwrap_or_default()
        };
        format!(
            "{} {} {}",
            field("Enabled:"),
            field("Server:"),
            field("Port:")
        )
    }

    impl super::Backend for NetworkSetup {
        fn wanted(&self, port: u16) -> Result<Vec<(String, Option<String>)>, String> {
            let list = services(&run(&["-listallnetworkservices"])?);
            if list.is_empty() {
                return Err(tr!("没有可用的网络服务", "No network service is enabled"));
            }
            Ok(list
                .into_iter()
                .map(|s| (s, Some(format!("Yes 127.0.0.1 {port}"))))
                .collect())
        }

        fn read(&self, key: &str) -> Result<Option<String>, String> {
            run(&["-getsocksfirewallproxy", key]).map(|t| Some(parse_state(&t)))
        }

        fn write(&self, key: &str, value: Option<&str>) -> Result<(), String> {
            let value = value.unwrap_or("No");
            let mut parts = value.split(' ');
            let on = parts.next() == Some("Yes");
            let server = parts.next().unwrap_or("");
            let port = parts.next().unwrap_or("");
            if !server.is_empty() && !port.is_empty() && port != "0" {
                // Also turns the proxy on.
                run(&["-setsocksfirewallproxy", key, server, port])?;
            }
            run(&[
                "-setsocksfirewallproxystate",
                key,
                if on { "on" } else { "off" },
            ])
            .map(drop)
        }
    }
}

// ---- Windows: WinINet ------------------------------------------------------------

#[cfg(windows)]
mod win {
    use crate::winreg;

    pub const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

    /// Keys are value names under Internet Settings; DWORDs as decimal text.
    #[derive(Default)]
    pub struct WinInet {
        /// Test hook: another registry key instead of the real one.
        pub key: Option<String>,
    }

    impl WinInet {
        fn key(&self) -> &str {
            self.key.as_deref().unwrap_or(KEY)
        }
    }

    impl super::Backend for WinInet {
        fn wanted(&self, port: u16) -> Result<Vec<(String, Option<String>)>, String> {
            Ok(vec![
                (
                    "ProxyServer".into(),
                    Some(format!("socks=127.0.0.1:{port}")),
                ),
                ("ProxyEnable".into(), Some("1".into())),
            ])
        }

        fn read(&self, name: &str) -> Result<Option<String>, String> {
            Ok(if name == "ProxyEnable" {
                winreg::get_dword(self.key(), name).map(|v| v.to_string())
            } else {
                winreg::get_string(self.key(), name)
            })
        }

        fn write(&self, name: &str, value: Option<&str>) -> Result<(), String> {
            match value {
                None => winreg::delete(self.key(), name),
                Some(v) if name == "ProxyEnable" => {
                    winreg::set_dword(self.key(), name, v.parse().unwrap_or(0))
                }
                Some(v) => winreg::set_string(self.key(), name, v),
            }
        }

        fn notify(&self) {
            use windows_sys::Win32::Networking::WinInet::{
                InternetSetOptionW, INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED,
            };
            if self.key.is_some() {
                return;
            }
            // SAFETY: option-only calls with no buffer.
            unsafe {
                InternetSetOptionW(
                    std::ptr::null(),
                    INTERNET_OPTION_SETTINGS_CHANGED,
                    std::ptr::null(),
                    0,
                );
                InternetSetOptionW(
                    std::ptr::null(),
                    INTERNET_OPTION_REFRESH,
                    std::ptr::null(),
                    0,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Fake {
        values: RefCell<HashMap<String, String>>,
        fail_on: Option<&'static str>,
    }

    impl Backend for &Fake {
        fn wanted(&self, port: u16) -> Result<Vec<(String, Option<String>)>, String> {
            Ok(vec![
                ("server".into(), Some(format!("127.0.0.1:{port}"))),
                ("enable".into(), Some("1".into())),
            ])
        }
        fn read(&self, key: &str) -> Result<Option<String>, String> {
            Ok(self.values.borrow().get(key).cloned())
        }
        fn write(&self, key: &str, value: Option<&str>) -> Result<(), String> {
            if self.fail_on == Some(key) {
                return Err("denied".into());
            }
            let mut values = self.values.borrow_mut();
            match value {
                Some(v) => values.insert(key.into(), v.into()),
                None => values.remove(key),
            };
            Ok(())
        }
    }

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ssh2socks-sysproxy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(name);
        let _ = std::fs::remove_file(&file);
        file
    }

    fn get(fake: &Fake, key: &str) -> Option<String> {
        fake.values.borrow().get(key).cloned()
    }

    #[test]
    fn enable_then_restore_puts_back_previous_values() {
        let fake = Fake::default();
        fake.values.borrow_mut().insert("enable".into(), "0".into());
        let file = temp_file("a.json");
        let mut p = Proxy::new(&fake, file.clone());
        p.enable("t1", 1080).unwrap();
        assert_eq!(get(&fake, "server").as_deref(), Some("127.0.0.1:1080"));
        assert_eq!(get(&fake, "enable").as_deref(), Some("1"));
        assert_eq!(p.owner(), Some("t1"));
        assert!(file.exists());

        // Switching tunnels restores first, so `before` stays the user's.
        p.enable("t2", 1081).unwrap();
        assert_eq!(get(&fake, "server").as_deref(), Some("127.0.0.1:1081"));
        p.restore().unwrap();
        assert_eq!(get(&fake, "server"), None);
        assert_eq!(get(&fake, "enable").as_deref(), Some("0"));
        assert_eq!(p.owner(), None);
        assert!(!file.exists());
    }

    #[test]
    fn keeps_settings_changed_by_someone_else() {
        let fake = Fake::default();
        let mut p = Proxy::new(&fake, temp_file("b.json"));
        p.enable("t", 1080).unwrap();
        fake.values
            .borrow_mut()
            .insert("server".into(), "corp:3128".into());
        p.restore().unwrap();
        assert_eq!(get(&fake, "server").as_deref(), Some("corp:3128"));
        assert_eq!(get(&fake, "enable"), None);
    }

    #[test]
    fn recovers_after_a_crash() {
        let fake = Fake::default();
        fake.values.borrow_mut().insert("enable".into(), "0".into());
        let file = temp_file("c.json");
        let mut crashed = Proxy::new(&fake, file.clone());
        crashed.enable("t", 1080).unwrap();
        drop(crashed);

        let mut next = Proxy::new(&fake, file.clone());
        next.recover();
        assert_eq!(get(&fake, "enable").as_deref(), Some("0"));
        assert_eq!(get(&fake, "server"), None);
        assert!(!file.exists());
        // Nothing left: a second recovery is a no-op.
        next.recover();
        assert_eq!(next.owner(), None);
    }

    #[test]
    fn failed_write_rolls_back() {
        let fake = Fake {
            fail_on: Some("enable"),
            ..Fake::default()
        };
        let file = temp_file("d.json");
        let mut p = Proxy::new(&fake, file.clone());
        assert!(p.enable("t", 1080).is_err());
        assert_eq!(get(&fake, "server"), None);
        assert_eq!(p.owner(), None);
        assert!(!file.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn gnome_round_trip() {
        let home = std::env::temp_dir().join(format!("ssh2socks-gsettings-{}", std::process::id()));
        let gnome = gnome::Gnome {
            env: vec![
                ("GSETTINGS_BACKEND".into(), "keyfile".into()),
                ("XDG_CONFIG_HOME".into(), home.display().to_string()),
            ],
        };
        // Skip where GNOME's schemas are not installed.
        if gnome.read("org.gnome.system.proxy mode").is_err() {
            return;
        }
        gnome
            .write("org.gnome.system.proxy.http host", Some("'corp'"))
            .unwrap();
        let mut p = Proxy::new(gnome, home.join("sysproxy.json"));
        p.enable("t", 1080).unwrap();
        let read = |k: &str| p.backend.read(k).unwrap().unwrap();
        assert_eq!(read("org.gnome.system.proxy mode"), "'manual'");
        assert_eq!(read("org.gnome.system.proxy.socks host"), "'127.0.0.1'");
        assert_eq!(read("org.gnome.system.proxy.socks port"), "1080");
        assert_eq!(read("org.gnome.system.proxy.http host"), "''");
        p.restore().unwrap();
        let read = |k: &str| p.backend.read(k).unwrap().unwrap();
        assert_eq!(read("org.gnome.system.proxy mode"), "'none'");
        assert_eq!(read("org.gnome.system.proxy.socks port"), "0");
        assert_eq!(read("org.gnome.system.proxy.http host"), "'corp'");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_networksetup_output() {
        let list = "An asterisk (*) denotes that a network service is disabled.\nWi-Fi\n*Bluetooth PAN\nThunderbolt Bridge\n";
        assert_eq!(mac::services(list), ["Wi-Fi", "Thunderbolt Bridge"]);
        let state = "Enabled: No\nServer: \nPort: 0\nAuthenticated Proxy Enabled: 0\n";
        assert_eq!(mac::parse_state(state), "No  0");
        let state = "Enabled: Yes\nServer: 127.0.0.1\nPort: 1080\n";
        assert_eq!(mac::parse_state(state), "Yes 127.0.0.1 1080");
    }

    #[cfg(windows)]
    #[test]
    fn wininet_round_trip() {
        let key = format!(r"Software\ssh2socks-test-proxy-{}", std::process::id());
        crate::winreg::set_dword(&key, "ProxyEnable", 0).unwrap();
        let backend = win::WinInet {
            key: Some(key.clone()),
        };
        let mut p = Proxy::new(backend, temp_file("w.json"));
        p.enable("t", 1080).unwrap();
        assert_eq!(
            crate::winreg::get_string(&key, "ProxyServer").as_deref(),
            Some("socks=127.0.0.1:1080")
        );
        assert_eq!(crate::winreg::get_dword(&key, "ProxyEnable"), Some(1));
        p.restore().unwrap();
        assert_eq!(crate::winreg::get_string(&key, "ProxyServer"), None);
        assert_eq!(crate::winreg::get_dword(&key, "ProxyEnable"), Some(0));
        crate::winreg::delete(&key, "ProxyEnable").unwrap();
    }
}
