//! "Launch at login": a per-user login item that starts the app hidden in
//! the tray.
//!
//! - Linux:   `~/.config/autostart/ssh2socks.desktop`
//! - macOS:   `~/Library/LaunchAgents/io.github.cobraliu.ssh2socks.plist`
//! - Windows: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\ssh2socks`

use std::path::PathBuf;

/// Passed by the login item: start in the tray without showing the window.
pub const HIDDEN_ARG: &str = "--hidden";

/// The program to put in the login item. An AppImage runs from a temporary
/// mount, so use the image itself.
fn program() -> Result<PathBuf, String> {
    #[cfg(target_os = "linux")]
    if let Some(image) = std::env::var_os("APPIMAGE") {
        return Ok(PathBuf::from(image));
    }
    std::env::current_exe().map_err(|e| e.to_string())
}

pub fn started_hidden() -> bool {
    std::env::args().any(|a| a == HIDDEN_ARG)
}

/// Re-point an enabled login item at this copy of the app, in case it was
/// moved or updated to a new path.
pub fn refresh() {
    if is_enabled() {
        let _ = set(true);
    }
}

#[cfg(target_os = "linux")]
fn entry_file() -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join("autostart")
            .join("ssh2socks.desktop"),
    )
}

#[cfg(target_os = "linux")]
fn desktop_entry(program: &std::path::Path) -> String {
    // Desktop Entry spec: quote the path; `"`, `` ` ``, `$` and `\` are
    // escaped inside quotes, and `%` is doubled.
    let mut quoted = String::from('"');
    for c in program.to_string_lossy().chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            '%' => quoted.push_str("%%"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    format!(
        "[Desktop Entry]\nType=Application\nName=ssh2socks\nComment=SSH proxies and port forwarding\nExec={quoted} {HIDDEN_ARG}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
    )
}

#[cfg(target_os = "linux")]
pub fn is_enabled() -> bool {
    entry_file().is_some_and(|f| f.is_file())
}

#[cfg(target_os = "linux")]
pub fn set(enabled: bool) -> Result<(), String> {
    let file = entry_file().ok_or("no config directory")?;
    if !enabled {
        return match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        };
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&file, desktop_entry(&program()?)).map_err(|e| e.to_string())
}

#[cfg(target_os = "macos")]
fn entry_file() -> Option<PathBuf> {
    Some(
        dirs::home_dir()?
            .join("Library/LaunchAgents")
            .join("io.github.cobraliu.ssh2socks.plist"),
    )
}

#[cfg(target_os = "macos")]
fn launch_agent(program: &std::path::Path) -> String {
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>io.github.cobraliu.ssh2socks</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>{HIDDEN_ARG}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>ProcessType</key>
  <string>Interactive</string>
</dict>
</plist>
"#,
        escape(&program.to_string_lossy())
    )
}

#[cfg(target_os = "macos")]
pub fn is_enabled() -> bool {
    entry_file().is_some_and(|f| f.is_file())
}

#[cfg(target_os = "macos")]
pub fn set(enabled: bool) -> Result<(), String> {
    let file = entry_file().ok_or("no home directory")?;
    if !enabled {
        return match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        };
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&file, launch_agent(&program()?)).map_err(|e| e.to_string())
}

#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const RUN_VALUE: &str = "ssh2socks";

#[cfg(windows)]
pub fn is_enabled() -> bool {
    crate::winreg::get_string(RUN_KEY, RUN_VALUE).is_some()
}

#[cfg(windows)]
pub fn set(enabled: bool) -> Result<(), String> {
    if !enabled {
        return crate::winreg::delete(RUN_KEY, RUN_VALUE);
    }
    let command = format!("\"{}\" {HIDDEN_ARG}", program()?.display());
    crate::winreg::set_string(RUN_KEY, RUN_VALUE, &command)
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    #[test]
    fn desktop_entry_quotes_the_path() {
        let text = super::desktop_entry(std::path::Path::new("/opt/my \"app\"/100%/ssh2socks"));
        assert!(text.contains(r#"Exec="/opt/my \"app\"/100%%/ssh2socks" --hidden"#));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn launch_agent_escapes_the_path() {
        let text = super::launch_agent(std::path::Path::new("/Apps/A&B.app/Contents/MacOS/x"));
        assert!(text.contains("<string>/Apps/A&amp;B.app/Contents/MacOS/x</string>"));
        assert!(text.contains("<string>--hidden</string>"));
    }
}
