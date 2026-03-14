use crate::config::{AppConfig, DisplayProfile, MonitorDef};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveMonitor {
    pub mstsc_id: u32,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
}

pub fn get_current_monitors() -> Result<Vec<LiveMonitor>, String> {
    match capture_mstsc_l() {
        Ok(monitors) if !monitors.is_empty() => Ok(monitors),
        _ => enumerate_display_monitors(),
    }
}

fn capture_mstsc_l() -> Result<Vec<LiveMonitor>, String> {
    #[cfg(not(target_os = "windows"))]
    {
        Err("mstsc is only available on Windows".into())
    }

    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        use std::time::{Duration, Instant};
        use windows::Win32::Foundation::*;
        use windows::Win32::UI::WindowsAndMessaging::*;

        let child = Command::new("mstsc.exe")
            .arg("/l")
            .spawn()
            .map_err(|e| format!("Failed to spawn mstsc: {e}"))?;
        let pid = child.id();

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut dialog_hwnd: Option<HWND> = None;

        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
            dialog_hwnd = find_dialog_by_pid(pid);
            if dialog_hwnd.is_some() {
                break;
            }
        }

        let hwnd = dialog_hwnd.ok_or("mstsc /l dialog not found")?;
        let text = read_dialog_static_text(hwnd);

        unsafe {
            let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        }

        if text.is_empty() {
            return Err("Could not read mstsc /l dialog text".into());
        }

        parse_mstsc_output(&text)
    }
}

#[cfg(target_os = "windows")]
fn find_dialog_by_pid(target_pid: u32) -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    struct SearchData {
        target_pid: u32,
        found: Option<HWND>,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let data = &mut *(lparam.0 as *mut SearchData);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == data.target_pid && IsWindowVisible(hwnd).as_bool() {
            let mut class_buf = [0u16; 64];
            let len = GetClassNameW(hwnd, &mut class_buf) as usize;
            let class = String::from_utf16_lossy(&class_buf[..len]);
            if class == "#32770" {
                data.found = Some(hwnd);
                return FALSE;
            }
        }
        TRUE
    }

    let mut data = SearchData {
        target_pid,
        found: None,
    };
    unsafe {
        let _ = EnumWindows(
            Some(callback),
            LPARAM(&mut data as *mut SearchData as isize),
        );
    }
    data.found
}

#[cfg(target_os = "windows")]
fn read_dialog_static_text(dialog: windows::Win32::Foundation::HWND) -> String {
    use windows::Win32::Foundation::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    struct TextData {
        result: String,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let data = &mut *(lparam.0 as *mut TextData);
        let mut class_buf = [0u16; 64];
        let len = GetClassNameW(hwnd, &mut class_buf) as usize;
        let class = String::from_utf16_lossy(&class_buf[..len]);

        if class == "Static" {
            let text_len = GetWindowTextLengthW(hwnd) as usize;
            if text_len > 10 {
                let mut buf = vec![0u16; text_len + 1];
                GetWindowTextW(hwnd, &mut buf);
                let text = String::from_utf16_lossy(&buf[..text_len]);
                if text.contains(';') && text.contains('(') {
                    data.result = text;
                    return FALSE;
                }
            }
        }
        TRUE
    }

    let mut data = TextData {
        result: String::new(),
    };
    unsafe {
        let _ = EnumChildWindows(
            dialog,
            Some(callback),
            LPARAM(&mut data as *mut TextData as isize),
        );
    }
    data.result
}

