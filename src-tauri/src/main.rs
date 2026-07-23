#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Force flush stderr so startup logs are immediately visible
    eprintln!("=== AlphaKey starting ===");
    redkey_lib::run();
    eprintln!("=== AlphaKey exiting ===");
}
