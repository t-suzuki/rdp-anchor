mod config;
mod monitor;
mod rdp;
mod session;

use config::{AppConfig, DisplayProfile, HostEntry, MonitorDef, SavedWindowPosition};
use monitor::LiveMonitor;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use tauri::{Manager, State};

struct AppState {
    config: Mutex<AppConfig>,
}

#[derive(Serialize)]
struct FullState {
    config: AppConfig,
    monitors: Vec<LiveMonitor>,
    active_sessions: Vec<session::ActiveSession>,
    mstsc_id_fallback: bool,
}

#[derive(Serialize)]
struct ConnectResult {
    success: bool,
    message: String,
    needs_confirm: bool,
    host: String,
    host_name: String,
}

#[tauri::command]
fn get_state(state: State<AppState>) -> Result<FullState, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let (monitors, mstsc_id_fallback) = monitor::get_monitors_for_connect()
        .unwrap_or_else(|_| (monitor::get_current_monitors().unwrap_or_default(), true));
    let active_sessions = session::get_active_sessions();

    Ok(FullState {
        config: config.clone(),
        monitors,
        active_sessions,
        mstsc_id_fallback,
    })
}

#[tauri::command]
fn refresh_monitors() -> Result<Vec<LiveMonitor>, String> {
    monitor::get_current_monitors()
}

#[tauri::command]
fn refresh_sessions() -> Vec<session::ActiveSession> {
    session::get_active_sessions()
}

#[tauri::command]
fn auto_detect_monitors(state: State<AppState>) -> Result<HashMap<String, MonitorDef>, String> {
    let live = monitor::get_current_monitors()?;
    let defs = monitor::auto_detect_defs(&live);

    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    config.monitors = defs.clone();
    config.save()?;

    Ok(defs)
}

#[tauri::command]
fn save_monitors(
    state: State<AppState>,
    monitors: HashMap<String, MonitorDef>,
) -> Result<(), String> {
    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    config.monitors = monitors;
    config.save()
}

#[tauri::command]
fn save_profile(state: State<AppState>, id: String, profile: DisplayProfile) -> Result<(), String> {
    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    config.profiles.insert(id, profile);
    config.save()
}

#[tauri::command]
fn delete_profile(state: State<AppState>, id: String) -> Result<(), String> {
    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    config.profiles.remove(&id);
    config.save()
}

#[tauri::command]
fn save_host(state: State<AppState>, host: HostEntry) -> Result<(), String> {
    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    if let Some(existing) = config.hosts.iter_mut().find(|h| h.id == host.id) {
        *existing = host;
    } else {
        config.hosts.push(host);
    }
    config.save()
}

#[tauri::command]
fn delete_host(state: State<AppState>, id: String) -> Result<(), String> {
    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    config.hosts.retain(|h| h.id != id);
    config.save()
}

#[tauri::command]
fn save_config(state: State<AppState>, new_config: AppConfig) -> Result<(), String> {
    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    *config = new_config;
    config.save()
}

#[tauri::command]
fn preflight_connect(
    state: State<AppState>,
    host_id: String,
    profile_id: Option<String>,
) -> Result<ConnectResult, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;

    let host = config
        .hosts
        .iter()
        .find(|h| h.id == host_id)
        .ok_or("Host not found")?;

    let profile_key = profile_id.as_deref().unwrap_or(&host.default_profile);
    let profile = config
        .profiles
        .get(profile_key)
        .ok_or_else(|| format!("Profile '{}' not found", profile_key))?;

    let (live, _fallback) = monitor::get_monitors_for_connect()?;
    let _resolved = monitor::resolve_profile(&config, profile, &live)?;

    let rdp_host = rdp::read_rdp_host(&host.rdp_file).unwrap_or_default();
    let is_connected = session::is_host_connected(&rdp_host);

    Ok(ConnectResult {
        success: true,
        message: String::new(),
        needs_confirm: is_connected,
        host: rdp_host,
        host_name: host.name.clone(),
    })
}

#[tauri::command]
fn connect(
    window: tauri::Window,
    state: State<AppState>,
    host_id: String,
    profile_id: Option<String>,
) -> Result<String, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;

    let host = config
        .hosts
        .iter()
        .find(|h| h.id == host_id)
        .ok_or("Host not found")?;

    let profile_key = profile_id.as_deref().unwrap_or(&host.default_profile);
    let profile = config
        .profiles
        .get(profile_key)
        .ok_or_else(|| format!("Profile '{}' not found", profile_key))?;

    let (live, _fallback) = monitor::get_monitors_for_connect()?;
    let resolved = monitor::resolve_profile(&config, profile, &live)?;

    let launch_rdp =
        rdp::prepare_rdp_for_launch(&host.rdp_file, &resolved.selected_monitors)?;

    if config.save_last_rdp {
        let dest = config::AppConfig::config_dir().join("last_launch.rdp");
        let _ = std::fs::copy(&launch_rdp, &dest);
    }

    let mut cmd = std::process::Command::new("mstsc.exe");
    cmd.arg(&launch_rdp);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd.spawn()
        .map_err(|e| format!("Failed to launch mstsc: {e}"))?;

    // Background thread: wait for fullscreen, then relocate to profile's primary monitor
    #[cfg(target_os = "windows")]
    if config.relocate_to_primary {
        let target_left = resolved.primary_left;
        let target_top = resolved.primary_top;
        std::thread::spawn(move || {
            session::relocate_mstsc_to_monitor(target_left, target_top);
        });
    }

    if config.minimize_on_connect {
        let _ = window.minimize();
    }

    Ok(format!("{}|{}", host.name, resolved.selected_monitors))
}