fn enumerate_display_monitors() -> Result<Vec<LiveMonitor>, String> {
    #[cfg(not(target_os = "windows"))]
    {
        Ok(vec![
            LiveMonitor {
                mstsc_id: 0,
                left: -1920,
                top: 0,
                width: 1920,
                height: 1080,
                is_primary: false,
            },
            LiveMonitor {
                mstsc_id: 1,
                left: 0,
                top: 0,
                width: 2560,
                height: 1440,
                is_primary: true,
            },
            LiveMonitor {
                mstsc_id: 2,
                left: 2560,
                top: 0,
                width: 1920,
                height: 1080,
                is_primary: false,
            },
        ])
    }

    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::*;
        use windows::Win32::Graphics::Gdi::*;

        struct CollectData {
            monitors: Vec<LiveMonitor>,
        }

        unsafe extern "system" fn callback(
            hmon: HMONITOR,
            _hdc: HDC,
            _rect: *mut RECT,
            lparam: LPARAM,
        ) -> BOOL {
            let data = &mut *(lparam.0 as *mut CollectData);

            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };

            if GetMonitorInfoW(hmon, &mut info).as_bool() {
                let r = info.rcMonitor;
                let id = data.monitors.len() as u32;
                data.monitors.push(LiveMonitor {
                    mstsc_id: id,
                    left: r.left,
                    top: r.top,
                    width: (r.right - r.left) as u32,
                    height: (r.bottom - r.top) as u32,
                    is_primary: (info.dwFlags & 1) != 0,
                });
            }
            TRUE
        }

        let mut data = CollectData {
            monitors: Vec::new(),
        };

        unsafe {
            let ok = EnumDisplayMonitors(
                None,
                None,
                Some(callback),
                LPARAM(&mut data as *mut CollectData as isize),
            );
            if !ok.as_bool() {
                return Err("EnumDisplayMonitors failed".into());
            }
        }

        if data.monitors.is_empty() {
            Err("No monitors detected".into())
        } else {
            Ok(data.monitors)
        }
    }
}

fn parse_mstsc_output(text: &str) -> Result<Vec<LiveMonitor>, String> {
    let re_line = regex_lite_parse(text);
    if re_line.is_empty() {
        return Err("No monitor lines found in mstsc output".into());
    }
    Ok(re_line)
}

fn regex_lite_parse(text: &str) -> Vec<LiveMonitor> {
    let mut monitors = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(';').collect();
        if parts.len() < 3 {
            continue;
        }
        let id_str = parts[0].trim();
        let id: u32 = match id_str.parse() {
            Ok(v) => v,
            Err(_) => continue,
        };

        let coords = extract_numbers(parts[1]);
        if coords.len() < 4 {
            continue;
        }
        let width = (coords[2] - coords[0]) as u32;
        let height = (coords[3] - coords[1]) as u32;

        let is_primary = line.to_uppercase().contains("PRIMARY");

        monitors.push(LiveMonitor {
            mstsc_id: id,
            left: coords[0],
            top: coords[1],
            width,
            height,
            is_primary,
        });
    }
    monitors
}

fn extract_numbers(s: &str) -> Vec<i32> {
    let mut nums = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        if !chars[i].is_ascii_digit() && chars[i] != '-' {
            i += 1;
            continue;
        }

        let start = i;

        if chars[i] == '-' {
            i += 1;
            if i >= len || !chars[i].is_ascii_digit() {
                continue;
            }
        }

        while i < len && chars[i].is_ascii_digit() {
            i += 1;
        }

        let token: String = chars[start..i].iter().collect();
        if let Ok(n) = token.parse::<i32>() {
            nums.push(n);
        }
    }
    nums
}

pub fn resolve_profile(
    config: &AppConfig,
    profile: &DisplayProfile,
    live: &[LiveMonitor],
) -> Result<String, String> {
    let mut primary_id: Option<u32> = None;
    let mut other_ids: Vec<u32> = Vec::new();

    for mon_key in &profile.monitor_ids {
        let def = config
            .monitors
            .get(mon_key)
            .ok_or_else(|| format!("Monitor definition '{}' not found in config", mon_key))?;

        let matched = match_monitor(def, live).ok_or_else(|| {
            format!(
                "No current monitor matches '{}' ({}x{} at {}, {})",
                mon_key, def.width, def.height, def.left, def.top
            )
        })?;

        if *mon_key == profile.primary {
            primary_id = Some(matched.mstsc_id);
        } else {
            other_ids.push(matched.mstsc_id);
        }
    }

    let primary = primary_id.ok_or("Primary monitor not found in profile")?;
    let mut ids = vec![primary];
    ids.extend(other_ids);

    Ok(ids
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(","))
}

fn match_monitor<'a>(def: &MonitorDef, live: &'a [LiveMonitor]) -> Option<&'a LiveMonitor> {
    if let Some(m) = live.iter().find(|m| {
        m.left == def.left && m.top == def.top && m.width == def.width && m.height == def.height
    }) {
        return Some(m);
    }

    let mut candidates: Vec<_> = live
        .iter()
        .filter(|m| m.width == def.width && m.height == def.height)
        .collect();

    candidates.sort_by_key(|m| (m.left - def.left).abs() + (m.top - def.top).abs());

    candidates.first().copied()
}

