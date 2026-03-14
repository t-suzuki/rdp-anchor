#![windows_subsystem = "windows"]
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--indirect") {
        // Spawn ourselves without --indirect, then wait for it to finish
        let exe = std::env::current_exe().expect("current_exe");
        let status = std::process::Command::new(exe)
            .spawn()
            .expect("spawn self")
            .wait()
            .expect("wait");
        std::process::exit(status.code().unwrap_or(1));
    }

    unsafe {
        MessageBoxW(
            None,
            w!("Hello from Rust hook test"),
            w!("Hook Test"),
            MB_OK,
        );
    }
}
