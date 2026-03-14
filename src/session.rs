use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ActiveSession {
    pub window_title: String,
    pub hostname: String,
}

pub fn get_active_sessions() -> Vec<ActiveSession> {
    #[cfg(not(target_os = "windows"))]
    {
        Vec::new()
    }

    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::*;
        use windows::Win32::UI::WindowsAndMessaging::*;

        struct SessionData {
            sessions: Vec<ActiveSession>,
        }

        unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let data = &mut *(lparam.0 as *mut SessionData);

            let mut class_buf = [0u16; 256];
            let class_len = GetClassNameW(hwnd, &mut class_buf) as usize;
            if class_len == 0 {
                return TRUE;
            }
            let class_name = String::from_utf16_lossy(&class_buf[..class_len]);

            let is_mstsc = class_name == "TscShellContainerClass" || class_name == "RAIL_WINDOW";

            if !is_mstsc || !IsWindowVisible(hwnd).as_bool() {
                return TRUE;
            }

            let title_len = GetWindowTextLengthW(hwnd) as usize;
            if title_len == 0 {
                return TRUE;
            }
            let mut title_buf = vec![0u16; title_len + 1];
            GetWindowTextW(hwnd, &mut title_buf);
            let title = String::from_utf16_lossy(&title_buf[..title_len]);

            let hostname = title
                .split(" - ")
                .next()
                .unwrap_or(&title)
                .trim()
                .to_string();

            if !hostname.is_empty() {
                data.sessions.push(ActiveSession {
                    window_title: title,
                    hostname,
                });
            }

            TRUE
        }

        let mut data = SessionData {
            sessions: Vec::new(),
        };

        unsafe {
            let _ = EnumWindows(
                Some(callback),
                LPARAM(&mut data as *mut SessionData as isize),
            );
        }

        data.sessions
    }
}

pub fn is_host_connected(host: &str) -> bool {
    let sessions = get_active_sessions();
    let host_lower = host.to_lowercase();
    sessions.iter().any(|s| {
        s.hostname.to_lowercase().contains(&host_lower)
            || host_lower.contains(&s.hostname.to_lowercase())
    })
}
