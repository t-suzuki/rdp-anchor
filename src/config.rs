use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorDef {
    pub name: String,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayProfile {
    pub name: String,
    pub monitor_ids: Vec<String>,
    pub primary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostEntry {
    pub id: String,
    pub name: String,
    pub rdp_file: String,
    pub default_profile: String,
    #[serde(default)]
    pub color: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub monitors: HashMap<String, MonitorDef>,
    #[serde(default)]
    pub profiles: HashMap<String, DisplayProfile>,
    #[serde(default)]
    pub hosts: Vec<HostEntry>,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default)]
    pub minimize_on_connect: bool,
    #[serde(default)]
    pub demo_mode: bool,
    #[serde(default)]
    pub save_last_rdp: bool,
    #[serde(default)]
    pub relocate_to_primary: bool,
    #[serde(default)]
    pub remember_window_position: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_position: Option<SavedWindowPosition>,
}

fn default_language() -> String {
    "ja".to_string()
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            monitors: HashMap::new(),
            profiles: HashMap::new(),
            hosts: Vec::new(),
            language: default_language(),
            minimize_on_connect: false,
            demo_mode: false,
            save_last_rdp: false,
            relocate_to_primary: false,
            remember_window_position: false,
            window_position: None,
        }
    }
}

/// Window position saved relative to a monitor (ratio-based for DPI/layout resilience).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedWindowPosition {
    pub monitor_width: u32,
    pub monitor_height: u32,
    pub x_ratio: f64,
    pub y_ratio: f64,
    pub width_ratio: f64,
    pub height_ratio: f64,
}

impl AppConfig {
    pub fn config_dir() -> PathBuf {
        let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        base.join("rdp-anchor")
    }

    pub fn config_path() -> PathBuf {
        Self::config_dir().join("config.json")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        if path.exists() {
            let data = fs::read_to_string(&path).unwrap_or_default();
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            Self::default()
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let dir = Self::config_dir();
        fs::create_dir_all(&dir).map_err(|e| format!("Failed to create config dir: {e}"))?;
        let json =
            serde_json::to_string_pretty(self).map_err(|e| format!("Serialization error: {e}"))?;
        fs::write(Self::config_path(), json).map_err(|e| format!("Write error: {e}"))?;
        Ok(())
    }

}