pub fn auto_detect_defs(live: &[LiveMonitor]) -> HashMap<String, MonitorDef> {
    let mut sorted: Vec<_> = live.to_vec();
    sorted.sort_by_key(|m| (m.left, m.top));

    let mut result = HashMap::new();
    for (i, m) in sorted.iter().enumerate() {
        let key = format!("mon-{}", i);
        let pos_label = if m.left < 0 {
            "left"
        } else if sorted.iter().any(|o| o.left > m.left) && m.left == 0 {
            "center"
        } else if i == 0 {
            "left"
        } else if i == sorted.len() - 1 {
            "right"
        } else {
            "center"
        };
        let name = format!("{} {}x{}", pos_label, m.width, m.height);
        result.insert(
            key,
            MonitorDef {
                name,
                left: m.left,
                top: m.top,
                width: m.width,
                height: m.height,
            },
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_mstsc_output() {
        let text = "0; (-1920, 0, 0, 1080); (1920 x 1080)\n\
                     1; (0, 0, 2560, 1440); (2560 x 1440)  [PRIMARY]\n\
                     2; (2560, 0, 4480, 1080); (1920 x 1080)";
        let monitors = parse_mstsc_output(text).unwrap();
        assert_eq!(monitors.len(), 3);
        assert_eq!(monitors[0].mstsc_id, 0);
        assert_eq!(monitors[0].left, -1920);
        assert_eq!(monitors[0].width, 1920);
        assert!(monitors[1].is_primary);
        assert_eq!(monitors[2].left, 2560);
    }

    #[test]
    fn test_match_exact() {
        let live = vec![
            LiveMonitor {
                mstsc_id: 5,
                left: -1920,
                top: 0,
                width: 1920,
                height: 1080,
                is_primary: false,
            },
            LiveMonitor {
                mstsc_id: 3,
                left: 0,
                top: 0,
                width: 2560,
                height: 1440,
                is_primary: true,
            },
        ];
        let def = MonitorDef {
            name: "test".into(),
            left: -1920,
            top: 0,
            width: 1920,
            height: 1080,
        };
        let m = match_monitor(&def, &live).unwrap();
        assert_eq!(m.mstsc_id, 5);
    }

    #[test]
    fn test_extract_numbers() {
        assert_eq!(
            extract_numbers("(-1920, 0, 0, 1080)"),
            vec![-1920, 0, 0, 1080]
        );
        assert_eq!(extract_numbers("(2560 x 1440)"), vec![2560, 1440]);
        assert_eq!(
            extract_numbers("(-3840, -200, -1920, 880)"),
            vec![-3840, -200, -1920, 880]
        );
        assert_eq!(extract_numbers("()"), Vec::<i32>::new());
    }

    #[test]
    fn test_match_fallback_same_resolution() {
        let live = vec![
            LiveMonitor {
                mstsc_id: 2,
                left: -1920,
                top: 0,
                width: 1920,
                height: 1080,
                is_primary: false,
            },
            LiveMonitor {
                mstsc_id: 0,
                left: 0,
                top: 0,
                width: 2560,
                height: 1440,
                is_primary: true,
            },
            LiveMonitor {
                mstsc_id: 1,
                left: 2560,
                top: 0,
                width: 1920,
                height: 1080,
                is_primary: false,
            },
        ];
        let def = MonitorDef {
            name: "left".into(),
            left: -1920,
            top: 0,
            width: 1920,
            height: 1080,
        };
        let m = match_monitor(&def, &live).unwrap();
        assert_eq!(m.mstsc_id, 2);
    }

    #[test]
    fn test_resolve_profile() {
        let mut config = AppConfig::default();
        config.monitors.insert(
            "left".into(),
            MonitorDef {
                name: "left".into(),
                left: -1920,
                top: 0,
                width: 1920,
                height: 1080,
            },
        );
        config.monitors.insert(
            "center".into(),
            MonitorDef {
                name: "center".into(),
                left: 0,
                top: 0,
                width: 2560,
                height: 1440,
            },
        );

        let profile = DisplayProfile {
            name: "test".into(),
            monitor_ids: vec!["left".into(), "center".into()],
            primary: "center".into(),
        };

        let live = vec![
            LiveMonitor {
                mstsc_id: 7,
                left: -1920,
                top: 0,
                width: 1920,
                height: 1080,
                is_primary: false,
            },
            LiveMonitor {
                mstsc_id: 3,
                left: 0,
                top: 0,
                width: 2560,
                height: 1440,
                is_primary: true,
            },
        ];

        let result = resolve_profile(&config, &profile, &live).unwrap();
        assert_eq!(result, "3,7");
    }
}
