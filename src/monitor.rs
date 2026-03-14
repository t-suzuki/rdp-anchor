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
/// Returns (monitors_with_correct_ids, used_fallback).
/// `used_fallback` is true when BP capture failed and EnumDisplayMonitors IDs are used as-is.
pub fn get_monitors_for_connect() -> Result<(Vec<LiveMonitor>, bool), String> {
    let mut monitors = enumerate_display_monitors()?;
    populate_device_names(&mut monitors);

    #[cfg(target_os = "windows")]
    {
        match capture_mstsc_silent() {
            Ok((_raw_text, mstsc_monitors, used_fallback)) => {
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
                return Ok((monitors, used_fallback));
            }
            Err(_) => {
                // Fall back to EnumDisplayMonitors IDs (best effort)
                return Ok((monitors, true));
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    Ok((monitors, false))
}

/// Diagnostic result with detailed logs.
#[derive(Serialize)]
pub struct CaptureResult {
    pub raw_text: String,
    pub monitors: Vec<LiveMonitor>,
    pub logs: Vec<String>,
}

/// Test: run mstsc /l BP capture and return raw text + parsed monitors.
pub fn test_mstsc_capture() -> Result<CaptureResult, String> {
    #[cfg(not(target_os = "windows"))]
    {
        Err("mstsc is only available on Windows".into())
    }

    #[cfg(target_os = "windows")]
    {
        let start = std::time::Instant::now();
        let mut logs = Vec::new();
        match capture_mstsc_bp(&mut logs) {
            Ok((text, monitors)) => {
                let elapsed = start.elapsed();
                logs.insert(0, format!("BP capture succeeded in {:.1}s", elapsed.as_secs_f64()));
                logs.push(format!("Parsed {} monitors", monitors.len()));
                Ok(CaptureResult {
                    logs,
                    raw_text: text,
                    monitors,
                })
            }
            Err(e) => {
                logs.insert(0, format!("BP capture FAILED: {e}"));
                Ok(CaptureResult {
                    logs,
                    raw_text: String::new(),
                    monitors: Vec::new(),
                })
            }
        }
    }
}

/// Test hook mechanism: spawn test_msgbox.exe --indirect (child process scenario).
pub fn test_hook_basic() -> Result<CaptureResult, String> {
    #[cfg(not(target_os = "windows"))]
    {
        Err("Windows only".into())
    }

    #[cfg(target_os = "windows")]
    {
        // --indirect: test_msgbox spawns itself without the flag as a child process.
        // This tests that our debug loop hooks child processes too.
        let exe = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("debug")
            .join("test_msgbox.exe");
        capture_debug_target(&format!("{} --indirect", exe.display()))
    }
}

/// Diagnose: set breakpoint on CreateWindowExW, log every call with params + stack trace.
pub fn diagnose_mstsc() -> Result<CaptureResult, String> {
    #[cfg(not(target_os = "windows"))]
    {
        Err("Windows only".into())
    }

    #[cfg(target_os = "windows")]
    {
        diagnose_mstsc_inner()
    }
}

#[cfg(target_os = "windows")]
fn diagnose_mstsc_inner() -> Result<CaptureResult, String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::*;
    use windows::Win32::System::Diagnostics::Debug::*;
    use windows::Win32::System::Memory::*;
    use windows::Win32::System::Threading::*;
    use windows::Win32::System::LibraryLoader::*;
    use windows::core::*;

    // Raw FFI for Get/SetThreadContext (not in windows crate with current features)
    #[repr(C, align(16))]
    struct ContextAmd64([u8; 1232]);

    extern "system" {
        fn GetThreadContext(hThread: HANDLE, lpContext: *mut ContextAmd64) -> BOOL;
        fn SetThreadContext(hThread: HANDLE, lpContext: *const ContextAmd64) -> BOOL;
    }

    // CONTEXT offsets for x86_64
    const CTX_FLAGS: usize = 0x30;
    const CTX_EFLAGS: usize = 0x44;
    const CTX_RCX: usize = 0x80;
    const CTX_RDX: usize = 0x88;
    const CTX_RSP: usize = 0x98;
    const CTX_R8: usize = 0xB8;
    const CTX_R9: usize = 0xC0;
    const CTX_RIP: usize = 0xF8;

    fn ctx_u64(ctx: &ContextAmd64, off: usize) -> u64 {
        u64::from_le_bytes(ctx.0[off..off + 8].try_into().unwrap())
    }
    fn ctx_u32(ctx: &ContextAmd64, off: usize) -> u32 {
        u32::from_le_bytes(ctx.0[off..off + 4].try_into().unwrap())
    }
    fn ctx_set_u64(ctx: &mut ContextAmd64, off: usize, val: u64) {
        ctx.0[off..off + 8].copy_from_slice(&val.to_le_bytes());
    }
    fn ctx_set_u32(ctx: &mut ContextAmd64, off: usize, val: u32) {
        ctx.0[off..off + 4].copy_from_slice(&val.to_le_bytes());
    }

    let mut logs: Vec<String> = Vec::new();

    // Resolve target function addresses
    struct BpTarget {
        name: &'static str,
        addr: usize,
        orig_byte: u8,
        active: bool,
        hit_count: u32,
    }

    let user32 = unsafe { LoadLibraryW(w!("user32.dll")).map_err(|e| format!("{e}"))? };
    let resolve = |name: &'static str, lib: HMODULE, sym: &str| -> Option<BpTarget> {
        let addr = unsafe {
            GetProcAddress(lib, windows::core::PCSTR(format!("{sym}\0").as_ptr()))
                .map(|f| f as usize)
        };
        addr.map(|a| {
            BpTarget { name, addr: a, orig_byte: 0, active: false, hit_count: 0 }
        })
    };

    let mut targets: Vec<BpTarget> = Vec::new();

    // user32.dll functions to breakpoint on
    let user32_syms: &[&str] = &[
        "CreateWindowExW",
        "DialogBoxParamW",
        "DialogBoxIndirectParamW",
        "MessageBoxW",
        "MessageBoxExW",
        "MessageBoxIndirectW",
    ];
    for &sym in user32_syms {
        if let Some(t) = resolve(sym, user32, sym) {
            logs.push(format!("{} @ 0x{:x}", t.name, t.addr));
            targets.push(t);
        }
    }

    // comctl32.dll functions to breakpoint on
    if let Ok(comctl32) = unsafe { LoadLibraryW(w!("comctl32.dll")) } {
        let comctl32_syms: &[&str] = &["TaskDialog", "TaskDialogIndirect"];
        for &sym in comctl32_syms {
            if let Some(t) = resolve(sym, comctl32, sym) {
                logs.push(format!("{} @ 0x{:x}", t.name, t.addr));
                targets.push(t);
            }
        }
    }

    // Spawn mstsc /l as debuggee
    let mut cmd_line: Vec<u16> = OsStr::new("mstsc.exe /l")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    unsafe {
        CreateProcessW(
            None,
            PWSTR(cmd_line.as_mut_ptr()),
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
    .map_err(|e| format!("CreateProcessW: {e}"))?;

    let h_process = pi.hProcess;
    let h_thread = pi.hThread;
    let diag_pid = pi.dwProcessId;
    logs.push(format!("mstsc PID={}", diag_pid));

    // Breakpoint state
    let mut single_step_tid: Option<u32> = None;
    let mut single_step_bp_idx: Option<usize> = None;
    let mut initial_bp = false;
    let mut event_count = 0u32;
    let mut dll_count = 0u32;

    // Module tracking: (base, name)
    let mut modules: Vec<(usize, String)> = Vec::new();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);

    loop {
        if std::time::Instant::now() > deadline {
            logs.push("Timeout (12s)".into());
            break;
        }

        let mut event: DEBUG_EVENT = unsafe { std::mem::zeroed() };
        if unsafe { WaitForDebugEvent(&mut event, 200) }.is_err() {
            continue;
        }
        event_count += 1;

        let cs = match event.dwDebugEventCode {
            CREATE_PROCESS_DEBUG_EVENT => {
                let base = unsafe { event.u.CreateProcessInfo.lpBaseOfImage } as usize;
                modules.push((base, "mstsc.exe".into()));
                logs.push(format!("[{}] CREATE_PROCESS base=0x{:x}", event_count, base));
                unsafe {
                    let h = event.u.CreateProcessInfo.hFile;
                    if !h.is_invalid() {
                        let _ = CloseHandle(h);
                    }
                }
                DBG_CONTINUE
            }
            LOAD_DLL_DEBUG_EVENT => {
                dll_count += 1;
                let base = unsafe { event.u.LoadDll.lpBaseOfDll } as usize;
                // Read DLL name from debug info
                let name = read_dll_name_from_event(h_process, unsafe { &event.u.LoadDll });
                let dll_name = if name.is_empty() { format!("dll_{}", dll_count) } else { name };
                modules.push((base, dll_name.clone()));
                unsafe {
                    let h = event.u.LoadDll.hFile;
                    if !h.is_invalid() {
                        let _ = CloseHandle(h);
                    }
                }
                // When a DLL loads, try to set any pending breakpoints that weren't set yet
                if initial_bp {
                    for t in targets.iter_mut() {
                        if !t.active {
                            // Check if this target's address falls within the newly loaded DLL
                            // (heuristic: addr >= base and within 64MB)
                            let addr = t.addr;
                            if addr >= base && addr - base < 0x400_0000 {
                                let mut orig = [0u8; 1];
                                if read_remote(h_process, addr, &mut orig) {
                                    t.orig_byte = orig[0];
                                    let bp_byte = [0xCCu8; 1];
                                    let mut old_prot = PAGE_PROTECTION_FLAGS(0);
                                    unsafe {
                                        let _ = VirtualProtectEx(h_process, addr as *const _, 1, PAGE_EXECUTE_READWRITE, &mut old_prot);
                                        let _ = WriteProcessMemory(h_process, addr as *mut _, bp_byte.as_ptr() as *const _, 1, None);
                                        let _ = VirtualProtectEx(h_process, addr as *const _, 1, old_prot, &mut old_prot);
                                    }
                                    t.active = true;
                                    logs.push(format!("BP set on {} (late, after {} loaded)", t.name, dll_name));
                                }
                            }
                        }
                    }
                }
                DBG_CONTINUE
            }
            EXCEPTION_DEBUG_EVENT => {
                let code = unsafe { event.u.Exception.ExceptionRecord.ExceptionCode };
                let exc_addr =
                    unsafe { event.u.Exception.ExceptionRecord.ExceptionAddress } as usize;

                if code.0 == 0x80000003u32 as i32 {
                    // STATUS_BREAKPOINT
                    if !initial_bp {
                        initial_bp = true;
                        logs.push(format!(
                            "[{}] Initial BP ({} DLLs)",
                            event_count, dll_count
                        ));
                        // Set breakpoints on all targets
                        for t in targets.iter_mut() {
                            let mut orig = [0u8; 1];
                            if read_remote(h_process, t.addr, &mut orig) {
                                t.orig_byte = orig[0];
                                let bp_byte = [0xCCu8; 1];
                                let mut old_prot = PAGE_PROTECTION_FLAGS(0);
                                unsafe {
                                    let _ = VirtualProtectEx(h_process, t.addr as *const _, 1, PAGE_EXECUTE_READWRITE, &mut old_prot);
                                    let _ = WriteProcessMemory(h_process, t.addr as *mut _, bp_byte.as_ptr() as *const _, 1, None);
                                    let _ = VirtualProtectEx(h_process, t.addr as *const _, 1, old_prot, &mut old_prot);
                                }
                                t.active = true;
                                logs.push(format!("BP set on {}", t.name));
                            }
                        }
                    } else if let Some(bp_idx) = targets.iter().position(|t| t.active && t.addr == exc_addr) {
                        // One of our breakpoints hit!
                        targets[bp_idx].hit_count += 1;
                        let bp_name = targets[bp_idx].name;
                        let bp_hit = targets[bp_idx].hit_count;

                        // Get thread context for params + stack
                        if let Ok(h_thread) = unsafe {
                            OpenThread(
                                THREAD_ACCESS_RIGHTS(0x001A), // GET_CONTEXT|SET_CONTEXT|SUSPEND_RESUME
                                false,
                                event.dwThreadId,
                            )
                        } {
                            let mut ctx = ContextAmd64([0u8; 1232]);
                            ctx_set_u32(&mut ctx, CTX_FLAGS, 0x10_0003); // CONTROL | INTEGER

                            let got = unsafe { GetThreadContext(h_thread, &mut ctx) };
                            if got.as_bool() {
                                let rcx = ctx_u64(&ctx, CTX_RCX);
                                let rdx = ctx_u64(&ctx, CTX_RDX);
                                let r8 = ctx_u64(&ctx, CTX_R8);
                                let rsp = ctx_u64(&ctx, CTX_RSP);

                                // Return address from [RSP]
                                let mut ret_buf = [0u8; 8];
                                let _ = read_remote(h_process, rsp as usize, &mut ret_buf);
                                let ret_addr = u64::from_le_bytes(ret_buf);
                                let ret_mod = find_module(ret_addr as usize, &modules);

                                if bp_name == "CreateWindowExW" {
                                    // RCX=dwExStyle, RDX=lpClassName, R8=lpWindowName
                                    let class_name = if rdx < 0x10000 {
                                        format!("ATOM(0x{:x})", rdx)
                                    } else {
                                        read_remote_wstr(h_process, rdx as usize, 128)
                                    };
                                    let wnd_text = if r8 == 0 {
                                        "<null>".into()
                                    } else {
                                        read_remote_wstr(h_process, r8 as usize, 256)
                                    };
                                    logs.push(format!(
                                        "[{}] {} #{}: class=\"{}\" text=\"{}\" ret=0x{:x}({})",
                                        event_count, bp_name, bp_hit, class_name, wnd_text, ret_addr, ret_mod
                                    ));

                                    // For calls with non-empty window text, dump raw stack
                                    if !wnd_text.is_empty() && wnd_text != "<null>" {
                                        logs.push(format!("  >>> Window with text: \"{}\"", wnd_text));
                                        let mut stack_buf = [0u8; 160];
                                        let _ = read_remote(h_process, rsp as usize, &mut stack_buf);
                                        for i in 0..20 {
                                            let addr = u64::from_le_bytes(stack_buf[i*8..(i+1)*8].try_into().unwrap());
                                            let m = find_module(addr as usize, &modules);
                                            if !m.is_empty() {
                                                logs.push(format!("    RSP+0x{:02x}: 0x{:016x} {}", i*8, addr, m));
                                            }
                                        }
                                    }
                                } else {
                                    // Dialog function hit! Log with full stack trace
                                    let r9 = ctx_u64(&ctx, CTX_R9);
                                    // Read 5th arg from [RSP+0x28]
                                    let mut arg5_buf = [0u8; 8];
                                    let _ = read_remote(h_process, (rsp + 0x28) as usize, &mut arg5_buf);
                                    let arg5 = u64::from_le_bytes(arg5_buf);

                                    logs.push(format!(
                                        "[{}] >>> {} #{}: RCX=0x{:x} RDX=0x{:x} R8=0x{:x} R9=0x{:x} [RSP+0x28]=0x{:x} ret=0x{:x}({})",
                                        event_count, bp_name, bp_hit, rcx, rdx, r8, r9, arg5, ret_addr, ret_mod
                                    ));

                                    // Try to read potential wide string args
                                    // TaskDialog: (hWndOwner, hInstance, pszWindowTitle, pszMainInstruction, pszContent, ...)
                                    //   RCX=hWndOwner RDX=hInstance R8=pszWindowTitle R9=pszMainInstruction [RSP+0x28]=pszContent
                                    // MessageBoxW: (hWnd, lpText, lpCaption, uType)
                                    //   RCX=hWnd RDX=lpText R8=lpCaption
                                    for (label, ptr) in [
                                        ("RDX as wstr", rdx),
                                        ("R8 as wstr", r8),
                                        ("R9 as wstr", r9),
                                        ("[RSP+0x28] as wstr", arg5),
                                    ] {
                                        if ptr > 0x10000 {
                                            let s = read_remote_wstr(h_process, ptr as usize, 512);
                                            if !s.starts_with("<read fail") && !s.is_empty() {
                                                logs.push(format!("    {} = \"{}\"", label, s));
                                            }
                                        }
                                    }

                                    // Full stack dump for dialog calls
                                    let mut stack_buf = [0u8; 512]; // 64 slots
                                    let _ = read_remote(h_process, rsp as usize, &mut stack_buf);
                                    for i in 0..64 {
                                        let addr = u64::from_le_bytes(stack_buf[i*8..(i+1)*8].try_into().unwrap());
                                        let m = find_module(addr as usize, &modules);
                                        if !m.is_empty() {
                                            logs.push(format!("    RSP+0x{:02x}: 0x{:016x} {}", i*8, addr, m));
                                        }
                                    }

                                    // For DialogBoxIndirectParamW: deep scan for TASKDIALOGCONFIG
                                    // Strategy:
                                    //   1. Collect all unique heap pointers from the stack
                                    //   2. For each heap pointer, try reading as TASKDIALOGCONFIG (check cbSize)
                                    //   3. Also scan dwInitParam (5th arg) structure for embedded pointers
                                    //   4. For each heap pointer, try reading at offset 0x38 as wide string (pszContent)
                                    //   5. Also try every pointer as a direct wide string
                                    if bp_name == "DialogBoxIndirectParamW" {
                                        logs.push("  --- Deep TASKDIALOGCONFIG scan ---".into());

                                        // Collect all unique heap pointers from stack
                                        let mut heap_ptrs: Vec<(String, u64)> = Vec::new();
                                        for i in 0..64 {
                                            let candidate = u64::from_le_bytes(
                                                stack_buf[i*8..(i+1)*8].try_into().unwrap()
                                            );
                                            if candidate < 0x10000 || candidate > 0x7FFF_FFFF_FFFF {
                                                continue;
                                            }
                                            let m = find_module(candidate as usize, &modules);
                                            if !m.contains("0x") {
                                                continue; // skip code addresses
                                            }
                                            let label = format!("RSP+0x{:02x}", i * 8);
                                            if !heap_ptrs.iter().any(|(_, p)| *p == candidate) {
                                                heap_ptrs.push((label, candidate));
                                            }
                                        }

                                        logs.push(format!("  Found {} unique heap pointers on stack", heap_ptrs.len()));

                                        // Phase 1: Try each heap pointer as direct wide string AND as struct
                                        for (label, candidate) in &heap_ptrs {
                                            // First: try reading the pointer itself as a wide string
                                            let direct_str = read_remote_wstr(h_process, *candidate as usize, 4096);
                                            if !direct_str.starts_with("<read fail") && !direct_str.is_empty() {
                                                // Check if it looks like printable text (not random bytes)
                                                let printable_ratio = direct_str.chars()
                                                    .take(50)
                                                    .filter(|c| c.is_ascii_graphic() || c.is_ascii_whitespace() || *c == '\n' || *c == '\r')
                                                    .count() as f64
                                                    / direct_str.chars().take(50).count().max(1) as f64;
                                                if printable_ratio > 0.6 && direct_str.len() > 3 {
                                                    logs.push(format!(
                                                        "  *** {} -> 0x{:x} DIRECT WSTR ({}ch, {:.0}% printable): \"{}\"",
                                                        label, candidate, direct_str.len(), printable_ratio * 100.0,
                                                        &direct_str[..direct_str.len().min(500)]
                                                    ));
                                                }
                                            }

                                            // Then: try reading as struct (log offset 0x38 etc.)
                                            let mut config_buf = [0u8; 0x48];
                                            if !read_remote(h_process, *candidate as usize, &mut config_buf) {
                                                continue;
                                            }
                                            let cb_size = u32::from_le_bytes(
                                                config_buf[0..4].try_into().unwrap()
                                            );
                                            let psz_content = u64::from_le_bytes(
                                                config_buf[0x38..0x40].try_into().unwrap()
                                            );
                                            let psz_title = u64::from_le_bytes(
                                                config_buf[0x20..0x28].try_into().unwrap()
                                            );

                                            // Only log struct details for non-code addresses with interesting cbSize
                                            if cb_size >= 0x80 && cb_size <= 0x200 {
                                                logs.push(format!(
                                                    "  {} -> 0x{:x}: cbSize=0x{:x}({}) +0x20=0x{:x} +0x38=0x{:x}",
                                                    label, candidate, cb_size, cb_size, psz_title, psz_content
                                                ));
                                                if psz_content > 0x10000 && psz_content < 0x7FFF_FFFF_FFFF {
                                                    let content = read_remote_wstr(h_process, psz_content as usize, 2048);
                                                    if !content.starts_with("<read fail") && !content.is_empty() {
                                                        logs.push(format!("    +0x38 as wstr = \"{}\"", &content[..content.len().min(200)]));
                                                    }
                                                }
                                            }
                                        }

                                        // Phase 2: Read dwInitParam (5th arg) as a structure and scan its pointers
                                        let dw_init_param = u64::from_le_bytes(
                                            stack_buf[5*8..6*8].try_into().unwrap()  // [RSP+0x28]
                                        );
                                        if dw_init_param > 0x10000 && dw_init_param < 0x7FFF_FFFF_FFFF {
                                            logs.push(format!("  --- dwInitParam scan (0x{:x}) ---", dw_init_param));
                                            let mut init_buf = [0u8; 0x200];
                                            if read_remote(h_process, dw_init_param as usize, &mut init_buf) {
                                                // Dump first 32 pointers (256 bytes) of dwInitParam
                                                for j in 0..32 {
                                                    let ptr = u64::from_le_bytes(
                                                        init_buf[j*8..(j+1)*8].try_into().unwrap()
                                                    );
                                                    if ptr < 0x10000 || ptr > 0x7FFF_FFFF_FFFF {
                                                        continue;
                                                    }
                                                    let m = find_module(ptr as usize, &modules);
                                                    // Try reading as wide string
                                                    let s = read_remote_wstr(h_process, ptr as usize, 1024);
                                                    let s_preview = if !s.starts_with("<read fail") && !s.is_empty() {
                                                        format!(" wstr=\"{}\"", &s[..s.len().min(200)])
                                                    } else {
                                                        String::new()
                                                    };
                                                    logs.push(format!(
                                                        "    +0x{:02x}: 0x{:x} [{}]{}",
                                                        j * 8, ptr, m, s_preview
                                                    ));

                                                    // If this pointer is heap-like, also try reading IT
                                                    // as a structure with pszContent at offset 0x38
                                                    if m.contains("0x") {
                                                        let mut inner_buf = [0u8; 0x48];
                                                        if read_remote(h_process, ptr as usize, &mut inner_buf) {
                                                            let inner_0x38 = u64::from_le_bytes(
                                                                inner_buf[0x38..0x40].try_into().unwrap()
                                                            );
                                                            if inner_0x38 > 0x10000 && inner_0x38 < 0x7FFF_FFFF_FFFF {
                                                                let cs = read_remote_wstr(h_process, inner_0x38 as usize, 2048);
                                                                if !cs.starts_with("<read fail") && !cs.is_empty() {
                                                                    logs.push(format!(
                                                                        "      ->+0x38 wstr = \"{}\"",
                                                                        &cs[..cs.len().min(200)]
                                                                    ));
                                                                }
                                                            }
                                                        }
                                                    }
                                                }

                                                // Also: scan dwInitParam for pointers to TASKDIALOGCONFIG
                                                // by checking each embedded pointer's cbSize
                                                logs.push("  --- dwInitParam embedded struct scan ---".into());
                                                for j in 0..64 {
                                                    if j * 8 + 8 > init_buf.len() { break; }
                                                    let ptr = u64::from_le_bytes(
                                                        init_buf[j*8..(j+1)*8].try_into().unwrap()
                                                    );
                                                    if ptr < 0x10000 || ptr > 0x7FFF_FFFF_FFFF {
                                                        continue;
                                                    }
                                                    let m = find_module(ptr as usize, &modules);
                                                    if !m.contains("0x") { continue; }
                                                    let mut cb_buf = [0u8; 0x48];
                                                    if !read_remote(h_process, ptr as usize, &mut cb_buf) {
                                                        continue;
                                                    }
                                                    let cb = u32::from_le_bytes(cb_buf[0..4].try_into().unwrap());
                                                    if cb >= 0x80 && cb <= 0x200 {
                                                        let p38 = u64::from_le_bytes(cb_buf[0x38..0x40].try_into().unwrap());
                                                        logs.push(format!(
                                                            "    dwInit+0x{:02x} -> 0x{:x}: cbSize=0x{:x}({}) +0x38=0x{:x}",
                                                            j*8, ptr, cb, cb, p38
                                                        ));
                                                        if p38 > 0x10000 && p38 < 0x7FFF_FFFF_FFFF {
                                                            let cs = read_remote_wstr(h_process, p38 as usize, 2048);
                                                            if !cs.starts_with("<read fail") && !cs.is_empty() {
                                                                logs.push(format!("      pszContent = \"{}\"", &cs[..cs.len().min(200)]));
                                                            }
                                                        }
                                                    }
                                                }
                                            } else {
                                                logs.push("  dwInitParam read failed".into());
                                            }
                                        }
                                    }
                                }

                                // Restore original byte, set RIP back, enable single-step
                                let bp_addr = targets[bp_idx].addr;
                                let bp_orig = targets[bp_idx].orig_byte;
                                let mut old_prot = PAGE_PROTECTION_FLAGS(0);
                                unsafe {
                                    let _ = VirtualProtectEx(h_process, bp_addr as *const _, 1, PAGE_EXECUTE_READWRITE, &mut old_prot);
                                    let _ = WriteProcessMemory(h_process, bp_addr as *mut _, &bp_orig as *const u8 as *const _, 1, None);
                                    let _ = VirtualProtectEx(h_process, bp_addr as *const _, 1, old_prot, &mut old_prot);
                                }
                                ctx_set_u64(&mut ctx, CTX_RIP, bp_addr as u64);
                                let eflags = ctx_u32(&ctx, CTX_EFLAGS) | 0x100; // Trap Flag
                                ctx_set_u32(&mut ctx, CTX_EFLAGS, eflags);
                                let _ = unsafe { SetThreadContext(h_thread, &ctx) };
                                single_step_tid = Some(event.dwThreadId);
                                single_step_bp_idx = Some(bp_idx);
                            } else {
                                logs.push(format!("  GetThreadContext failed for tid={}", event.dwThreadId));
                            }
                            unsafe {
                                let _ = CloseHandle(h_thread);
                            }
                        }
                    }
                    DBG_CONTINUE
                } else if code.0 == 0x80000004u32 as i32 {
                    // STATUS_SINGLE_STEP
                    if single_step_tid == Some(event.dwThreadId) {
                        // Re-set breakpoint on the target that was single-stepped
                        if let Some(idx) = single_step_bp_idx {
                            let bp_addr = targets[idx].addr;
                            let bp_byte = [0xCCu8; 1];
                            let mut old_prot = PAGE_PROTECTION_FLAGS(0);
                            unsafe {
                                let _ = VirtualProtectEx(h_process, bp_addr as *const _, 1, PAGE_EXECUTE_READWRITE, &mut old_prot);
                                let _ = WriteProcessMemory(h_process, bp_addr as *mut _, bp_byte.as_ptr() as *const _, 1, None);
                                let _ = VirtualProtectEx(h_process, bp_addr as *const _, 1, old_prot, &mut old_prot);
                            }
                        }
                        single_step_tid = None;
                        single_step_bp_idx = None;
                        DBG_CONTINUE
                    } else {
                        DBG_EXCEPTION_NOT_HANDLED
                    }
                } else {
                    DBG_EXCEPTION_NOT_HANDLED
                }
            }
            EXIT_PROCESS_DEBUG_EVENT => {
                let exit_code = unsafe { event.u.ExitProcess.dwExitCode };
                logs.push(format!(
                    "[{}] EXIT_PROCESS (code={})",
                    event_count, exit_code
                ));
                unsafe {
                    let _ = ContinueDebugEvent(
                        event.dwProcessId,
                        event.dwThreadId,
                        DBG_CONTINUE,
                    );
                }
                break;
            }
            _ => DBG_CONTINUE,
        };

        unsafe {
            let _ = ContinueDebugEvent(event.dwProcessId, event.dwThreadId, cs);
        }
    }

    logs.push(format!("\nTotal: {} events, {} DLLs", event_count, dll_count));
    for t in &targets {
        logs.push(format!("  {}: {} hits", t.name, t.hit_count));
    }

    // Log loaded modules sorted by base
    modules.sort_by_key(|(base, _)| *base);
    logs.push(format!("\nModules ({}):", modules.len()));
    for (base, name) in &modules {
        logs.push(format!("  0x{:016x} {}", base, name));
    }

    // Clean up debug session: terminate, drain events, close handles
    unsafe {
        use windows::Win32::System::Threading::*;
        let _ = TerminateProcess(h_process, 1);
        let drain_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if std::time::Instant::now() > drain_deadline { break; }
            let mut ev: DEBUG_EVENT = std::mem::zeroed();
            if WaitForDebugEvent(&mut ev, 500).is_err() { break; }
            let is_exit = ev.dwDebugEventCode == EXIT_PROCESS_DEBUG_EVENT;
            match ev.dwDebugEventCode {
                CREATE_PROCESS_DEBUG_EVENT => {
                    let info = ev.u.CreateProcessInfo;
                    if !info.hFile.is_invalid() { let _ = CloseHandle(info.hFile); }
                }
                LOAD_DLL_DEBUG_EVENT => {
                    let info = ev.u.LoadDll;
                    if !info.hFile.is_invalid() { let _ = CloseHandle(info.hFile); }
                }
                _ => {}
            }
            let _ = ContinueDebugEvent(ev.dwProcessId, ev.dwThreadId, DBG_CONTINUE);
            if is_exit { break; }
        }
        let _ = WaitForSingleObject(h_process, 1000);
        let _ = CloseHandle(h_process);
        let _ = CloseHandle(h_thread);
    }

    Ok(CaptureResult {
        raw_text: String::new(),
        monitors: Vec::new(),
        logs,
    })
}

/// Read a wide string from remote process memory.
#[cfg(target_os = "windows")]
fn read_remote_wstr(
    h_process: windows::Win32::Foundation::HANDLE,
    addr: usize,
    max_chars: usize,
) -> String {
    if addr == 0 {
        return "<null>".into();
    }
    let mut buf = vec![0u16; max_chars];
    let byte_len = max_chars * 2;
    let mut read = 0usize;
    let ok = unsafe {
        windows::Win32::System::Diagnostics::Debug::ReadProcessMemory(
            h_process,
            addr as *const _,
            buf.as_mut_ptr() as *mut _,
            byte_len,
            Some(&mut read),
        )
        .is_ok()
    };
    if !ok {
        return format!("<read fail@0x{:x}>", addr);
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(read / 2);
    String::from_utf16_lossy(&buf[..len])
}

/// Read DLL name from LOAD_DLL_DEBUG_INFO (reads pointer-to-name from debuggee).
#[cfg(target_os = "windows")]
fn read_dll_name_from_event(
    h_process: windows::Win32::Foundation::HANDLE,
    info: &windows::Win32::System::Diagnostics::Debug::LOAD_DLL_DEBUG_INFO,
) -> String {
    let name_ptr_addr = info.lpImageName as usize;
    if name_ptr_addr == 0 {
        return String::new();
    }
    // Read pointer to name string
    let mut name_ptr: u64 = 0;
    if !read_remote(
        h_process,
        name_ptr_addr,
        unsafe { std::slice::from_raw_parts_mut(&mut name_ptr as *mut u64 as *mut u8, 8) },
    ) {
        return String::new();
    }
    if name_ptr == 0 {
        return String::new();
    }
    if info.fUnicode != 0 {
        let s = read_remote_wstr(h_process, name_ptr as usize, 260);
        // Extract just filename
        s.rsplit('\\').next().unwrap_or(&s).to_string()
    } else {
        let mut buf = vec![0u8; 260];
        if !read_remote(h_process, name_ptr as usize, &mut buf) {
            return String::new();
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(260);
        let s = String::from_utf8_lossy(&buf[..len]).into_owned();
        s.rsplit('\\').next().unwrap_or(&s).to_string()
    }
}

/// Find which module an address belongs to (heuristic: largest base <= addr within 64MB).
#[cfg(target_os = "windows")]
fn find_module(addr: usize, modules: &[(usize, String)]) -> String {
    let mut best: Option<&(usize, String)> = None;
    for m in modules {
        if m.0 <= addr {
            if best.is_none() || m.0 > best.unwrap().0 {
                best = Some(m);
            }
        }
    }
    if let Some((base, name)) = best {
        if addr - base < 0x400_0000 {
            return format!("{}+0x{:x}", name, addr - base);
        }
    }
    format!("0x{:x}", addr)
}

/// Non-diagnostic version for connect/preflight.
/// Returns (raw_text, monitors, used_fallback).
#[cfg(target_os = "windows")]
fn capture_mstsc_silent() -> Result<(String, Vec<LiveMonitor>, bool), String> {
    // Primary: breakpoint-based capture (silent, no dialog/sound)
    let mut _logs = Vec::new();
    match capture_mstsc_bp(&mut _logs) {
        Ok((text, monitors)) => Ok((text, monitors, false)),
        Err(_bp_err) => {
            // Fallback: old inline-hook approach (may show brief flash)
            match capture_debug_target("mstsc.exe /l") {
                Ok(result) if !result.monitors.is_empty() => {
                    Ok((result.raw_text, result.monitors, true))
                }
                _ => Err("Both BP and hook capture failed".into()),
            }
        }
    }
}

/// Check if text looks like mstsc /l monitor output.
/// Expected format: "0: 1920 x 1200; (3840, 241, 5759, 1440)\n..."
fn looks_like_monitor_text(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim();
        line.len() > 5
            && line.chars().next().map_or(false, |c| c.is_ascii_digit())
            && (line.contains(':') || line.contains(';'))
            && line.contains('(')
            && line.contains(')')
    })
}

/// Capture mstsc /l output via DialogBoxIndirectParamW breakpoint.
/// Sets a single int3 BP, reads the monitor text from the stack, terminates mstsc.
/// No dialog is shown, no sound is played.
#[cfg(target_os = "windows")]
fn capture_mstsc_bp(logs: &mut Vec<String>) -> Result<(String, Vec<LiveMonitor>), String> {
    // Retry wrapper: if mstsc exits immediately (likely stale debug port), retry once
    for attempt in 0..3 {
        let result = capture_mstsc_bp_inner(logs);
        match &result {
            Err(e) if e.contains("process_exit") && attempt < 2 => {
                logs.push(format!("--- Retry #{} (mstsc exited prematurely) ---", attempt + 1));
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
            _ => return result,
        }
    }
    unreachable!()
}

fn capture_mstsc_bp_inner(logs: &mut Vec<String>) -> Result<(String, Vec<LiveMonitor>), String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::*;
    use windows::Win32::System::Diagnostics::Debug::*;
    use windows::Win32::System::Memory::*;
    use windows::Win32::System::Threading::*;
    use windows::Win32::System::LibraryLoader::*;
    use windows::core::*;

    #[repr(C, align(16))]
    struct Ctx([u8; 1232]);
    extern "system" {
        fn GetThreadContext(hThread: HANDLE, lpContext: *mut Ctx) -> BOOL;
    }
    const CTX_FLAGS: usize = 0x30;
    const CTX_RSP: usize = 0x98;
    const CTX_RIP: usize = 0xF8;
    fn ctx_u64(ctx: &Ctx, off: usize) -> u64 {
        u64::from_le_bytes(ctx.0[off..off + 8].try_into().unwrap())
    }

    // 1. Resolve DialogBoxIndirectParamW address (same base across processes)
    let dbip_addr: usize = unsafe {
        let user32 = LoadLibraryW(w!("user32.dll"))
            .map_err(|e| format!("LoadLibrary user32: {e}"))?;
        let proc = GetProcAddress(user32, s!("DialogBoxIndirectParamW"))
            .ok_or("GetProcAddress(DialogBoxIndirectParamW) failed")?;
        let addr = proc as usize;
        let _ = FreeLibrary(user32);
        addr
    };
    logs.push(format!("DialogBoxIndirectParamW @ 0x{:X}", dbip_addr));

    // 2. Spawn mstsc /l as debuggee
    let mut cmd_line: Vec<u16> = OsStr::new("mstsc.exe /l")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    unsafe {
        CreateProcessW(
            None,
            PWSTR(cmd_line.as_mut_ptr()),
            None,
            None,
            false,
            DEBUG_PROCESS | CREATE_NEW_CONSOLE,
            None,
            None,
            &si,
            &mut pi,
        )
        .map_err(|e| format!("CreateProcess mstsc: {e}"))?;
    }
    let pid = pi.dwProcessId;
    let h_process = pi.hProcess;
    let h_thread = pi.hThread;
    logs.push(format!("mstsc.exe PID={}", pid));

    // 3. Debug loop — wait for DialogBoxIndirectParamW BP, read monitor text
    let mut initial_bp = false;
    let mut bp_set = false;
    let mut orig_byte = 0u8;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut result_text: Option<String> = None;
    let mut event_count = 0u32;
    let mut dll_count = 0u32;
    let mut bp_exception_count = 0u32;
    let mut other_exception_count = 0u32;
    #[allow(unused_assignments)]
    let mut exit_reason = "";

    // Helper closure: try to set int3 BP at dbip_addr, verify it was written
    let try_set_bp = |h_process: HANDLE, orig: &mut u8, already_set: &mut bool, logs: &mut Vec<String>, context: &str| {
        if *already_set { return; }
        unsafe {
            let mut old_prot = PAGE_PROTECTION_FLAGS(0);
            if ReadProcessMemory(
                h_process, dbip_addr as *const _,
                orig as *mut u8 as *mut _, 1, None,
            ).is_err() {
                logs.push(format!("[{}] ReadProcessMemory failed at 0x{:X} — page not mapped", context, dbip_addr));
                return;
            }
            logs.push(format!("[{}] orig byte at 0x{:X} = 0x{:02X}", context, dbip_addr, *orig));
            let _ = VirtualProtectEx(
                h_process, dbip_addr as *const _, 1,
                PAGE_EXECUTE_READWRITE, &mut old_prot,
            );
            let bp = [0xCCu8];
            let write_ok = WriteProcessMemory(
                h_process, dbip_addr as *mut _,
                bp.as_ptr() as *const _, 1, None,
            ).is_ok();
            let _ = VirtualProtectEx(
                h_process, dbip_addr as *const _, 1,
                old_prot, &mut old_prot,
            );
            // Verify the BP was actually written
            let mut check = [0u8; 1];
            if ReadProcessMemory(
                h_process, dbip_addr as *const _,
                check.as_mut_ptr() as *mut _, 1, None,
            ).is_ok() && check[0] == 0xCC {
                *already_set = true;
                logs.push(format!("[{}] BP set OK (write={}, verify=0xCC)", context, write_ok));
            } else {
                logs.push(format!("[{}] BP FAILED (write={}, verify=0x{:02X})", context, write_ok, check[0]));
            }
        }
    };

    loop {
        if std::time::Instant::now() > deadline {
            exit_reason = "timeout";
            break;
        }

        let mut event: DEBUG_EVENT = unsafe { std::mem::zeroed() };
        if unsafe { WaitForDebugEvent(&mut event, 300).is_err() } {
            continue;
        }
        event_count += 1;

        let cont_status = match event.dwDebugEventCode {
            CREATE_PROCESS_DEBUG_EVENT => {
                let info = unsafe { event.u.CreateProcessInfo };
                if !info.hFile.is_invalid() {
                    unsafe { let _ = CloseHandle(info.hFile); }
                }
                DBG_CONTINUE
            }
            LOAD_DLL_DEBUG_EVENT => {
                let info = unsafe { event.u.LoadDll };
                if !info.hFile.is_invalid() {
                    unsafe { let _ = CloseHandle(info.hFile); }
                }
                dll_count += 1;
                // Try setting BP on each DLL load in case user32.dll just loaded
                if initial_bp && !bp_set {
                    try_set_bp(h_process, &mut orig_byte, &mut bp_set, logs, &format!("dll#{}", dll_count));
                }
                DBG_CONTINUE
            }
            EXCEPTION_DEBUG_EVENT => {
                let info = unsafe { event.u.Exception };
                let code = info.ExceptionRecord.ExceptionCode;

                if code.0 == 0x80000003u32 as i32 {
                    bp_exception_count += 1;
                    // STATUS_BREAKPOINT
                    if !initial_bp {
                        // Initial breakpoint — try to set our BP
                        initial_bp = true;
                        logs.push(format!("Initial BP (event#{}, dll_count={})", event_count, dll_count));
                        try_set_bp(h_process, &mut orig_byte, &mut bp_set, logs, "initial_bp");
                        DBG_CONTINUE
                    } else if bp_set {
                        // Possible hit on our BP
                        let h_thread = unsafe {
                            OpenThread(THREAD_ALL_ACCESS, false, event.dwThreadId)
                        };
                        if let Ok(h_thread) = h_thread {
                            let mut ctx = Ctx([0u8; 1232]);
                            ctx.0[CTX_FLAGS..CTX_FLAGS + 4]
                                .copy_from_slice(&0x0010001Fu32.to_le_bytes());
                            let ok = unsafe { GetThreadContext(h_thread, &mut ctx).as_bool() };
                            if ok {
                                let rip = ctx_u64(&ctx, CTX_RIP);
                                if rip.wrapping_sub(1) == dbip_addr as u64 {
                                    // DialogBoxIndirectParamW BP hit!
                                    logs.push(format!("BP HIT! RIP=0x{:X} (event#{})", rip, event_count));
                                    let rsp = ctx_u64(&ctx, CTX_RSP);
                                    logs.push(format!("RSP=0x{:X}", rsp));

                                    // Try RSP+0x78 first (known good offset for current comctl32)
                                    if let Some(t) = try_read_stack_wstr(h_process, rsp, 0x78) {
                                        if looks_like_monitor_text(&t) {
                                            logs.push(format!("RSP+0x78 -> monitor text ({} chars)", t.len()));
                                            result_text = Some(t);
                                        } else {
                                            logs.push(format!("RSP+0x78 -> not monitor text: {:?}", &t[..t.len().min(80)]));
                                        }
                                    } else {
                                        logs.push("RSP+0x78 -> null/unreadable".into());
                                    }

                                    // Fallback: scan all stack slots for monitor text
                                    if result_text.is_none() {
                                        logs.push("Scanning stack slots 0x00..0x1F8...".into());
                                        for off in (0x00..=0x1F8).step_by(8) {
                                            if off == 0x78 { continue; }
                                            if let Some(t) = try_read_stack_wstr(h_process, rsp, off) {
                                                if looks_like_monitor_text(&t) {
                                                    logs.push(format!("RSP+0x{:X} -> monitor text ({} chars)", off, t.len()));
                                                    result_text = Some(t);
                                                    break;
                                                }
                                            }
                                        }
                                        if result_text.is_none() {
                                            logs.push("No monitor text found in any stack slot".into());
                                        }
                                    }
                                } else {
                                    logs.push(format!("BP exception but RIP=0x{:X} != expected 0x{:X}+1, tid={}", rip, dbip_addr, event.dwThreadId));
                                }
                            } else {
                                logs.push("GetThreadContext failed".into());
                            }
                            unsafe { let _ = CloseHandle(h_thread); }
                        } else {
                            logs.push(format!("OpenThread failed for tid={}", event.dwThreadId));
                        }

                        if result_text.is_some() {
                            exit_reason = "success";
                            unsafe {
                                let _ = ContinueDebugEvent(
                                    event.dwProcessId, event.dwThreadId, DBG_CONTINUE,
                                );
                            }
                            break;
                        }
                        DBG_CONTINUE
                    } else {
                        logs.push(format!("BP exception but bp_set=false (event#{}, tid={})", event_count, event.dwThreadId));
                        // BP not yet set — this might be a child thread initial breakpoint
                        // Try to set BP again now
                        try_set_bp(h_process, &mut orig_byte, &mut bp_set, logs, "extra_bp");
                        DBG_CONTINUE
                    }
                } else {
                    other_exception_count += 1;
                    logs.push(format!("Exception 0x{:08X} (event#{}, first_chance={})",
                        code.0 as u32, event_count, info.dwFirstChance));
                    // Other exception — pass through
                    DBG_EXCEPTION_NOT_HANDLED
                }
            }
            EXIT_PROCESS_DEBUG_EVENT => {
                exit_reason = "process_exit";
                let exit_code = unsafe { event.u.ExitProcess.dwExitCode };
                logs.push(format!("Process exited code={} (event#{})", exit_code, event_count));
                unsafe {
                    let _ = ContinueDebugEvent(
                        event.dwProcessId, event.dwThreadId, DBG_CONTINUE,
                    );
                }
                break;
            }
            _ => DBG_CONTINUE,
        };

        unsafe {
            let _ = ContinueDebugEvent(event.dwProcessId, event.dwThreadId, cont_status);
        }
    }

    logs.push(format!(
        "Loop done: exit={}, events={}, dlls={}, bp_exceptions={}, other_exceptions={}, bp_set={}",
        exit_reason, event_count, dll_count, bp_exception_count, other_exception_count, bp_set
    ));

    // Clean up debug session properly:
    // 1. Terminate the process (if still alive)
    // 2. Drain ALL remaining debug events until EXIT_PROCESS
    //    (this is critical — undrained events poison the per-thread debug port)
    // 3. Close handles
    unsafe {
        let _ = TerminateProcess(h_process, 1);
        // Drain remaining events (EXIT_THREAD, EXIT_PROCESS from termination)
        let drain_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut drained = 0u32;
        loop {
            if std::time::Instant::now() > drain_deadline { break; }
            let mut ev: DEBUG_EVENT = std::mem::zeroed();
            if WaitForDebugEvent(&mut ev, 500).is_err() { break; }
            drained += 1;
            let is_exit = ev.dwDebugEventCode == EXIT_PROCESS_DEBUG_EVENT;
            match ev.dwDebugEventCode {
                CREATE_PROCESS_DEBUG_EVENT => {
                    let info = ev.u.CreateProcessInfo;
                    if !info.hFile.is_invalid() { let _ = CloseHandle(info.hFile); }
                }
                LOAD_DLL_DEBUG_EVENT => {
                    let info = ev.u.LoadDll;
                    if !info.hFile.is_invalid() { let _ = CloseHandle(info.hFile); }
                }
                _ => {}
            }
            let _ = ContinueDebugEvent(ev.dwProcessId, ev.dwThreadId, DBG_CONTINUE);
            if is_exit { break; }
        }
        logs.push(format!("Drained {} cleanup events", drained));
        // Wait for process to fully terminate
        let _ = WaitForSingleObject(h_process, 1000);
        let _ = CloseHandle(h_process);
        let _ = CloseHandle(h_thread);
    }
    // Prevent ProcessGuard from double-closing (we already cleaned up)
    std::mem::forget(ProcessGuard { h_process, h_thread });

    match result_text {
        Some(text) => {
            let monitors = parse_mstsc_output(&text)?;
            Ok((text, monitors))
        }
        None => Err(format!("BP capture: no monitor text found ({})", exit_reason)),
    }
}

/// Try reading a stack slot as a pointer to a wide string.
#[cfg(target_os = "windows")]
fn try_read_stack_wstr(
    h_process: windows::Win32::Foundation::HANDLE,
    rsp: u64,
    offset: u64,
) -> Option<String> {
    let mut ptr_buf = [0u8; 8];
    if !read_remote(h_process, (rsp + offset) as usize, &mut ptr_buf) {
        return None;
    }
    let ptr = u64::from_le_bytes(ptr_buf);
    if ptr < 0x10000 || ptr > 0x7FFF_FFFF_FFFF {
        return None;
    }
    let s = read_remote_wstr(h_process, ptr as usize, 4096);
    if s.starts_with("<read fail") || s.starts_with("<null>") || s.is_empty() {
        return None;
    }
    Some(s)
}

/// Spawn a target as a debuggee, hook dialog functions via inline patching, capture text.
#[cfg(target_os = "windows")]
fn capture_debug_target(cmd: &str) -> Result<CaptureResult, String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::*;
    use windows::Win32::System::Diagnostics::Debug::*;
    use windows::Win32::System::Memory::*;
    use windows::Win32::System::Threading::*;

    let mut logs: Vec<String> = Vec::new();

    // Spawn target as a debuggee
    let mut cmd_line: Vec<u16> = OsStr::new(cmd)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;

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
    let cdt_pid = pi.dwProcessId;

    logs.push(format!("Process created: PID={}", cdt_pid));

    // Allocate remote buffer for captured text
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

    // Allocate and write shellcode (copies lpText from RDX to buffer, returns IDOK)
    let stub_mem = allocate_shellcode(h_process, remote_buf, &mut logs);

    // Per-process tracking: each debugged process needs its own buffer/shellcode/hooks
    struct ProcState {
        h_process: HANDLE,
        remote_buf: *mut std::ffi::c_void,
        stub_mem: *mut std::ffi::c_void,
        exe_base: usize,
        initial_bp_handled: bool,
        taskdlg_hooked: bool,
    }
    let mut procs: HashMap<u32, ProcState> = HashMap::new();
    // Register the initial process
    procs.insert(pi.dwProcessId, ProcState {
        h_process,
        remote_buf,
        stub_mem,
        exe_base: 0,
        initial_bp_handled: false,
        taskdlg_hooked: false,
    });

    let mut event_count = 0u32;
    let mut dll_count = 0u32;
    let mut _all_exited = false;
    let mut captured_bufs: Vec<Vec<u8>> = Vec::new();

    fn read_proc_sentinel(
        h_proc: HANDLE,
        buf: *mut std::ffi::c_void,
        buf_size: usize,
        pid: u32,
        logs: &mut Vec<String>,
        captured: &mut Vec<Vec<u8>>,
    ) {
        let mut sentinel_check = [0u8; 4];
        unsafe {
            let _ = ReadProcessMemory(h_proc, buf, sentinel_check.as_mut_ptr() as *mut _, 4, None);
        }
        let sentinel_val = u32::from_le_bytes(sentinel_check);
        if sentinel_val == 0xCAFEBABE {
            logs.push(format!("pid={}: Sentinel INTACT -> NO hook called", pid));
        } else {
            let marker = sentinel_check[0];
            let name = match marker {
                0x01 => "MessageBoxW",
                0x02 => "MessageBoxExW",
                0x03 => "MessageBoxIndirectW",
                0x04 => "MessageBoxA",
                0x05 => "MessageBoxExA",
                0x06 => "TaskDialogIndirect",
                0x07 => "DialogBoxParamW",
                _ => "UNKNOWN",
            };
            logs.push(format!("*** pid={}: CALLED {} (marker=0x{:02x}) ***", pid, name, marker));

            // Read full buffer for text extraction
            let mut local_buf = vec![0u8; buf_size];
            let mut bytes_read = 0usize;
            unsafe {
                let _ = ReadProcessMemory(h_proc, buf, local_buf.as_mut_ptr() as *mut _, buf_size, Some(&mut bytes_read));
            }
            local_buf.truncate(bytes_read);

            let hex_preview: String = local_buf[..bytes_read.min(32)]
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join(" ");
            logs.push(format!("  Buffer hex[0..32]: {}", hex_preview));

            captured.push(local_buf);
        }
        // Free remote memory
        unsafe {
            let _ = VirtualFreeEx(h_proc, buf, 0, MEM_RELEASE);
        }
    }

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
        let event_pid = event.dwProcessId;

        let continue_status = match event.dwDebugEventCode {
            CREATE_PROCESS_DEBUG_EVENT => {
                let base = unsafe { event.u.CreateProcessInfo.lpBaseOfImage } as usize;
                let child_h = unsafe { event.u.CreateProcessInfo.hProcess };
                logs.push(format!(
                    "[{}] CREATE_PROCESS pid={} base=0x{:x}{}",
                    event_count, event_pid, base,
                    if event_pid != pi.dwProcessId { " (CHILD)" } else { "" }
                ));
                unsafe {
                    let h = event.u.CreateProcessInfo.hFile;
                    if !h.is_invalid() {
                        let _ = CloseHandle(h);
                    }
                }

                // For child processes, set up their own buffer/shellcode/hooks
                if !procs.contains_key(&event_pid) {
                    let child_buf = unsafe {
                        VirtualAllocEx(child_h, None, buf_size, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE)
                    };
                    if !child_buf.is_null() {
                        // Write sentinel
                        unsafe {
                            let _ = WriteProcessMemory(child_h, child_buf, &sentinel as *const u32 as *const _, 4, None);
                        }
                        let child_stub = allocate_shellcode(child_h, child_buf, &mut logs);
                        logs.push(format!("  Child pid={}: buffer={:p} stub={:p}", event_pid, child_buf, child_stub));
                        procs.insert(event_pid, ProcState {
                            h_process: child_h,
                            remote_buf: child_buf,
                            stub_mem: child_stub,
                            exe_base: base,
                            initial_bp_handled: false,
                            taskdlg_hooked: false,
                        });
                    } else {
                        logs.push(format!("  Child pid={}: VirtualAllocEx FAILED", event_pid));
                    }
                } else {
                    if let Some(ps) = procs.get_mut(&event_pid) {
                        ps.exe_base = base;
                    }
                }

                DBG_CONTINUE
            }
            LOAD_DLL_DEBUG_EVENT => {
                dll_count += 1;
                unsafe {
                    let h = event.u.LoadDll.hFile;
                    if !h.is_invalid() {
                        let _ = CloseHandle(h);
                    }
                }
                // Retry TaskDialogIndirect hook after each DLL load for this process
                if let Some(ps) = procs.get_mut(&event_pid) {
                    if ps.initial_bp_handled && !ps.taskdlg_hooked && !ps.stub_mem.is_null() {
                        if try_hook_taskdialog(ps.h_process, ps.remote_buf, &mut logs) {
                            ps.taskdlg_hooked = true;
                        }
                    }
                }
                DBG_CONTINUE
            }
            EXCEPTION_DEBUG_EVENT => {
                let code = unsafe { event.u.Exception.ExceptionRecord.ExceptionCode };
                let first_chance = unsafe { event.u.Exception.dwFirstChance };
                let addr = unsafe { event.u.Exception.ExceptionRecord.ExceptionAddress } as usize;

                if code.0 == 0x80000003u32 as i32 {
                    if let Some(ps) = procs.get_mut(&event_pid) {
                        if !ps.initial_bp_handled {
                            ps.initial_bp_handled = true;
                            logs.push(format!(
                                "[{}] Initial breakpoint pid={} ({} DLLs loaded)",
                                event_count, event_pid, dll_count
                            ));

                            // IAT scan (diagnostic only)
                            if ps.exe_base != 0 {
                                scan_iat_for_info(ps.h_process, ps.exe_base, &mut logs);
                            }

                            // Inline-hook all MessageBox variants
                            if !ps.stub_mem.is_null() {
                                inline_hook_probe(ps.h_process, ps.stub_mem, ps.remote_buf, &mut logs);
                            }
                        }
                    }
                    DBG_CONTINUE
                } else {
                    logs.push(format!(
                        "[{}] EXCEPTION pid={} code=0x{:08x} addr=0x{:x} 1st={}",
                        event_count, event_pid, code.0, addr, first_chance
                    ));
                    DBG_EXCEPTION_NOT_HANDLED
                }
            }
            EXIT_PROCESS_DEBUG_EVENT => {
                let exit_code = unsafe { event.u.ExitProcess.dwExitCode };
                logs.push(format!("[{}] EXIT_PROCESS pid={} (code={})", event_count, event_pid, exit_code));
                // Read buffer BEFORE process handle becomes invalid
                if let Some(ps) = procs.get(&event_pid) {
                    read_proc_sentinel(ps.h_process, ps.remote_buf, buf_size, event_pid, &mut logs, &mut captured_bufs);
                }
                procs.remove(&event_pid);
                if procs.is_empty() {
                    unsafe {
                        let _ = ContinueDebugEvent(event_pid, event.dwThreadId, DBG_CONTINUE);
                    }
                    _all_exited = true;
                    break;
                }
                DBG_CONTINUE
            }
            CREATE_THREAD_DEBUG_EVENT => DBG_CONTINUE,
            EXIT_THREAD_DEBUG_EVENT => DBG_CONTINUE,
            UNLOAD_DLL_DEBUG_EVENT => DBG_CONTINUE,
            OUTPUT_DEBUG_STRING_EVENT => DBG_CONTINUE,
            _ => DBG_CONTINUE,
        };

        unsafe {
            let _ = ContinueDebugEvent(event_pid, event.dwThreadId, continue_status);
        }
    }

    logs.push(format!(
        "Summary: {} events, {} DLLs",
        event_count, dll_count
    ));

    // Read buffers from any processes still alive (timeout case)
    for (&pid, ps) in procs.iter() {
        read_proc_sentinel(ps.h_process, ps.remote_buf, buf_size, pid, &mut logs, &mut captured_bufs);
    }

    // Extract text from first captured buffer that has content
    let mut text = String::new();
    for buf in &captured_bufs {
        if buf.len() >= 2 {
            let u16_slice: &[u16] =
                unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u16, buf.len() / 2) };
            let text_end = u16_slice
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(u16_slice.len());
            let candidate = String::from_utf16_lossy(&u16_slice[..text_end]);
            if !candidate.is_empty() {
                logs.push(format!("Captured {} chars", candidate.len()));
                logs.push(format!("Preview: {:?}", &candidate[..candidate.len().min(200)]));
                text = candidate;
                break;
            }
        }
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

    // Clean up debug session: terminate, drain events, close handles
    unsafe {
        let _ = TerminateProcess(h_process, 1);
        let drain_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if std::time::Instant::now() > drain_deadline { break; }
            let mut ev: DEBUG_EVENT = std::mem::zeroed();
            if WaitForDebugEvent(&mut ev, 500).is_err() { break; }
            let is_exit = ev.dwDebugEventCode == EXIT_PROCESS_DEBUG_EVENT;
            match ev.dwDebugEventCode {
                CREATE_PROCESS_DEBUG_EVENT => {
                    let info = ev.u.CreateProcessInfo;
                    if !info.hFile.is_invalid() { let _ = CloseHandle(info.hFile); }
                }
                LOAD_DLL_DEBUG_EVENT => {
                    let info = ev.u.LoadDll;
                    if !info.hFile.is_invalid() { let _ = CloseHandle(info.hFile); }
                }
                _ => {}
            }
            let _ = ContinueDebugEvent(ev.dwProcessId, ev.dwThreadId, DBG_CONTINUE);
            if is_exit { break; }
        }
        let _ = WaitForSingleObject(h_process, 1000);
        let _ = CloseHandle(h_process);
        let _ = CloseHandle(h_thread);
    }

    Ok(CaptureResult {
        raw_text: text,
        monitors,
        logs,
    })
}

