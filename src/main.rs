#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    rdp_launcher_lib::run();
}
