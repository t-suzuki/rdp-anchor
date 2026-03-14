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
    #[serde(default)]
    pub device_name: String,
}

pub fn get_current_monitors() -> Result<Vec<LiveMonitor>, String> {
    let mut monitors = enumerate_display_monitors()?;
    populate_device_names(&mut monitors);
    Ok(monitors)
}

/// Get monitors with verified mstsc IDs for use during connect/preflight.
/// Falls back to EnumDisplayMonitors IDs if the hook approach fails.
pub fn get_monitors_for_connect() -> Result<Vec<LiveMonitor>, String> {
    let mut monitors = enumerate_display_monitors()?;
    populate_device_names(&mut monitors);

    #[cfg(target_os = "windows")]
    {
        match capture_mstsc_silent() {
            Ok((_raw_text, mstsc_monitors)) => {
                // Merge mstsc IDs into our monitors by matching coordinates
                for mon in &mut monitors {
                    if let Some(mstsc_mon) = mstsc_monitors.iter().find(|m| {
                        m.left == mon.left
                            && m.top == mon.top
                            && m.width == mon.width
                            && m.height == mon.height
                    }) {
                        mon.mstsc_id = mstsc_mon.mstsc_id;
                    }
                }
            }
            Err(_) => {
                // Fall back to EnumDisplayMonitors IDs (best effort)
            }
        }
    }

    Ok(monitors)
}

/// Diagnostic result with detailed logs.
#[derive(Serialize)]
pub struct CaptureResult {
    pub raw_text: String,
    pub monitors: Vec<LiveMonitor>,
    pub logs: Vec<String>,
}

/// Diagnostic: run mstsc /l capture and return raw text + parsed monitors + logs.
pub fn test_mstsc_capture() -> Result<CaptureResult, String> {
    #[cfg(not(target_os = "windows"))]
    {
        Err("mstsc is only available on Windows".into())
    }

    #[cfg(target_os = "windows")]
    {
        capture_mstsc_silent_with_logs()
    }
}

/// Non-diagnostic version for connect/preflight (discards logs).
#[cfg(target_os = "windows")]
fn capture_mstsc_silent() -> Result<(String, Vec<LiveMonitor>), String> {
    let result = capture_mstsc_silent_with_logs()?;
    Ok((result.raw_text, result.monitors))
}

