// No console window next to the GUI on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod keys;
mod models;
mod platform;
mod probe;
mod ssh_config;
mod ssh_edit;
mod store;
mod tunnel;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager as _, RunEvent, State, WindowEvent};

use models::{Tunnel, TunnelKind, TunnelView, DEFAULT_PROBE_URL};
use ssh_config::HostEntry;
use tunnel::Manager;

const TRAY_ID: &str = "main";

type Mgr<'a> = State<'a, Arc<Manager>>;

/// Whether a tray icon exists; without one, closing the window quits.
struct TrayReady(bool);

// ---- commands -----------------------------------------------------------

#[derive(Deserialize)]
struct TunnelInput {
    id: Option<String>,
    name: String,
    host: String,
    kind: TunnelKind,
    port: u16,
    remote_port: u16,
    target_host: String,
    probe_url: String,
    auto_reconnect: bool,
}

#[tauri::command]
fn list_tunnels(mgr: Mgr) -> Vec<TunnelView> {
    mgr.views()
}

#[tauri::command]
fn list_hosts() -> Vec<HostEntry> {
    ssh_config::load_hosts()
}

#[tauri::command]
fn suggest_port(mgr: Mgr, start: u16) -> u16 {
    mgr.suggest_port(start)
}

#[tauri::command]
fn default_probe_url() -> &'static str {
    DEFAULT_PROBE_URL
}

#[tauri::command]
fn save_tunnel(mgr: Mgr, input: TunnelInput) -> Result<(), String> {
    let name = input.name.trim().to_string();
    let host = input.host.trim().to_string();
    if host.is_empty() {
        return Err("请从列表中选择一个 ssh 连接。".into());
    }
    if name.is_empty() {
        return Err("请填写隧道名称。".into());
    }
    if input.port == 0 || (input.kind != TunnelKind::Socks && input.remote_port == 0) {
        return Err("端口必须在 1–65535 之间。".into());
    }
    let target_host = match input.target_host.trim() {
        "" => "127.0.0.1".to_string(),
        h => h.trim_start_matches('[').trim_end_matches(']').to_string(),
    };
    if target_host.contains(char::is_whitespace) {
        return Err("目标地址格式不正确。".into());
    }
    let probe_url = match input.probe_url.trim() {
        "" => DEFAULT_PROBE_URL.to_string(),
        url => url.to_string(),
    };
    mgr.upsert(Tunnel {
        name,
        host,
        kind: input.kind,
        port: input.port,
        remote_port: input.remote_port,
        target_host,
        probe_url,
        auto_reconnect: input.auto_reconnect,
        id: input.id.unwrap_or_else(models::new_id),
    })
}

#[tauri::command]
fn delete_tunnel(mgr: Mgr, id: String) -> Result<(), String> {
    mgr.remove(&id)
}

#[tauri::command]
fn start_tunnel(mgr: Mgr, id: String) {
    mgr.start(&id);
}

#[tauri::command]
fn stop_tunnel(mgr: Mgr, id: String) {
    mgr.stop(&id);
}

#[tauri::command]
fn start_all(mgr: Mgr) {
    mgr.start_all();
}

#[tauri::command]
fn stop_all(mgr: Mgr) {
    mgr.stop_all();
}

#[tauri::command]
async fn open_in_browser(mgr: Mgr<'_>, id: String) -> Result<String, String> {
    let url = mgr.browser_url(&id).await?;
    platform::open_url(&url).map_err(|e| format!("无法打开浏览器：{e}"))?;
    Ok(url)
}

#[tauri::command]
fn tunnel_logs(mgr: Mgr, id: String) -> Vec<String> {
    mgr.logs(&id)
}

// ---- ssh keys + config ----------------------------------------------------

#[tauri::command]
fn list_keys() -> Vec<keys::KeyInfo> {
    keys::list_keys()
}

#[derive(Serialize)]
struct SshConfigView {
    path: String,
    blocks: Vec<ssh_edit::HostBlock>,
}

