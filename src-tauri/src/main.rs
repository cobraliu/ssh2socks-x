// No console window next to the GUI on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
mod i18n;
mod keygen;
mod keys;
mod models;
mod platform;
mod probe;
mod progress;
#[cfg(unix)]
mod reaper;
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
        return Err(tr!(
            "请从列表中选择一个 ssh 连接。",
            "Choose an ssh host from the list."
        ));
    }
    if name.is_empty() {
        return Err(tr!("请填写隧道名称。", "Enter a tunnel name."));
    }
    if input.port == 0 || (input.kind != TunnelKind::Socks && input.remote_port == 0) {
        return Err(tr!(
            "端口必须在 1–65535 之间。",
            "Ports must be between 1 and 65535."
        ));
    }
    let target_host = match input.target_host.trim() {
        "" => "127.0.0.1".to_string(),
        h => h.trim_start_matches('[').trim_end_matches(']').to_string(),
    };
    if target_host.contains(char::is_whitespace) {
        return Err(tr!(
            "目标地址格式不正确。",
            "The target address is not valid."
        ));
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
    platform::open_url(&url)
        .map_err(|e| tr!("无法打开浏览器：{e}", "Could not open the browser: {e}"))?;
    Ok(url)
}

#[tauri::command]
fn tunnel_logs(mgr: Mgr, id: String) -> Vec<String> {
    mgr.logs(&id)
}

// ---- ssh keys + config ----------------------------------------------------

fn no_home() -> String {
    tr!("找不到用户主目录", "Could not find the home directory")
}

#[tauri::command]
fn list_keys() -> Vec<keys::KeyInfo> {
    keys::list_keys()
}

#[derive(Serialize)]
struct KeyDefaults {
    name: String,
    comment: String,
}

/// Unused file name for a new key (`id_ed25519`, `id_rsa_2`…) and the
/// usual `user@host` comment.
#[tauri::command]
fn key_defaults(kind: String) -> Result<KeyDefaults, String> {
    let dir = ssh_config::ssh_dir().ok_or_else(no_home)?;
    Ok(KeyDefaults {
        name: keygen::suggest_name(
            &dir,
            if kind == "rsa" {
                "id_rsa"
            } else {
                "id_ed25519"
            },
        ),
        comment: keygen::default_comment(),
    })
}

/// Key generation and the import checks (bcrypt, RSA) take a while, so
/// they run off the main thread.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce(&std::path::Path) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let dir = ssh_config::ssh_dir().ok_or_else(no_home)?;
    tauri::async_runtime::spawn_blocking(move || f(&dir))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn generate_key(input: keygen::GenerateInput) -> Result<keys::KeyInfo, String> {
    blocking(move |dir| keygen::generate(dir, &input)).await
}

#[tauri::command]
async fn check_key_import(input: keygen::ImportInput) -> Result<keygen::ImportReport, String> {
    blocking(move |dir| Ok(keygen::check_import(dir, &input))).await
}

#[tauri::command]
async fn import_key(input: keygen::ImportInput) -> Result<keys::KeyInfo, String> {
    blocking(move |dir| keygen::import(dir, &input)).await
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
    let config = ssh_edit::main_config().ok_or_else(no_home)?;
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
            output: tr!(
                "30 秒内没有完成登录（网络不通，或 ProxyCommand / 跳板机卡住）",
                "Login did not finish within 30 seconds (network unreachable, or the ProxyCommand / jump host is stuck)"
            ),
        },
        Ok(Err(e)) => TestResult {
            ok: false,
            output: tr!("无法启动 ssh：{e}", "Could not start ssh: {e}"),
        },
        Ok(Ok(out)) => {
            let text = platform::decode(&out.stderr);
            let ok = out.status.success();
            TestResult {
                ok,
                output: match (ok, text.trim()) {
                    (true, "") => tr!("登录成功", "Login succeeded"),
                    (_, t) => t.to_string(),
                },
            }
        }
    }
}