/// Allocate shellcode in remote process that copies lpText (RDX) to buffer and returns IDOK(1).
#[cfg(target_os = "windows")]
fn allocate_shellcode(
    h_process: windows::Win32::Foundation::HANDLE,
    remote_buf: *mut std::ffi::c_void,
    logs: &mut Vec<String>,
) -> *mut std::ffi::c_void {
    use windows::Win32::System::Diagnostics::Debug::*;
    use windows::Win32::System::Memory::*;

    //   mov rax, <buf_addr>       ; 48 B8 <imm64>
    // copy_loop:
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
    shellcode.extend_from_slice(&[0x48, 0xB8]);
    shellcode.extend_from_slice(&buf_addr.to_le_bytes());
    shellcode.extend_from_slice(&[0x66, 0x8B, 0x0A]);
    shellcode.extend_from_slice(&[0x66, 0x89, 0x08]);
    shellcode.extend_from_slice(&[0x48, 0x83, 0xC2, 0x02]);
    shellcode.extend_from_slice(&[0x48, 0x83, 0xC0, 0x02]);
    shellcode.extend_from_slice(&[0x66, 0x85, 0xC9]);
    shellcode.extend_from_slice(&[0x75, 0xED]);
    shellcode.extend_from_slice(&[0xB8, 0x01, 0x00, 0x00, 0x00]);
    shellcode.push(0xC3);

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
        logs.push("Failed to allocate shellcode memory".into());
        return stub_mem;
    }

    if unsafe {
        WriteProcessMemory(
            h_process,
            stub_mem,
            shellcode.as_ptr() as *const _,
            shellcode.len(),
            None,
        )
    }
    .is_err()
    {
        logs.push("Failed to write shellcode".into());
        return std::ptr::null_mut();
    }

    logs.push(format!("Shellcode at {:p} ({} bytes)", stub_mem, shellcode.len()));
    stub_mem
}

