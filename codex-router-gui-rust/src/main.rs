#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
include!("windows_main.rs");

#[cfg(not(windows))]
fn main() {
    let version = env!("CARGO_PKG_VERSION");
    println!("CodexRouter v{version}");
    println!();
    println!(
        "This is a theoretical {os} build.",
        os = std::env::consts::OS
    );
    println!(
        "It has not been tested on a real {os} machine.",
        os = std::env::consts::OS
    );
    println!("The current supported runtime is Windows 10/11 x64.");
    println!("Repository: https://github.com/HernanJiang/CodexRouter");
    println!("Contributions that help build and verify macOS/Linux versions are welcome.");
    std::process::exit(2);
}