#[tauri::command]
fn browse_rdp_file() -> Result<Option<rdp::RdpInfo>, String> {
    let mut cmd = std::process::Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-Command",
        r#"
        Add-Type -AssemblyName System.Windows.Forms
        $d = New-Object System.Windows.Forms.OpenFileDialog
        $d.Filter = 'RDP Files (*.rdp)|*.rdp|All Files (*.*)|*.*'
        $d.Title = 'Select RDP File'
        if ($d.ShowDialog() -eq 'OK') { Write-Output $d.FileName }
        "#,
    ]);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let output = cmd
        .output()
        .map_err(|e| format!("File dialog error: {e}"))?;

    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return Ok(None);
    }
    rdp::read_rdp_info(&path).map(Some)
}

#[tauri::command]
fn import_rdp(path: String) -> Result<rdp::RdpInfo, String> {
    rdp::read_rdp_info(&path)
}

#[tauri::command]
fn test_mstsc_capture() -> Result<monitor::CaptureResult, String> {
    monitor::test_mstsc_capture()
}

#[tauri::command]
fn test_hook_basic() -> Result<monitor::CaptureResult, String> {
    monitor::test_hook_basic()
}

#[tauri::command]
fn diagnose_mstsc() -> Result<monitor::CaptureResult, String> {
    monitor::diagnose_mstsc()
}

#[tauri::command]
fn show_window(window: tauri::Window) {
    let _ = window.show();
}

#[tauri::command]
fn is_debug_build() -> bool {
    cfg!(debug_assertions)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let config = AppConfig::load();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppState {
            config: Mutex::new(config),
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            refresh_monitors,
            refresh_sessions,
            auto_detect_monitors,
            save_monitors,
            save_profile,
            delete_profile,
            save_host,
            delete_host,
            save_config,
            preflight_connect,
            connect,
            browse_rdp_file,
            import_rdp,
            test_mstsc_capture,
            test_hook_basic,
            diagnose_mstsc,
            show_window,
            is_debug_build,
        ])
        .setup(|app| {
            let state = app.state::<AppState>();
            let config = state.config.lock().unwrap();
            let should_restore = config.remember_window_position;
            let saved = config.window_position.clone();
            drop(config);

            if let Some(win) = app.get_webview_window("main") {
                if should_restore {
                    if let Some(saved) = saved {
                        if let Some((x, y, w, h)) = resolve_saved_position(&saved) {
                            let _ = win.set_position(
                                tauri::PhysicalPosition::<i32>::new(x, y),
                            );
                            let _ = win.set_size(
                                tauri::PhysicalSize::<u32>::new(w, h),
                            );
                        }
                    }
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                let state = window.state::<AppState>();
                let mut config = state.config.lock().unwrap();
                if config.remember_window_position {
                    if let (Ok(pos), Ok(size)) =
                        (window.outer_position(), window.inner_size())
                    {
                        config.window_position =
                            compute_window_position(pos, size);
                        let _ = config.save();
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Compute window position as ratios relative to the containing monitor.
fn compute_window_position(
    pos: tauri::PhysicalPosition<i32>,
    size: tauri::PhysicalSize<u32>,
) -> Option<SavedWindowPosition> {
    let monitors = monitor::get_current_monitors().unwrap_or_default();
    let center_x = pos.x + size.width as i32 / 2;
    let center_y = pos.y + size.height as i32 / 2;

    let mon = monitors.iter().find(|m| {
        center_x >= m.left
            && center_x < m.left + m.width as i32
            && center_y >= m.top
            && center_y < m.top + m.height as i32
    })?;

    Some(SavedWindowPosition {
        monitor_width: mon.width,
        monitor_height: mon.height,
        x_ratio: (pos.x - mon.left) as f64 / mon.width as f64,
        y_ratio: (pos.y - mon.top) as f64 / mon.height as f64,
        width_ratio: size.width as f64 / mon.width as f64,
        height_ratio: size.height as f64 / mon.height as f64,
    })
}

/// Resolve a saved position to absolute coordinates. Returns None if no matching
/// monitor exists or the window wouldn't fit entirely on any monitor.
fn resolve_saved_position(saved: &SavedWindowPosition) -> Option<(i32, i32, u32, u32)> {
    let monitors = monitor::get_current_monitors().ok()?;

    // Find a monitor with matching resolution
    let mon = monitors
        .iter()
        .find(|m| m.width == saved.monitor_width && m.height == saved.monitor_height)?;

    let x = mon.left + (saved.x_ratio * mon.width as f64) as i32;
    let y = mon.top + (saved.y_ratio * mon.height as f64) as i32;
    let w = (saved.width_ratio * mon.width as f64).max(200.0) as u32;
    let h = (saved.height_ratio * mon.height as f64).max(200.0) as u32;

    // Window must fit entirely within at least one monitor
    let fits = monitors.iter().any(|m| {
        x >= m.left
            && y >= m.top
            && x + w as i32 <= m.left + m.width as i32
            && y + h as i32 <= m.top + m.height as i32
    });

    if fits {
        Some((x, y, w, h))
    } else {
        None
    }
}