/// Read bytes from remote process memory.
#[cfg(target_os = "windows")]
fn read_remote(
    h_process: windows::Win32::Foundation::HANDLE,
    addr: usize,
    buf: &mut [u8],
) -> bool {
    use windows::Win32::System::Diagnostics::Debug::*;
    let mut read = 0usize;
    unsafe {
        ReadProcessMemory(
            h_process,
            addr as *const _,
            buf.as_mut_ptr() as *mut _,
            buf.len(),
            Some(&mut read),
        )
        .is_ok()
            && read == buf.len()
    }
}

/// Diagnostic: scan mstsc.exe's IAT and log MessageBox-related imports (no hooking).
#[cfg(target_os = "windows")]
fn scan_iat_for_info(
    h_process: windows::Win32::Foundation::HANDLE,
    exe_base: usize,
    logs: &mut Vec<String>,
) {
    let mut dos = [0u8; 64];
    if !read_remote(h_process, exe_base, &mut dos) { return; }
    if dos[0] != b'M' || dos[1] != b'Z' { return; }

    let e_lfanew = u32::from_le_bytes([dos[0x3C], dos[0x3D], dos[0x3E], dos[0x3F]]) as usize;
    let mut pe_hdr = [0u8; 264];
    if !read_remote(h_process, exe_base + e_lfanew, &mut pe_hdr) { return; }
    if &pe_hdr[0..4] != b"PE\0\0" { return; }

    let dd1_off = 24 + 120;
    let import_rva = u32::from_le_bytes([
        pe_hdr[dd1_off], pe_hdr[dd1_off+1], pe_hdr[dd1_off+2], pe_hdr[dd1_off+3],
    ]) as usize;
    if import_rva == 0 { return; }

    let import_base = exe_base + import_rva;
    let mut desc_idx = 0u32;
    loop {
        let mut desc = [0u8; 20];
        if !read_remote(h_process, import_base + (desc_idx as usize) * 20, &mut desc) { break; }
        let ilt_rva = u32::from_le_bytes([desc[0], desc[1], desc[2], desc[3]]) as usize;
        let name_rva = u32::from_le_bytes([desc[12], desc[13], desc[14], desc[15]]) as usize;
        if ilt_rva == 0 && name_rva == 0 { break; }

        let mut name_buf = [0u8; 128];
        if !read_remote(h_process, exe_base + name_rva, &mut name_buf) { desc_idx += 1; continue; }
        let name_end = name_buf.iter().position(|&b| b == 0).unwrap_or(name_buf.len());
        let dll_name = String::from_utf8_lossy(&name_buf[..name_end]).to_string();

        // Only log DLLs that might have MessageBox
        let dll_lower = dll_name.to_lowercase();
        let interesting = dll_lower.contains("user32") || dll_lower.contains("api-ms") || dll_lower.contains("comctl");

        let ilt_base = exe_base + ilt_rva;
        let mut entry_idx = 0usize;
        let mut has_msgbox = false;
        loop {
            let mut ilt_entry = [0u8; 8];
            if !read_remote(h_process, ilt_base + entry_idx * 8, &mut ilt_entry) { break; }
            let ilt_val = u64::from_le_bytes(ilt_entry);
            if ilt_val == 0 { break; }
            if (ilt_val >> 63) != 0 { entry_idx += 1; continue; }

            let hint_rva = (ilt_val & 0x7FFFFFFF) as usize;
            let mut hint_buf = [0u8; 256];
            if !read_remote(h_process, exe_base + hint_rva, &mut hint_buf) { entry_idx += 1; continue; }
            let fn_end = hint_buf[2..].iter().position(|&b| b == 0).unwrap_or(hint_buf.len() - 2);
            let fn_name = std::str::from_utf8(&hint_buf[2..2 + fn_end]).unwrap_or("");

            if fn_name.contains("MessageBox") || fn_name.contains("TaskDialog") || fn_name.contains("DialogBox") {
                if !has_msgbox { logs.push(format!("IAT [{}]: {}", desc_idx, dll_name)); has_msgbox = true; }
                logs.push(format!("  -> {}", fn_name));
            }
            entry_idx += 1;
        }
        if interesting && !has_msgbox {
            logs.push(format!("IAT [{}]: {} (no MessageBox funcs)", desc_idx, dll_name));
        }
        desc_idx += 1;
    }
}

