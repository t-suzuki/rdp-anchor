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

/// Wait for a new mstsc window to appear after connection, then relocate it
/// to the profile's primary monitor and re-enter fullscreen there.
///
/// mstsc's fullscreen is a special mode that ignores ShowWindow/SetWindowPlacement.
/// The only way to exit fullscreen is to drag the connection bar away from the
/// top edge. We find the bar window by enumerating windows in the mstsc process,
/// get its exact position, and simulate a mouse drag.
///
/// Steps: detect new window → wait for fullscreen → find connection bar window →
/// drag bar downward (exits fullscreen) → move window to target monitor →
/// Ctrl+Alt+Break to re-enter fullscreen on correct monitor.
#[cfg(target_os = "windows")]
pub fn relocate_mstsc_to_monitor(target_left: i32, target_top: i32) {
    use windows::Win32::Foundation::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    // Snapshot existing mstsc windows before the new one appears
    let existing: Vec<isize> = enum_mstsc_hwnds();

    // Poll for a new window (up to 30 seconds, every 500ms)
    let mut new_hwnd: Option<HWND> = None;
    for _ in 0..60 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let current = enum_mstsc_hwnds();
        if let Some(&h) = current.iter().find(|h| !existing.contains(h)) {
            new_hwnd = Some(HWND(h as *mut _));
            break;
        }
    }

    let hwnd = match new_hwnd {
        Some(h) => h,
        None => return,
    };

    // Wait for mstsc to finish connecting and enter fullscreen
    std::thread::sleep(std::time::Duration::from_secs(3));

    unsafe {
        // Find the connection bar by enumerating all windows in the mstsc process.
        let mut mstsc_pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut mstsc_pid));

        let bar = find_connection_bar(mstsc_pid);
        let bar_hwnd = match bar {
            Some(b) => b,
            None => return, // no bar found, can't exit fullscreen
        };

        // Get actual position of the connection bar
        let mut bar_rect = RECT::default();
        let _ = GetWindowRect(bar_hwnd, &mut bar_rect);

        let start_x = (bar_rect.left + bar_rect.right) / 2;
        let start_y = (bar_rect.top + bar_rect.bottom) / 2;

        // Two-phase drag in one continuous motion (mouse stays held):
        //   Phase 1: drag DOWN to detach bar from top edge → exits fullscreen
        //   Phase 2: drag to target monitor's top edge → re-enters fullscreen there
        let mid_y = start_y + 400;
        let end_x = target_left + 400;
        let end_y = target_top;

        send_mouse_drag_via(start_x, start_y, &[(start_x, mid_y), (end_x, end_y)]);
    }
}

/// Find the mstsc connection bar window handle by process ID.
/// The bar is a separate top-level window belonging to the same process,
/// distinct from TscShellContainerClass / RAIL_WINDOW.
/// It is typically thin and wide (height << width).
#[cfg(target_os = "windows")]
fn find_connection_bar(target_pid: u32) -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    struct BarSearch {
        target_pid: u32,
        candidates: Vec<(HWND, String, RECT)>,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let data = &mut *(lparam.0 as *mut BarSearch);

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != data.target_pid {
            return TRUE;
        }

        if !IsWindowVisible(hwnd).as_bool() {
            return TRUE;
        }

        let mut class_buf = [0u16; 256];
        let class_len = GetClassNameW(hwnd, &mut class_buf) as usize;
        if class_len == 0 {
            return TRUE;
        }
        let class_name = String::from_utf16_lossy(&class_buf[..class_len]);

        // Skip the main mstsc windows
        if class_name == "TscShellContainerClass" || class_name == "RAIL_WINDOW" {
            return TRUE;
        }

        let mut rect = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rect);

        data.candidates.push((hwnd, class_name, rect));
        TRUE
    }

    let mut data = BarSearch {
        target_pid,
        candidates: Vec::new(),
    };

    unsafe {
        let _ = EnumWindows(
            Some(callback),
            LPARAM(&mut data as *mut BarSearch as isize),
        );
    }

    // The connection bar is thin and wide. Pick the candidate with the
    // largest width-to-height ratio.
    data.candidates
        .iter()
        .filter(|(_, _, r)| {
            let w = r.right - r.left;
            let h = r.bottom - r.top;
            w > 100 && h > 0 && h < 80
        })
        .max_by_key(|(_, _, r)| {
            let w = r.right - r.left;
            let h = (r.bottom - r.top).max(1);
            w / h
        })
        .map(|(hwnd, _, _)| *hwnd)
}

