#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
include!("windows_main.rs");

#[cfg(not(windows))]
fn main() {
    let version = env!("CARGO_PKG_VERSION");
    println!("CodexRouter v{version}");
    println!("The current supported runtime is Windows 10/11 x64.");
    std::process::exit(2);
}