/// Spawn mstsc /l as a debuggee, hook MessageBoxW to silently capture monitor text.
#[cfg(target_os = "windows")]
fn capture_mstsc_silent_with_logs() -> Result<CaptureResult, String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::*;
    use windows::Win32::System::Diagnostics::Debug::*;
    use windows::Win32::System::Memory::*;
    use windows::Win32::System::Threading::*;

    let mut logs: Vec<String> = Vec::new();

    // Spawn mstsc /l as a debuggee
    let mut cmd_line: Vec<u16> = OsStr::new("mstsc.exe /l")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    // Don't use SW_HIDE - mstsc may skip MessageBoxW if window is hidden
    // si.dwFlags = STARTF_USESHOWWINDOW;
    // si.wShowWindow = 0;

    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    unsafe {
        CreateProcessW(
            None,
            windows::core::PWSTR(cmd_line.as_mut_ptr()),
            None,
            None,
            false,
            DEBUG_PROCESS | CREATE_NEW_CONSOLE,
            None,
            None,
            &si,
            &mut pi,
        )
    }
    .map_err(|e| format!("CreateProcessW failed: {e}"))?;

    let h_process = pi.hProcess;
    let h_thread = pi.hThread;
    let _guard = ProcessGuard {
        h_process,
        h_thread,
    };

    logs.push(format!("Process created: PID={}", pi.dwProcessId));

    // Buffer layout: [sentinel 4 bytes] [text data ...]
    // Sentinel 0xCAFEBABE = shellcode not yet called
    // Shellcode writes text starting at offset 0, overwriting sentinel
    let buf_size: usize = 8192;
    let remote_buf = unsafe {
        VirtualAllocEx(
            h_process,
            None,
            buf_size,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        )
    };
    if remote_buf.is_null() {
        return Err("VirtualAllocEx for buffer failed".into());
    }
    logs.push(format!("Remote buffer at {:p}", remote_buf));

    // Write sentinel
    let sentinel: u32 = 0xCAFEBABE;
    unsafe {
        let _ = WriteProcessMemory(
            h_process,
            remote_buf,
            &sentinel as *const u32 as *const _,
            4,
            None,
        );
    }

    let mut hooked = false;
    let mut original_bytes = [0u8; 12];
    let mut msgbox_addr: usize = 0;
    let mut event_count = 0u32;
    let mut dll_count = 0u32;
    let mut initial_bp_handled = false;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);

    loop {
        if std::time::Instant::now() > deadline {
            logs.push("Timeout (8s) reached".into());
            break;
        }

        let mut event: DEBUG_EVENT = unsafe { std::mem::zeroed() };
        let got = unsafe { WaitForDebugEvent(&mut event, 200) };
        if got.is_err() {
            continue;
        }

        event_count += 1;

        let continue_status = match event.dwDebugEventCode {
            CREATE_PROCESS_DEBUG_EVENT => {
                logs.push(format!("[{}] CREATE_PROCESS", event_count));
                if !hooked {
                    if let Some(addr) =
                        try_hook_messagebox(h_process, remote_buf, &mut original_bytes)
                    {
                        hooked = true;
                        msgbox_addr = addr;
                        logs.push(format!("  Hook applied (addr=0x{:x})", addr));
                    }
                }
                DBG_CONTINUE
            }
            LOAD_DLL_DEBUG_EVENT => {
                dll_count += 1;
                if !hooked {
                    if let Some(addr) =
                        try_hook_messagebox(h_process, remote_buf, &mut original_bytes)
                    {
                        hooked = true;
                        msgbox_addr = addr;
                        logs.push(format!(
                            "[{}] LOAD_DLL #{} -> Hook applied! (addr=0x{:x})",
                            event_count, dll_count, addr
                        ));
                    } else if dll_count <= 5 {
                        logs.push(format!(
                            "[{}] LOAD_DLL #{} (user32 not ready)",
                            event_count, dll_count
                        ));
                    }
                }
                DBG_CONTINUE
            }
            EXCEPTION_DEBUG_EVENT => {
                let code = unsafe { event.u.Exception.ExceptionRecord.ExceptionCode };
                let first_chance = unsafe { event.u.Exception.dwFirstChance };
                let addr = unsafe { event.u.Exception.ExceptionRecord.ExceptionAddress } as usize;
                logs.push(format!(
                    "[{}] EXCEPTION code=0x{:08x} addr=0x{:x} 1st={}",
                    event_count, code.0, addr, first_chance
                ));

                // STATUS_BREAKPOINT - initial breakpoint and any others
                if code.0 == 0x80000003u32 as i32 {
                    if !initial_bp_handled {
                        initial_bp_handled = true;
                        logs.push("  -> Initial breakpoint (DBG_CONTINUE)".into());
                    }
                    DBG_CONTINUE
                } else {
                    DBG_EXCEPTION_NOT_HANDLED
                }
            }
            EXIT_PROCESS_DEBUG_EVENT => {
                let exit_code = unsafe { event.u.ExitProcess.dwExitCode };
                logs.push(format!("[{}] EXIT_PROCESS (code={})", event_count, exit_code));
                unsafe {
                    let _ = ContinueDebugEvent(
                        event.dwProcessId,
                        event.dwThreadId,
                        DBG_CONTINUE,
                    );
                }
                break;
            }
            CREATE_THREAD_DEBUG_EVENT => DBG_CONTINUE,
            EXIT_THREAD_DEBUG_EVENT => DBG_CONTINUE,
            UNLOAD_DLL_DEBUG_EVENT => DBG_CONTINUE,
            OUTPUT_DEBUG_STRING_EVENT => {
                logs.push(format!("[{}] OUTPUT_DEBUG_STRING", event_count));
                DBG_CONTINUE
            }
            other => {
                logs.push(format!("[{}] event_code={}", event_count, other.0));
                DBG_CONTINUE
            }
        };

        unsafe {
            let _ = ContinueDebugEvent(event.dwProcessId, event.dwThreadId, continue_status);
        }
    }

    logs.push(format!(
        "Summary: {} events, {} DLLs, hooked={}, bp_handled={}",
        event_count, dll_count, hooked, initial_bp_handled
    ));

    // Verify hook is still in place
    if hooked && msgbox_addr != 0 {
        let mut verify = [0u8; 12];
        let mut vread = 0usize;
        unsafe {
            let _ = ReadProcessMemory(
                h_process,
                msgbox_addr as *const _,
                verify.as_mut_ptr() as *mut _,
                12,
                Some(&mut vread),
            );
        }
        let hook_intact = verify[0] == 0x48 && verify[1] == 0xB8 && verify[10] == 0xFF && verify[11] == 0xE0;
        logs.push(format!(
            "Hook verify: intact={} bytes={:02x?}",
            hook_intact,
            &verify[..vread.min(12)]
        ));
    }

    // Check sentinel
    let mut sentinel_check = [0u8; 4];
    unsafe {
        let _ = ReadProcessMemory(
            h_process,
            remote_buf,
            sentinel_check.as_mut_ptr() as *mut _,
            4,
            None,
        );
    }
    let sentinel_val = u32::from_le_bytes(sentinel_check);
    if sentinel_val == 0xCAFEBABE {
        logs.push("Sentinel: INTACT (0xCAFEBABE) -> shellcode was NOT called".into());
    } else {
        logs.push(format!(
            "Sentinel: OVERWRITTEN (0x{:08x}) -> shellcode was called!",
            sentinel_val
        ));
    }

    // Restore original bytes if we hooked
    if hooked && msgbox_addr != 0 {
        unsafe {
            let mut old_prot = PAGE_PROTECTION_FLAGS(0);
            let _ = VirtualProtectEx(
                h_process,
                msgbox_addr as *const _,
                12,
                PAGE_EXECUTE_READWRITE,
                &mut old_prot,
            );
            let _ = WriteProcessMemory(
                h_process,
                msgbox_addr as *const _,
                original_bytes.as_ptr() as *const _,
                12,
                None,
            );
            let _ = VirtualProtectEx(
                h_process,
                msgbox_addr as *const _,
                12,
                old_prot,
                &mut old_prot,
            );
        }
        logs.push("Original MessageBoxW bytes restored".into());
    }

    // Read captured text from remote buffer
    let mut local_buf = vec![0u8; buf_size];
    let mut bytes_read = 0usize;
    unsafe {
        let _ = ReadProcessMemory(
            h_process,
            remote_buf,
            local_buf.as_mut_ptr() as *mut _,
            buf_size,
            Some(&mut bytes_read),
        );
    }
    logs.push(format!("Buffer: read {} bytes", bytes_read));

    // Log first 32 bytes as hex for debugging
    let hex_preview: String = local_buf[..bytes_read.min(32)]
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .join(" ");
    logs.push(format!("Buffer hex[0..32]: {}", hex_preview));

    // Free remote buffer
    unsafe {
        let _ = VirtualFreeEx(h_process, remote_buf, 0, MEM_RELEASE);
    }

    // Convert UTF-16 LE buffer to string
    let u16_slice: &[u16] =
        unsafe { std::slice::from_raw_parts(local_buf.as_ptr() as *const u16, bytes_read / 2) };
    let text_end = u16_slice
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(u16_slice.len());
    let text = String::from_utf16_lossy(&u16_slice[..text_end]);

    logs.push(format!("Text: {} chars", text.len()));
    if !text.is_empty() {
        logs.push(format!("Preview: {:?}", &text[..text.len().min(200)]));
    }

    let monitors = if text.is_empty() {
        Vec::new()
    } else {
        match parse_mstsc_output(&text) {
            Ok(m) => {
                logs.push(format!("Parsed {} monitors", m.len()));
                m
            }
            Err(e) => {
                logs.push(format!("Parse error: {}", e));
                Vec::new()
            }
        }
    };

    Ok(CaptureResult {
        raw_text: text,
        monitors,
        logs,
    })
}