/// Convert pixel coordinates to SendInput absolute coordinates (0–65535)
/// using the virtual desktop (spans all monitors).
#[cfg(target_os = "windows")]
fn to_absolute(px_x: i32, px_y: i32) -> (i32, i32) {
    use windows::Win32::UI::WindowsAndMessaging::*;
    unsafe {
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        let ax = ((px_x - vx) as i64 * 65535 / vw as i64) as i32;
        let ay = ((px_y - vy) as i64 * 65535 / vh as i64) as i32;
        (ax, ay)
    }
}

/// Simulate a mouse drag through waypoints via SendInput.
/// Mouse down at (x0,y0), smooth move through each waypoint, mouse up at the last point.
/// Pauses briefly at each waypoint to let the system register the position.
#[cfg(target_os = "windows")]
unsafe fn send_mouse_drag_via(x0: i32, y0: i32, waypoints: &[(i32, i32)]) {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    let size = std::mem::size_of::<INPUT>() as i32;
    let steps_per_segment = 20;

    // Move to start position
    let (ax, ay) = to_absolute(x0, y0);
    let move_to = [mouse_input(
        ax,
        ay,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
    )];
    SendInput(&move_to, size);
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Press left button
    let down = [mouse_input(0, 0, MOUSEEVENTF_LEFTDOWN)];
    SendInput(&down, size);
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Drag through each waypoint
    let mut prev_x = x0;
    let mut prev_y = y0;
    for &(wx, wy) in waypoints {
        for i in 1..=steps_per_segment {
            let cx = prev_x + (wx - prev_x) * i / steps_per_segment;
            let cy = prev_y + (wy - prev_y) * i / steps_per_segment;
            let (acx, acy) = to_absolute(cx, cy);
            let step = [mouse_input(
                acx,
                acy,
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            )];
            SendInput(&step, size);
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        // Pause at waypoint to let the system register
        std::thread::sleep(std::time::Duration::from_millis(300));
        prev_x = wx;
        prev_y = wy;
    }

    std::thread::sleep(std::time::Duration::from_millis(50));

    // Release left button
    let up = [mouse_input(0, 0, MOUSEEVENTF_LEFTUP)];
    SendInput(&up, size);
}

#[cfg(target_os = "windows")]
fn mouse_input(dx: i32, dy: i32, flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS) -> windows::Win32::UI::Input::KeyboardAndMouse::INPUT {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Enumerate all visible mstsc window handles.
#[cfg(target_os = "windows")]
fn enum_mstsc_hwnds() -> Vec<isize> {
    use windows::Win32::Foundation::*;
    use windows::Win32::UI::WindowsAndMessaging::*;

    struct HwndData {
        hwnds: Vec<isize>,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let data = &mut *(lparam.0 as *mut HwndData);
        let mut class_buf = [0u16; 256];
        let class_len = GetClassNameW(hwnd, &mut class_buf) as usize;
        if class_len == 0 {
            return TRUE;
        }
        let class_name = String::from_utf16_lossy(&class_buf[..class_len]);
        if (class_name == "TscShellContainerClass" || class_name == "RAIL_WINDOW")
            && IsWindowVisible(hwnd).as_bool()
        {
            data.hwnds.push(hwnd.0 as isize);
        }
        TRUE
    }

    let mut data = HwndData { hwnds: Vec::new() };
    unsafe {
        let _ = EnumWindows(
            Some(callback),
            LPARAM(&mut data as *mut HwndData as isize),
        );
    }
    data.hwnds
}

pub fn is_host_connected(host: &str) -> bool {
    let sessions = get_active_sessions();
    let host_lower = host.to_lowercase();
    sessions.iter().any(|s| {
        s.hostname.to_lowercase().contains(&host_lower)
            || host_lower.contains(&s.hostname.to_lowercase())
    })
}
