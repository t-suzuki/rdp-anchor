use std::fs;
use std::path::Path;

pub fn read_rdp_host(path: &str) -> Result<String, String> {
    let content = read_rdp_file(path)?;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("full address:s:") {
            let host = rest.split(':').next().unwrap_or(rest);
            return Ok(host.to_string());
        }
    }
    Err(format!("No 'full address' found in {}", path))
}

pub fn prepare_rdp_for_launch(rdp_path: &str, selected_monitors: &str) -> Result<String, String> {
    let content = read_rdp_file(rdp_path)?;

    let mut lines: Vec<String> = Vec::new();
    let mut has_multimon = false;
    let mut has_screen_mode = false;
    let mut has_selected = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("selectedmonitors:") {
            lines.push(format!("selectedmonitors:s:{}", selected_monitors));
            has_selected = true;
        } else if trimmed.starts_with("use multimon:") {
            lines.push("use multimon:i:1".to_string());
            has_multimon = true;
        } else if trimmed.starts_with("screen mode id:") {
            lines.push("screen mode id:i:2".to_string());
            has_screen_mode = true;
        } else {
            lines.push(line.to_string());
        }
    }

    if !has_multimon {
        lines.push("use multimon:i:1".to_string());
    }
    if !has_screen_mode {
        lines.push("screen mode id:i:2".to_string());
    }
    if !has_selected {
        lines.push(format!("selectedmonitors:s:{}", selected_monitors));
    }

    let original = Path::new(rdp_path);
    let stem = original.file_stem().unwrap_or_default().to_string_lossy();
    let dir = original.parent().unwrap_or_else(|| Path::new("."));
    let temp_path = dir.join(format!("{}_launch.rdp", stem));

    let output = lines.join("\r\n");
    fs::write(&temp_path, output.as_bytes())
        .map_err(|e| format!("Failed to write temp .rdp: {e}"))?;

    Ok(temp_path.to_string_lossy().to_string())
}

fn read_rdp_file(path: &str) -> Result<String, String> {
    let raw = fs::read(path).map_err(|e| format!("Cannot read {}: {}", path, e))?;

    if raw.len() >= 2 && raw[0] == 0xFF && raw[1] == 0xFE {
        let (decoded, _, _) = encoding_rs::UTF_16LE.decode(&raw[2..]);
        Ok(decoded.into_owned())
    } else if raw.len() >= 3 && raw[0] == 0xEF && raw[1] == 0xBB && raw[2] == 0xBF {
        String::from_utf8(raw[3..].to_vec()).map_err(|e| format!("UTF-8 decode error: {e}"))
    } else {
        Ok(String::from_utf8_lossy(&raw).into_owned())
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RdpInfo {
    pub host: String,
    pub username: Option<String>,
    pub port: u16,
    pub path: String,
}

pub fn read_rdp_info(path: &str) -> Result<RdpInfo, String> {
    let content = read_rdp_file(path)?;
    let mut host = String::new();
    let mut username = None;
    let mut port = 3389u16;

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(addr) = trimmed.strip_prefix("full address:s:") {
            let parts: Vec<&str> = addr.splitn(2, ':').collect();
            host = parts[0].to_string();
            if parts.len() > 1 {
                port = parts[1].parse().unwrap_or(3389);
            }
        } else if let Some(user) = trimmed.strip_prefix("username:s:") {
            if !user.is_empty() {
                username = Some(user.to_string());
            }
        }
    }

    if host.is_empty() {
        return Err(format!("No host found in {}", path));
    }

    Ok(RdpInfo {
        host,
        username,
        port,
        path: path.to_string(),
    })
}