/// Try to find and hook MessageBoxW in the target process.
/// Returns Some(address) if successfully hooked.
#[cfg(target_os = "windows")]
fn try_hook_messagebox(
    h_process: windows::Win32::Foundation::HANDLE,
    remote_buf: *mut std::ffi::c_void,
    original_bytes: &mut [u8; 12],
) -> Option<usize> {
    use windows::Win32::System::Diagnostics::Debug::*;
    use windows::Win32::System::LibraryLoader::*;
    use windows::Win32::System::Memory::*;
    use windows::core::s;

    // Get MessageBoxW address from our own process (same address in target due to ASLR shared mapping)
    let user32 = unsafe { GetModuleHandleA(s!("user32.dll")) }.ok()?;
    let msgbox_fn = unsafe { GetProcAddress(user32, s!("MessageBoxW")) }?;
    let msgbox_addr = msgbox_fn as usize;

    // Read original bytes
    let mut bytes_read = 0usize;
    unsafe {
        ReadProcessMemory(
            h_process,
            msgbox_addr as *const _,
            original_bytes.as_mut_ptr() as *mut _,
            12,
            Some(&mut bytes_read),
        )
    }
    .ok()?;
    if bytes_read < 12 {
        return None;
    }

    // Build shellcode stub:
    //   mov rax, <buf_addr>       ; 48 B8 <imm64>
    // copy_loop:                  ; offset 10
    //   mov cx, [rdx]             ; 66 8B 0A
    //   mov [rax], cx             ; 66 89 08
    //   add rdx, 2               ; 48 83 C2 02
    //   add rax, 2               ; 48 83 C0 02
    //   test cx, cx              ; 66 85 C9
    //   jnz copy_loop            ; 75 ED
    //   mov eax, 1               ; B8 01 00 00 00
    //   ret                      ; C3
    let buf_addr = remote_buf as u64;
    let mut shellcode: Vec<u8> = Vec::with_capacity(35);
    shellcode.extend_from_slice(&[0x48, 0xB8]); // mov rax, imm64
    shellcode.extend_from_slice(&buf_addr.to_le_bytes());
    // copy_loop (offset 10):
    shellcode.extend_from_slice(&[0x66, 0x8B, 0x0A]); // mov cx, [rdx]
    shellcode.extend_from_slice(&[0x66, 0x89, 0x08]); // mov [rax], cx
    shellcode.extend_from_slice(&[0x48, 0x83, 0xC2, 0x02]); // add rdx, 2
    shellcode.extend_from_slice(&[0x48, 0x83, 0xC0, 0x02]); // add rax, 2
    shellcode.extend_from_slice(&[0x66, 0x85, 0xC9]); // test cx, cx
    shellcode.extend_from_slice(&[0x75, 0xED]); // jnz -19 (back to offset 10)
    shellcode.extend_from_slice(&[0xB8, 0x01, 0x00, 0x00, 0x00]); // mov eax, 1
    shellcode.push(0xC3); // ret

    // Allocate remote executable memory for shellcode
    let stub_mem = unsafe {
        VirtualAllocEx(
            h_process,
            None,
            shellcode.len(),
            MEM_COMMIT | MEM_RESERVE,
            PAGE_EXECUTE_READWRITE,
        )
    };
    if stub_mem.is_null() {
        return None;
    }

    // Write shellcode to remote process
    unsafe {
        WriteProcessMemory(
            h_process,
            stub_mem,
            shellcode.as_ptr() as *const _,
            shellcode.len(),
            None,
        )
    }
    .ok()?;

    // Build inline hook at MessageBoxW:
    //   mov rax, <stub_addr>    ; 48 B8 <imm64>
    //   jmp rax                 ; FF E0
    let stub_addr = stub_mem as u64;
    let mut hook: [u8; 12] = [0; 12];
    hook[0] = 0x48;
    hook[1] = 0xB8;
    hook[2..10].copy_from_slice(&stub_addr.to_le_bytes());
    hook[10] = 0xFF;
    hook[11] = 0xE0;

    // Make MessageBoxW writable, write hook, restore protection
    let mut old_prot = PAGE_PROTECTION_FLAGS(0);
    unsafe {
        VirtualProtectEx(
            h_process,
            msgbox_addr as *const _,
            12,
            PAGE_EXECUTE_READWRITE,
            &mut old_prot,
        )
    }
    .ok()?;

    let write_ok = unsafe {
        WriteProcessMemory(
            h_process,
            msgbox_addr as *const _,
            hook.as_ptr() as *const _,
            12,
            None,
        )
    };
    if write_ok.is_err() {
        return None;
    }

    unsafe {
        let _ = VirtualProtectEx(
            h_process,
            msgbox_addr as *const _,
            12,
            old_prot,
            &mut old_prot,
        );
    }

    Some(msgbox_addr)
}