/// Inline-hook all MessageBox variants to probe which function actually gets called.
/// Each hook writes a unique marker (0x01..0x06) to buf[0] and returns 1.
#[cfg(target_os = "windows")]
fn inline_hook_probe(
    h_process: windows::Win32::Foundation::HANDLE,
    _text_stub: *mut std::ffi::c_void,
    remote_buf: *mut std::ffi::c_void,
    logs: &mut Vec<String>,
) {
    use windows::Win32::System::Diagnostics::Debug::*;
    use windows::Win32::System::LibraryLoader::*;
    use windows::Win32::System::Memory::*;
    use windows::core::s;

    let user32 = match unsafe { GetModuleHandleA(s!("user32.dll")) } {
        Ok(h) => h,
        Err(_) => { logs.push("PROBE: can't get user32".into()); return; }
    };
    let comctl32 = unsafe { GetModuleHandleA(s!("comctl32.dll")) }.ok();

    // (display_name, c_name with null terminator, dll_handle, marker_byte)
    let targets: Vec<(&str, &[u8], Option<windows::Win32::Foundation::HMODULE>, u8)> = vec![
        ("MessageBoxW", b"MessageBoxW\0", Some(user32), 0x01),
        ("MessageBoxExW", b"MessageBoxExW\0", Some(user32), 0x02),
        ("MessageBoxIndirectW", b"MessageBoxIndirectW\0", Some(user32), 0x03),
        ("MessageBoxA", b"MessageBoxA\0", Some(user32), 0x04),
        ("MessageBoxExA", b"MessageBoxExA\0", Some(user32), 0x05),
        ("TaskDialogIndirect", b"TaskDialogIndirect\0", comctl32, 0x06),
        ("DialogBoxParamW", b"DialogBoxParamW\0", Some(user32), 0x07),
    ];

    let buf_addr = remote_buf as u64;

    for (name, c_name, dll, marker) in &targets {
        let dll_h = match dll {
            Some(h) => *h,
            None => { logs.push(format!("PROBE: {} - DLL not loaded", name)); continue; }
        };

        let func = unsafe { GetProcAddress(dll_h, windows::core::PCSTR(c_name.as_ptr())) };
        let func_addr = match func {
            Some(f) => f as usize,
            None => { logs.push(format!("PROBE: {} - not found", name)); continue; }
        };

        // Build probe shellcode: write marker to buf, return 1
        //   mov rax, <buf_addr>       ; 48 B8 <imm64>
        //   mov byte [rax], <marker>  ; C6 00 <imm8>
        //   mov eax, 1                ; B8 01 00 00 00
        //   ret                       ; C3
        let mut probe: Vec<u8> = Vec::with_capacity(18);
        probe.extend_from_slice(&[0x48, 0xB8]);
        probe.extend_from_slice(&buf_addr.to_le_bytes());
        probe.push(0xC6); probe.push(0x00); probe.push(*marker);
        probe.extend_from_slice(&[0xB8, 0x01, 0x00, 0x00, 0x00]);
        probe.push(0xC3);

        // Alloc + write probe shellcode in target
        let probe_mem = unsafe {
            VirtualAllocEx(h_process, None, probe.len(), MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE)
        };
        if probe_mem.is_null() {
            logs.push(format!("PROBE: {} - alloc failed", name));
            continue;
        }
        if unsafe {
            WriteProcessMemory(h_process, probe_mem, probe.as_ptr() as *const _, probe.len(), None)
        }.is_err() {
            logs.push(format!("PROBE: {} - write failed", name));
            continue;
        }

        // Patch function entry: mov rax, <probe_mem>; jmp rax (12 bytes)
        let stub_addr = probe_mem as u64;
        let mut trampoline = [0u8; 12];
        trampoline[0] = 0x48; trampoline[1] = 0xB8;
        trampoline[2..10].copy_from_slice(&stub_addr.to_le_bytes());
        trampoline[10] = 0xFF; trampoline[11] = 0xE0;

        let mut old_prot = PAGE_PROTECTION_FLAGS(0);
        if unsafe {
            VirtualProtectEx(h_process, func_addr as *const _, 12, PAGE_EXECUTE_READWRITE, &mut old_prot)
        }.is_err() {
            logs.push(format!("PROBE: {} @ 0x{:x} - VirtualProtect failed", name, func_addr));
            continue;
        }

        let write_ok = unsafe {
            WriteProcessMemory(h_process, func_addr as *const _, trampoline.as_ptr() as *const _, 12, None)
        };
        unsafe {
            let _ = VirtualProtectEx(h_process, func_addr as *const _, 12, old_prot, &mut old_prot);
        }

        if write_ok.is_ok() {
            logs.push(format!("PROBE: {} @ 0x{:x} -> marker 0x{:02x} HOOKED", name, func_addr, marker));
        } else {
            logs.push(format!("PROBE: {} @ 0x{:x} - write failed", name, func_addr));
        }
    }
}