#[tauri::command]
fn open_ssh_config() -> Result<(), String> {
    let path = ssh_edit::main_config().ok_or_else(no_home)?;
    if !path.exists() {
        return Err(tr!(
            "~/.ssh/config 还不存在，先添加一个主机即可创建。",
            "~/.ssh/config does not exist yet. Add a host to create it."
        ));
    }
    platform::open_in_editor(&path)
        .map_err(|e| tr!("无法打开编辑器：{e}", "Could not open an editor: {e}"))
}

// ---- language + theme -------------------------------------------------------

#[tauri::command]
fn get_prefs() -> i18n::Prefs {
    i18n::load_prefs()
}

#[tauri::command]
fn set_prefs(app: AppHandle, prefs: i18n::Prefs) -> Result<(), String> {
    apply_prefs(&app, &prefs);
    i18n::save_prefs(&prefs)
}

fn apply_prefs(app: &AppHandle, prefs: &i18n::Prefs) {
    if !prefs.lang.is_empty() {
        i18n::set_english(prefs.lang == "en");
    }
    relabel_tray(app);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_theme(match prefs.theme.as_str() {
            "light" => Some(tauri::Theme::Light),
            "dark" => Some(tauri::Theme::Dark),
            _ => None,
        });
    }
}

// ---- tray + window ------------------------------------------------------

pub fn update_tray_tooltip(app: &AppHandle, mgr: &Manager) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let (connected, total) = mgr.connected_summary();
        let _ = tray.set_tooltip(Some(tr!(
            "ssh2socks — {connected}/{total} 已连接",
            "ssh2socks — {connected}/{total} connected"
        )));
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Tray menu entries, kept so their text can follow the language.
struct TrayMenu([MenuItem<tauri::Wry>; 4]);

fn tray_labels() -> [String; 4] {
    [
        tr!("显示主界面", "Show window"),
        tr!("全部启动", "Start all"),
        tr!("全部停止", "Stop all"),
        tr!("退出", "Quit"),
    ]
}

fn relabel_tray(app: &AppHandle) {
    if let Some(menu) = app.try_state::<TrayMenu>() {
        for (item, label) in menu.0.iter().zip(tray_labels()) {
            let _ = item.set_text(label);
        }
    }
    update_tray_tooltip(app, &app.state::<Arc<Manager>>());
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let [show, start, stop, quit] = tray_labels();
    let show = MenuItem::with_id(app, "show", show, true, None::<&str>)?;
    let start = MenuItem::with_id(app, "start_all", start, true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop_all", stop, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", quit, true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show, &sep1, &start, &stop, &sep2, &quit])?;
    app.manage(TrayMenu([
        show.clone(),
        start.clone(),
        stop.clone(),
        quit.clone(),
    ]));

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

/// Treat SIGTERM / SIGINT / SIGHUP (logout, `kill`, Ctrl-C) like Quit, so
/// the tunnels are stopped instead of left running.
#[cfg(unix)]
fn quit_on_signals(app: AppHandle) {
    use tokio::signal::unix::{signal, SignalKind};
    tauri::async_runtime::spawn(async move {
        let (Ok(mut term), Ok(mut int), Ok(mut hup)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
            signal(SignalKind::hangup()),
        ) else {
            return;
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
            _ = hup.recv() => {}
        }
        app.state::<Arc<Manager>>().kill_all_now();
        app.exit(0);
    });
}

fn main() {
    let app = tauri::Builder::default()
        .setup(|app| {
            #[cfg(unix)]
            {
                let n = reaper::init(&store::config_dir());
                if n > 0 {
                    eprintln!("ssh2socks: stopped {n} ssh process(es) left by an earlier run");
                }
                quit_on_signals(app.handle().clone());
            }
            let mgr = Manager::new(Some(app.handle().clone()), store::load());
            app.manage(mgr);
            let prefs = i18n::load_prefs();
            if !prefs.lang.is_empty() {
                i18n::set_english(prefs.lang == "en");
            }
            let tray_ok = build_tray(app).is_ok();
            app.manage(TrayReady(tray_ok));
            apply_prefs(app.handle(), &prefs);
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
            key_defaults,
            generate_key,
            check_key_import,
            import_key,
            list_ssh_hosts,
            save_ssh_host,
            delete_ssh_host,
            test_ssh_host,
            open_ssh_config,
            get_prefs,
            set_prefs,
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
