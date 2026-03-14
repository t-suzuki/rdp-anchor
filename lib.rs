mod config;
mod monitor;
mod rdp;
mod session;

use config::{AppConfig, DisplayProfile, HostEntry, MonitorDef};
use monitor::LiveMonitor;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use tauri::State;

struct AppState {
    config: Mutex<AppConfig>,
}

// ─── Data types for frontend ────────────────────────────────────────

#[derive(Serialize)]
struct FullState {
    config: AppConfig,
    monitors: Vec<LiveMonitor>,
    active_sessions: Vec<session::ActiveSession>,
}

#[derive(Serialize)]
struct ConnectResult {
    success: bool,
    message: String,
    /// true if a session was already active and user needs to confirm
    needs_confirm: bool,
    host: String,
}

// ─── Commands ───────────────────────────────────────────────────────

/// Get everything the frontend needs in one call (fast startup)
#[tauri::command]
fn get_state(state: State<AppState>) -> Result<FullState, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let monitors = monitor::get_current_monitors().unwrap_or_default();
    let active_sessions = session::get_active_sessions();

    Ok(FullState {
        config: config.clone(),
        monitors,
        active_sessions,
    })
}

/// Refresh just the live monitor list
#[tauri::command]
fn refresh_monitors() -> Result<Vec<LiveMonitor>, String> {
    monitor::get_current_monitors()
}

/// Refresh active sessions
#[tauri::command]
fn refresh_sessions() -> Vec<session::ActiveSession> {
    session::get_active_sessions()
}

/// Auto-detect monitors and generate definitions
#[tauri::command]
fn auto_detect_monitors(state: State<AppState>) -> Result<HashMap<String, MonitorDef>, String> {
    let live = monitor::get_current_monitors()?;
    let defs = monitor::auto_detect_defs(&live);

    // Save to config
    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    config.monitors = defs.clone();
    config.save()?;

    Ok(defs)
}

/// Save monitor definitions
#[tauri::command]
fn save_monitors(
    state: State<AppState>,
    monitors: HashMap<String, MonitorDef>,
) -> Result<(), String> {
    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    config.monitors = monitors;
    config.save()
}

/// Save a display profile
#[tauri::command]
fn save_profile(
    state: State<AppState>,
    id: String,
    profile: DisplayProfile,
) -> Result<(), String> {
    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    config.profiles.insert(id, profile);
    config.save()
}

/// Delete a display profile
#[tauri::command]
fn delete_profile(state: State<AppState>, id: String) -> Result<(), String> {
    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    config.profiles.remove(&id);
    config.save()
}

/// Add or update a host entry
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

/// Delete a host entry
#[tauri::command]
fn delete_host(state: State<AppState>, id: String) -> Result<(), String> {
    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    config.hosts.retain(|h| h.id != id);
    config.save()
}

/// Save complete config at once (for bulk edits)
#[tauri::command]
fn save_config(state: State<AppState>, new_config: AppConfig) -> Result<(), String> {
    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    *config = new_config;
    config.save()
}

/// Pre-flight check: resolve the profile and check for conflicts
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

    // Resolve monitor IDs
    let live = monitor::get_current_monitors()?;
    let _selected = monitor::resolve_profile(&config, profile, &live)?;

    // Check for active session
    let rdp_host = rdp::read_rdp_host(&host.rdp_file).unwrap_or_default();
    let is_connected = session::is_host_connected(&rdp_host);

    Ok(ConnectResult {
        success: true,
        message: if is_connected {
            format!(
                "{}({})に既に接続中です。再接続すると既存のセッションが切断されます。",
                host.name, rdp_host
            )
        } else {
            String::new()
        },
        needs_confirm: is_connected,
        host: rdp_host,
    })
}

/// Actually connect: resolve monitors → patch .rdp → launch mstsc
#[tauri::command]
fn connect(
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

    // Resolve monitor IDs
    let live = monitor::get_current_monitors()?;
    let selected = monitor::resolve_profile(&config, profile, &live)?;

    // Patch .rdp and get temp file path
    let launch_rdp = rdp::prepare_rdp_for_launch(&host.rdp_file, &selected)?;

    // Launch mstsc
    std::process::Command::new("mstsc.exe")
        .arg(&launch_rdp)
        .spawn()
        .map_err(|e| format!("Failed to launch mstsc: {e}"))?;

    Ok(format!(
        "接続開始: {} (monitors: {})",
        host.name, selected
    ))
}

/// Browse for an .rdp file using a system file dialog (via PowerShell)
#[tauri::command]
fn browse_rdp_file() -> Result<Option<rdp::RdpInfo>, String> {
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            r#"
            Add-Type -AssemblyName System.Windows.Forms
            $d = New-Object System.Windows.Forms.OpenFileDialog
            $d.Filter = 'RDP Files (*.rdp)|*.rdp|All Files (*.*)|*.*'
            $d.Title = 'RDPファイルを選択'
            if ($d.ShowDialog() -eq 'OK') { Write-Output $d.FileName }
            "#,
        ])
        .output()
        .map_err(|e| format!("File dialog error: {e}"))?;

    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return Ok(None);
    }
    rdp::read_rdp_info(&path).map(Some)
}

// ─── App entry ──────────────────────────────────────────────────────

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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