/// Try to hook TaskDialogIndirect in comctl32.dll (may not be loaded yet).
/// Returns true if successfully hooked.
#[cfg(target_os = "windows")]
fn try_hook_taskdialog(
    h_process: windows::Win32::Foundation::HANDLE,
    remote_buf: *mut std::ffi::c_void,
    logs: &mut Vec<String>,
) -> bool {
    use windows::Win32::System::Diagnostics::Debug::*;
    use windows::Win32::System::LibraryLoader::*;
    use windows::Win32::System::Memory::*;
    use windows::core::s;

    let comctl32 = match unsafe { GetModuleHandleA(s!("comctl32.dll")) } {
        Ok(h) => h,
        Err(_) => return false,
    };
    let func = unsafe { GetProcAddress(comctl32, windows::core::PCSTR(b"TaskDialogIndirect\0".as_ptr())) };
    let func_addr = match func {
        Some(f) => f as usize,
        None => return false,
    };

    // Try VirtualProtectEx - will fail if comctl32 not loaded in target
    let mut old_prot = PAGE_PROTECTION_FLAGS(0);
    if unsafe {
        VirtualProtectEx(h_process, func_addr as *const _, 12, PAGE_EXECUTE_READWRITE, &mut old_prot)
    }.is_err() {
        return false;
    }

    // Build probe: write marker 0x06 to buf, return S_OK (0)
    let buf_addr = remote_buf as u64;
    let mut probe: Vec<u8> = Vec::with_capacity(18);
    probe.extend_from_slice(&[0x48, 0xB8]);
    probe.extend_from_slice(&buf_addr.to_le_bytes());
    probe.push(0xC6); probe.push(0x00); probe.push(0x06); // mov byte [rax], 0x06
    probe.extend_from_slice(&[0x31, 0xC0]); // xor eax, eax (return S_OK=0)
    probe.push(0xC3); // ret

    let probe_mem = unsafe {
        VirtualAllocEx(h_process, None, probe.len(), MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE)
    };
    if probe_mem.is_null() {
        unsafe { let _ = VirtualProtectEx(h_process, func_addr as *const _, 12, old_prot, &mut old_prot); }
        return false;
    }
    if unsafe {
        WriteProcessMemory(h_process, probe_mem, probe.as_ptr() as *const _, probe.len(), None)
    }.is_err() {
        unsafe { let _ = VirtualProtectEx(h_process, func_addr as *const _, 12, old_prot, &mut old_prot); }
        return false;
    }

    let stub_addr = probe_mem as u64;
    let mut trampoline = [0u8; 12];
    trampoline[0] = 0x48; trampoline[1] = 0xB8;
    trampoline[2..10].copy_from_slice(&stub_addr.to_le_bytes());
    trampoline[10] = 0xFF; trampoline[11] = 0xE0;

    let ok = unsafe {
        WriteProcessMemory(h_process, func_addr as *const _, trampoline.as_ptr() as *const _, 12, None)
    }.is_ok();
    unsafe { let _ = VirtualProtectEx(h_process, func_addr as *const _, 12, old_prot, &mut old_prot); }

    if ok {
        logs.push(format!("PROBE: TaskDialogIndirect @ 0x{:x} -> marker 0x06 HOOKED (late)", func_addr));
    }
    ok
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

fn parse_mstsc_output(text: &str) -> Result<Vec<LiveMonitor>, String> {
    let monitors = parse_mstsc_lines(text);
    if monitors.is_empty() {
        return Err("No monitor lines found in mstsc output".into());
    }
    Ok(monitors)
}

/// Parse mstsc /l output. Handles real format:
///   "0: 1920 x 1200; (3840, 241, 5759, 1440)"
/// Coordinates are inclusive bounds → width = right - left + 1.
fn parse_mstsc_lines(text: &str) -> Vec<LiveMonitor> {
    let mut monitors = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Split "ID: rest" by first ':'
        let (id_str, rest) = match line.split_once(':') {
            Some(pair) => pair,
            None => continue,
        };
        let id: u32 = match id_str.trim().parse() {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Find coordinates in parentheses: (left, top, right, bottom)
        let coords_start = match rest.rfind('(') {
            Some(i) => i,
            None => continue,
        };
        let coords = extract_numbers(&rest[coords_start..]);
        if coords.len() < 4 {
            continue;
        }
        // Inclusive bounds → +1 for width/height
        let width = (coords[2] - coords[0] + 1) as u32;
        let height = (coords[3] - coords[1] + 1) as u32;

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
        // Real mstsc /l format: "ID: WxH; (left, top, right, bottom)"
        // Coordinates are inclusive bounds → width = right - left + 1
        let text = "0: 1920 x 1080; (-1921, 0, -2, 1079)\n\
                     1: 2560 x 1440; (0, 0, 2559, 1439)  [PRIMARY]\n\
                     2: 1920 x 1080; (2560, 0, 4479, 1079)";
        let monitors = parse_mstsc_output(text).unwrap();
        assert_eq!(monitors.len(), 3);
        assert_eq!(monitors[0].mstsc_id, 0);
        assert_eq!(monitors[0].left, -1921);
        assert_eq!(monitors[0].width, 1920);
        assert_eq!(monitors[0].height, 1080);
        assert!(monitors[1].is_primary);
        assert_eq!(monitors[1].width, 2560);
        assert_eq!(monitors[2].left, 2560);
        assert_eq!(monitors[2].width, 1920);
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
