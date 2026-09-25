// No console window next to the GUI on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod models;
mod platform;
mod probe;
mod ssh_config;
mod store;
mod tunnel;

use std::sync::Arc;

use serde::Deserialize;
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