/// RAII guard for process/thread handles
#[cfg(target_os = "windows")]
struct ProcessGuard {
    h_process: windows::Win32::Foundation::HANDLE,
    h_thread: windows::Win32::Foundation::HANDLE,
}

#[cfg(target_os = "windows")]
impl Drop for ProcessGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Threading::TerminateProcess(self.h_process, 1);
            let _ = windows::Win32::Foundation::CloseHandle(self.h_process);
            let _ = windows::Win32::Foundation::CloseHandle(self.h_thread);
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn populate_device_names(_monitors: &mut [LiveMonitor]) {}

#[cfg(target_os = "windows")]
fn populate_device_names(monitors: &mut [LiveMonitor]) {
    use windows::Win32::Graphics::Gdi::*;
    use windows::core::PCWSTR;

    let mut adapter_idx = 0u32;
    loop {
        let mut adapter: DISPLAY_DEVICEW = unsafe { std::mem::zeroed() };
        adapter.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;

        let ok = unsafe {
            EnumDisplayDevicesW(PCWSTR(std::ptr::null()), adapter_idx, &mut adapter, 0)
        };
        if !ok.as_bool() {
            break;
        }
        adapter_idx += 1;

        // Skip inactive adapters (DISPLAY_DEVICE_ATTACHED_TO_DESKTOP = 0x1)
        if (adapter.StateFlags & 0x1) == 0 {
            continue;
        }

        let name_nul: Vec<u16> = adapter
            .DeviceName
            .iter()
            .take_while(|&&c| c != 0)
            .copied()
            .chain(std::iter::once(0))
            .collect();

        // Get current display settings for position & resolution
        let mut devmode: DEVMODEW = unsafe { std::mem::zeroed() };
        devmode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
        let ok = unsafe {
            EnumDisplaySettingsW(PCWSTR(name_nul.as_ptr()), ENUM_CURRENT_SETTINGS, &mut devmode)
        };
        if !ok.as_bool() {
            continue;
        }

        let (pos_x, pos_y) = unsafe {
            let pos = devmode.Anonymous1.Anonymous2.dmPosition;
            (pos.x, pos.y)
        };
        let w = devmode.dmPelsWidth;
        let h = devmode.dmPelsHeight;

        // Find the LiveMonitor that matches this adapter's position & resolution
        if let Some(mon) = monitors
            .iter_mut()
            .find(|m| m.left == pos_x && m.top == pos_y && m.width == w && m.height == h)
        {
            // Get monitor friendly name via second call to EnumDisplayDevicesW
            let mut mon_dev: DISPLAY_DEVICEW = unsafe { std::mem::zeroed() };
            mon_dev.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
            let ok = unsafe {
                EnumDisplayDevicesW(PCWSTR(name_nul.as_ptr()), 0, &mut mon_dev, 0)
            };
            if ok.as_bool() {
                let chars: Vec<u16> = mon_dev
                    .DeviceString
                    .iter()
                    .take_while(|&&c| c != 0)
                    .copied()
                    .collect();
                let friendly = String::from_utf16_lossy(&chars);
                if !friendly.is_empty() {
                    mon.device_name = friendly;
                }
            }
        }
    }
}

