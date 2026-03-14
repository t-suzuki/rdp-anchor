use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// Physical monitor definition — identified by position & resolution, not volatile ID
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorDef {
    pub name: String,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

/// A named display profile: which monitors to use and which is primary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayProfile {
    pub name: String,
    /// Keys into AppConfig::monitors
    pub monitor_ids: Vec<String>,
    /// Key into AppConfig::monitors — first in selectedmonitors (gets taskbar)
    pub primary: String,
}

/// A host (RDP target) with its base .rdp file and default display profile
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostEntry {
    pub id: String,
    pub name: String,
    pub rdp_file: String,
    pub default_profile: String,
    /// Optional color tag for the UI card
    #[serde(default)]
    pub color: String,
}

/// Top-level config persisted as JSON
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Physical monitors keyed by stable user-chosen ID (e.g. "left-fhd")
    #[serde(default)]
    pub monitors: HashMap<String, MonitorDef>,
    /// Display profiles keyed by ID
    #[serde(default)]
    pub profiles: HashMap<String, DisplayProfile>,
    /// RDP host entries
    #[serde(default)]
    pub hosts: Vec<HostEntry>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            monitors: HashMap::new(),
            profiles: HashMap::new(),
            hosts: Vec::new(),
        }
    }
}

impl AppConfig {
    pub fn config_dir() -> PathBuf {
        let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
        base.join("rdp-launcher")
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