#[tauri::command]
fn list_ssh_hosts() -> SshConfigView {
    SshConfigView {
        path: ssh_edit::main_config()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        blocks: ssh_edit::list_blocks(),
    }
}

#[tauri::command]
fn save_ssh_host(input: ssh_edit::HostInput) -> Result<ssh_edit::HostBlock, String> {
    let config = ssh_edit::main_config().ok_or("找不到用户主目录")?;
    ssh_edit::save_block(&config, &input)
}

#[tauri::command]
fn delete_ssh_host(file: String, line: usize, patterns: String) -> Result<(), String> {
    ssh_edit::delete_block(&file, line, &patterns)
}

#[derive(Serialize)]
struct TestResult {
    ok: bool,
    output: String,
}

/// Log in once (no command, no tty) to check the host, keys and proxy setup.
#[tauri::command]
async fn test_ssh_host(alias: String) -> TestResult {
    let mut cmd = tokio::process::Command::new("ssh");
    cmd.args([
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "StrictHostKeyChecking=accept-new",
        &alias,
        "exit",
    ])
    .stdin(std::process::Stdio::null())
    .kill_on_drop(true);
    platform::hide_console(&mut cmd);
    match tokio::time::timeout(std::time::Duration::from_secs(30), cmd.output()).await {
        Err(_) => TestResult {
            ok: false,
            output: "30 秒内没有完成登录（网络不通，或 ProxyCommand / 跳板机卡住）".into(),
        },
        Ok(Err(e)) => TestResult {
            ok: false,
            output: format!("无法启动 ssh：{e}"),
        },
        Ok(Ok(out)) => {
            let text = platform::decode(&out.stderr);
            let ok = out.status.success();
            TestResult {
                ok,
                output: match (ok, text.trim()) {
                    (true, "") => "登录成功".into(),
                    (_, t) => t.to_string(),
                },
            }
        }
    }
}

#[tauri::command]
fn open_ssh_config() -> Result<(), String> {
    let path = ssh_edit::main_config().ok_or("找不到用户主目录")?;
    if !path.exists() {
        return Err("~/.ssh/config 还不存在，先添加一个主机即可创建。".into());
    }
    platform::open_in_editor(&path).map_err(|e| format!("无法打开编辑器：{e}"))
}

// ---- tray + window ------------------------------------------------------

pub fn update_tray_tooltip(app: &AppHandle, mgr: &Manager) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let (connected, total) = mgr.connected_summary();
        let _ = tray.set_tooltip(Some(format!("ssh2socks — {connected}/{total} 已连接")));
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示主界面", true, None::<&str>)?;
    let start = MenuItem::with_id(app, "start_all", "全部启动", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop_all", "全部停止", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show, &sep1, &start, &stop, &sep2, &quit])?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("ssh2socks")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            let mgr = app.state::<Arc<Manager>>();
            match event.id().as_ref() {
                "show" => show_main_window(app),
                "start_all" => mgr.start_all(),
                "stop_all" => mgr.stop_all(),
                "quit" => {
                    mgr.kill_all_now();
                    app.exit(0);
                }
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

fn main() {
    let app = tauri::Builder::default()
        .setup(|app| {
            let mgr = Manager::new(Some(app.handle().clone()), store::load());
            app.manage(mgr);
            let tray_ok = build_tray(app).is_ok();
            app.manage(TrayReady(tray_ok));
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Keep tunnels running in the tray instead of quitting.
                if window.app_handle().state::<TrayReady>().0 {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_tunnels,
            list_hosts,
            suggest_port,
            default_probe_url,
            save_tunnel,
            delete_tunnel,
            start_tunnel,
            stop_tunnel,
            start_all,
            stop_all,
            tunnel_logs,
            open_in_browser,
            list_keys,
            list_ssh_hosts,
            save_ssh_host,
            delete_ssh_host,
            test_ssh_host,
            open_ssh_config,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build ssh2socks");

    app.run(|app, event| match event {
        RunEvent::Exit => app.state::<Arc<Manager>>().kill_all_now(),
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => show_main_window(app),
        _ => {}
    });
}