#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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
                device_name: "Generic Monitor".into(),
            },
            LiveMonitor {
                mstsc_id: 1,
                left: 0,
                top: 0,
                width: 2560,
                height: 1440,
                is_primary: true,
                device_name: "Generic Monitor".into(),
            },
            LiveMonitor {
                mstsc_id: 2,
                left: 2560,
                top: 0,
                width: 1920,
                height: 1080,
                is_primary: false,
                device_name: "Generic Monitor".into(),
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
                    device_name: String::new(),
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

#[allow(dead_code)]
fn parse_mstsc_output(text: &str) -> Result<Vec<LiveMonitor>, String> {
    let re_line = regex_lite_parse(text);
    if re_line.is_empty() {
        return Err("No monitor lines found in mstsc output".into());
    }
    Ok(re_line)
}

#[allow(dead_code)]
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
            device_name: String::new(),
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
        let name = if !m.device_name.is_empty() {
            format!("{} {}x{}", m.device_name, m.width, m.height)
        } else {
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
            format!("{} {}x{}", pos_label, m.width, m.height)
        };
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
                device_name: String::new(),
            },
            LiveMonitor {
                mstsc_id: 3,
                left: 0,
                top: 0,
                width: 2560,
                height: 1440,
                is_primary: true,
                device_name: String::new(),
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
                device_name: String::new(),
            },
            LiveMonitor {
                mstsc_id: 0,
                left: 0,
                top: 0,
                width: 2560,
                height: 1440,
                is_primary: true,
                device_name: String::new(),
            },
            LiveMonitor {
                mstsc_id: 1,
                left: 2560,
                top: 0,
                width: 1920,
                height: 1080,
                is_primary: false,
                device_name: String::new(),
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
                device_name: String::new(),
            },
            LiveMonitor {
                mstsc_id: 3,
                left: 0,
                top: 0,
                width: 2560,
                height: 1440,
                is_primary: true,
                device_name: String::new(),
            },
        ];

        let result = resolve_profile(&config, &profile, &live).unwrap();
        assert_eq!(result, "3,7");
    }
}
